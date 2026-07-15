//! Ollama adapter for local LLM models with tool calling support.
//!
//! This adapter communicates with a local Ollama instance via HTTP,
//! supporting both simple chat and function calling.

use crate::errors::{Error, Result};
use crate::llm::{ChatModel, MessageChunk, ToolInfo};
use crate::state::{Message, ToolCall};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

/// Ollama API response structure
#[derive(Debug, Deserialize)]
struct OllamaResponse {
    message: OllamaMessage,
    #[allow(dead_code)]
    model: String,
    #[allow(dead_code)]
    done: bool,
}

#[derive(Debug, Deserialize)]
struct OllamaMessage {
    role: String,
    content: String,
    #[serde(default)]
    tool_calls: Option<Vec<Value>>,
}

/// Ollama adapter implementing ChatModel trait.
///
/// Connects to a local Ollama instance to run LLMs with optional
/// function calling support.
///
/// # Example
///
/// ```rust,no_run
/// use rust_langgraph::llm::ollama::OllamaAdapter;
/// use rust_langgraph::llm::ChatModel;
/// use rust_langgraph::state::Message;
///
/// #[tokio::main]
/// async fn main() {
///     let adapter = OllamaAdapter::new("llama3.1:8b");
///     let messages = vec![Message::user("Hello!")];
///     let response = adapter.invoke(&messages).await.unwrap();
///     println!("{}", response.content);
/// }
/// ```
#[derive(Clone)]
pub struct OllamaAdapter {
    client: Client,
    base_url: String,
    model: String,
    tools: Vec<ToolInfo>,
}

impl OllamaAdapter {
    /// Create a new Ollama adapter with default URL (http://localhost:11434)
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            base_url: "http://localhost:11434".to_string(),
            model: model.into(),
            tools: Vec::new(),
        }
    }

    /// Create adapter with custom base URL
    pub fn with_base_url(model: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.into(),
            model: model.into(),
            tools: Vec::new(),
        }
    }

    /// Bind tools to this model for function calling.
    ///
    /// The tools will be sent to Ollama in OpenAI-compatible format.
    pub fn with_tools(mut self, tools: Vec<ToolInfo>) -> Self {
        self.tools = tools;
        self
    }

    /// Convert a Message to Ollama JSON format
    fn message_to_json(msg: &Message) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("role".into(), json!(msg.role.clone()));
        m.insert("content".into(), json!(msg.content.clone()));

        // Handle tool response messages
        if msg.role == "tool" {
            if let Some(ref id) = msg.tool_call_id {
                m.insert("tool_call_id".into(), json!(id));
            }
        }

        // Handle assistant messages with tool calls
        if msg.role == "assistant" {
            if let Some(ref tool_calls) = msg.tool_calls {
                if !tool_calls.is_empty() {
                    let calls: Vec<Value> = tool_calls
                        .iter()
                        .map(|tc| {
                            json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": tc.arguments
                                }
                            })
                        })
                        .collect();
                    m.insert("tool_calls".into(), json!(calls));
                }
            }
        }

        Value::Object(m)
    }

    /// Convert tools to Ollama/OpenAI JSON format
    fn tools_to_json(tools: &[ToolInfo]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters
                    }
                })
            })
            .collect()
    }

    /// Parse tool calls from Ollama response
    fn parse_tool_calls(raw: Option<&Value>) -> Vec<ToolCall> {
        let Some(Value::Array(arr)) = raw else {
            return Vec::new();
        };

        let mut out = Vec::new();
        for tc in arr {
            let id = tc
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let func = tc.get("function").cloned().unwrap_or(json!({}));
            let name = func
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args_val = func.get("arguments").cloned().unwrap_or(json!({}));
            
            // Arguments might be a string (JSON) or already an object
            let args = match args_val {
                Value::String(s) => serde_json::from_str(&s).unwrap_or(json!({})),
                other => other,
            };
            
            if name.is_empty() {
                continue;
            }
            
            out.push(ToolCall::new(id, name, args));
        }
        out
    }

    /// Parse Ollama response message to our Message type
    fn parse_message(msg: &OllamaMessage) -> Result<Message> {
        let content = msg.content.clone();
        
        // Convert Vec<Value> to a single Value::Array for parse_tool_calls
        let tool_calls = if let Some(ref calls_vec) = msg.tool_calls {
            let array_value = serde_json::Value::Array(calls_vec.clone());
            Self::parse_tool_calls(Some(&array_value))
        } else {
            Vec::new()
        };
        
        if tool_calls.is_empty() {
            Ok(Message::assistant(content))
        } else {
            Ok(Message::assistant(content).with_tool_calls(tool_calls))
        }
    }
}

#[async_trait]
impl ChatModel for OllamaAdapter {
    async fn invoke(&self, messages: &[Message]) -> Result<Message> {
        let msgs: Vec<Value> = messages.iter().map(Self::message_to_json).collect();

        let mut body = serde_json::Map::new();
        body.insert("model".into(), json!(self.model.clone()));
        body.insert("stream".into(), false.into());
        body.insert("messages".into(), json!(msgs));
        
        // Add tools if present
        if !self.tools.is_empty() {
            body.insert("tools".into(), json!(Self::tools_to_json(&self.tools)));
        }

        let resp = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&Value::Object(body))
            .send()
            .await
            .map_err(|e| Error::execution(format!("Ollama request failed: {}", e)))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| Error::execution(format!("Failed to read Ollama response: {}", e)))?;

        let v: Value = serde_json::from_str(&text).map_err(|e| {
            Error::execution(format!(
                "Ollama JSON parse error: {}; HTTP {}; body: {}",
                e, status, text
            ))
        })?;

        let response: OllamaResponse = serde_json::from_value(v.clone()).map_err(|e| {
            Error::execution(format!("Failed to parse Ollama response: {}; body: {}", e, v))
        })?;

        Self::parse_message(&response.message)
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
        let adapter = OllamaAdapter::new("llama2");
        assert_eq!(adapter.model, "llama2");
        assert_eq!(adapter.base_url, "http://localhost:11434");
    }

    #[test]
    fn test_with_base_url() {
        let adapter = OllamaAdapter::with_base_url("llama3.1:8b", "http://127.0.0.1:11434");
        assert_eq!(adapter.base_url, "http://127.0.0.1:11434");
    }

    #[test]
    fn test_message_to_json_user() {
        let msg = Message::user("Hello");
        let json = OllamaAdapter::message_to_json(&msg);
        
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "Hello");
    }

    #[test]
    fn test_message_to_json_with_tool_calls() {
        let msg = Message::assistant("Let me search").with_tool_calls(vec![
            ToolCall::new("call-1", "search", json!({"query": "rust"})),
        ]);
        
        let json = OllamaAdapter::message_to_json(&msg);
        
        assert_eq!(json["role"], "assistant");
        assert!(json["tool_calls"].is_array());
        
        let calls = json["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["function"]["name"], "search");
    }

    #[test]
    fn test_tool_message_format() {
        let msg = Message::tool("Result: found!", "call-1");
        let json = OllamaAdapter::message_to_json(&msg);
        
        assert_eq!(json["role"], "tool");
        assert_eq!(json["content"], "Result: found!");
        assert_eq!(json["tool_call_id"], "call-1");
    }

    #[test]
    fn test_tools_to_json() {
        let tools = vec![
            ToolInfo::new(
                "search",
                "Search the web",
                json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"}
                    }
                }),
            ),
        ];

        let json_tools = OllamaAdapter::tools_to_json(&tools);
        assert_eq!(json_tools.len(), 1);
        assert_eq!(json_tools[0]["type"], "function");
        assert_eq!(json_tools[0]["function"]["name"], "search");
    }

    #[test]
    fn test_parse_tool_calls() {
        let tool_calls_json = json!([
            {
                "id": "call-123",
                "type": "function",
                "function": {
                    "name": "search",
                    "arguments": {"query": "rust"}
                }
            }
        ]);

        let parsed = OllamaAdapter::parse_tool_calls(Some(&tool_calls_json));
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "call-123");
        assert_eq!(parsed[0].name, "search");
    }
}
