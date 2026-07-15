//! Anthropic Claude adapter using HTTP client.
//!
//! Provides integration with Anthropic's Claude API including
//! support for tool use.

use crate::errors::{Error, Result};
use crate::llm::{ChatModel, ToolInfo};
use crate::state::{Message, ToolCall};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Anthropic Claude adapter implementing ChatModel trait.
///
/// Supports Claude models including Claude 3 Opus, Sonnet, and Haiku.
///
/// # Example
///
/// ```rust,no_run
/// use rust_langgraph::llm::anthropic::AnthropicAdapter;
/// use rust_langgraph::llm::ChatModel;
/// use rust_langgraph::state::Message;
///
/// #[tokio::main]
/// async fn main() {
///     let adapter = AnthropicAdapter::with_api_key("sk-ant-...");
///     let messages = vec![Message::user("Hello!")];
///     let response = adapter.invoke(&messages).await.unwrap();
///     println!("{}", response.content);
/// }
/// ```
#[derive(Clone)]
pub struct AnthropicAdapter {
    client: Client,
    api_key: String,
    model: String,
    temperature: Option<f32>,
    max_tokens: u32,
    tools: Vec<ToolInfo>,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
    #[allow(dead_code)]
    model: String,
    #[allow(dead_code)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContent {
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
}

impl AnthropicAdapter {
    /// Create a new Anthropic adapter with API key.
    ///
    /// # Arguments
    ///
    /// * `api_key` - Your Anthropic API key (starts with sk-ant-)
    pub fn with_api_key(api_key: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            model: "claude-3-5-sonnet-20241022".to_string(),
            temperature: None,
            max_tokens: 4096,
            tools: Vec::new(),
        }
    }

    /// Set the model to use.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Set temperature for generation (0.0 - 1.0).
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Set max tokens for response.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Bind tools to this model for function calling.
    pub fn with_tools(mut self, tools: Vec<ToolInfo>) -> Self {
        self.tools = tools;
        self
    }

    /// Convert our Message to Anthropic format.
    fn message_to_json(msg: &Message) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("role".into(), json!(msg.role.clone()));

        // Anthropic uses "content" array for complex messages
        if msg.role == "tool" {
            // Tool result message
            m.insert(
                "content".into(),
                json!([{
                    "type": "tool_result",
                    "tool_use_id": msg.tool_call_id.clone().unwrap_or_default(),
                    "content": msg.content.clone()
                }]),
            );
        } else if msg.role == "assistant" && msg.tool_calls.is_some() {
            // Assistant message with tool calls
            let mut content_arr = vec![];

            // Add text content if any
            if !msg.content.is_empty() {
                content_arr.push(json!({
                    "type": "text",
                    "text": msg.content.clone()
                }));
            }

            // Add tool uses
            if let Some(ref tool_calls) = msg.tool_calls {
                for tc in tool_calls {
                    content_arr.push(json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": tc.arguments
                    }));
                }
            }

            m.insert("content".into(), json!(content_arr));
        } else {
            // Simple text message
            m.insert("content".into(), json!(msg.content.clone()));
        }

        Value::Object(m)
    }

    /// Convert tools to Anthropic format.
    fn tools_to_json(tools: &[ToolInfo]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters
                })
            })
            .collect()
    }

    /// Parse Anthropic response.
    fn parse_response(response: AnthropicResponse) -> Result<Message> {
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for content in response.content {
            match content {
                AnthropicContent::Text { text } => {
                    text_parts.push(text);
                }
                AnthropicContent::ToolUse { id, name, input } => {
                    tool_calls.push(ToolCall::new(id, name, input));
                }
            }
        }

        let content = text_parts.join("\n");

        if tool_calls.is_empty() {
            Ok(Message::assistant(content))
        } else {
            Ok(Message::assistant(content).with_tool_calls(tool_calls))
        }
    }
}

#[async_trait]
impl ChatModel for AnthropicAdapter {
    async fn invoke(&self, messages: &[Message]) -> Result<Message> {
        // Separate system messages
        let system_message = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.clone());

        let msgs: Vec<Value> = messages
            .iter()
            .filter(|m| m.role != "system")
            .map(Self::message_to_json)
            .collect();

        let mut body = serde_json::Map::new();
        body.insert("model".into(), json!(self.model.clone()));
        body.insert("max_tokens".into(), json!(self.max_tokens));
        body.insert("messages".into(), json!(msgs));

        if let Some(ref system) = system_message {
            body.insert("system".into(), json!(system));
        }

        if let Some(temp) = self.temperature {
            body.insert("temperature".into(), json!(temp));
        }

        // Add tools if present
        if !self.tools.is_empty() {
            body.insert("tools".into(), json!(Self::tools_to_json(&self.tools)));
        }

        let resp = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&Value::Object(body))
            .send()
            .await
            .map_err(|e| Error::execution(format!("Anthropic request failed: {}", e)))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| Error::execution(format!("Failed to read Anthropic response: {}", e)))?;

        let v: Value = serde_json::from_str(&text).map_err(|e| {
            Error::execution(format!(
                "Anthropic JSON parse error: {}; HTTP {}; body: {}",
                e, status, text
            ))
        })?;

        let response: AnthropicResponse = serde_json::from_value(v.clone()).map_err(|e| {
            Error::execution(format!("Failed to parse Anthropic response: {}; body: {}", e, v))
        })?;

        Self::parse_response(response)
    }

    fn name(&self) -> &str {
        &self.model
    }

    fn clone_box(&self) -> Box<dyn ChatModel> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_creation() {
        let adapter = AnthropicAdapter::with_api_key("sk-ant-test123");
        assert_eq!(adapter.model, "claude-3-5-sonnet-20241022");
        assert_eq!(adapter.max_tokens, 4096);
    }

    #[test]
    fn test_with_model() {
        let adapter = AnthropicAdapter::with_api_key("sk-ant-test")
            .with_model("claude-3-opus-20240229");
        assert_eq!(adapter.model, "claude-3-opus-20240229");
    }

    #[test]
    fn test_message_to_json_user() {
        let msg = Message::user("Hello");
        let json = AnthropicAdapter::message_to_json(&msg);

        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "Hello");
    }

    #[test]
    fn test_message_to_json_with_tool_calls() {
        let msg = Message::assistant("Let me search").with_tool_calls(vec![ToolCall::new(
            "call-1",
            "search",
            json!({"query": "rust"}),
        )]);

        let json = AnthropicAdapter::message_to_json(&msg);

        assert_eq!(json["role"], "assistant");
        let content = json["content"].as_array().unwrap();
        assert_eq!(content.len(), 2); // Text + tool_use
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["name"], "search");
    }

    #[test]
    fn test_tool_message_format() {
        let msg = Message::tool("Result: found!", "call-1");
        let json = AnthropicAdapter::message_to_json(&msg);

        assert_eq!(json["role"], "tool");
        let content = json["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "call-1");
    }

    #[test]
    fn test_tools_to_json() {
        let tools = vec![ToolInfo::new(
            "search",
            "Search the web",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"}
                }
            }),
        )];

        let json_tools = AnthropicAdapter::tools_to_json(&tools);
        assert_eq!(json_tools.len(), 1);
        assert_eq!(json_tools[0]["name"], "search");
        assert_eq!(json_tools[0]["description"], "Search the web");
        assert!(json_tools[0]["input_schema"].is_object());
    }
}
