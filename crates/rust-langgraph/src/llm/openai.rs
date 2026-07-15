//! OpenAI adapter using the async-openai crate.
//!
//! Provides integration with OpenAI's Chat Completion API including
//! support for function calling and tool use.

use crate::errors::{Error, Result};
use crate::llm::{ChatModel, ToolInfo};
use crate::state::{Message, ToolCall};
use async_openai::{
    config::OpenAIConfig,
    types::{
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessage, ChatCompletionRequestToolMessage,
        ChatCompletionRequestUserMessage, ChatCompletionTool, ChatCompletionToolType,
        CreateChatCompletionRequestArgs, FunctionObject,
    },
    Client,
};
use async_trait::async_trait;

/// OpenAI adapter implementing ChatModel trait.
///
/// Supports all OpenAI chat models including GPT-4, GPT-3.5, etc.
///
/// # Example
///
/// ```rust,no_run
/// use rust_langgraph::llm::openai::OpenAIAdapter;
/// use rust_langgraph::llm::ChatModel;
/// use rust_langgraph::state::Message;
///
/// #[tokio::main]
/// async fn main() {
///     let adapter = OpenAIAdapter::with_api_key("gpt-4", "sk-...");
///     let messages = vec![Message::user("Hello!")];
///     let response = adapter.invoke(&messages).await.unwrap();
///     println!("{}", response.content);
/// }
/// ```
#[derive(Clone)]
pub struct OpenAIAdapter {
    client: Client<OpenAIConfig>,
    model: String,
    temperature: Option<f32>,
    bound_tools: Vec<ToolInfo>,
}

impl OpenAIAdapter {
    /// Create a new OpenAI adapter using the default API key from environment.
    ///
    /// Reads API key from `OPENAI_API_KEY` environment variable.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            model: model.into(),
            temperature: None,
            bound_tools: Vec::new(),
        }
    }

    /// Create adapter with custom API key.
    pub fn with_api_key(model: impl Into<String>, api_key: impl Into<String>) -> Self {
        let config = OpenAIConfig::new().with_api_key(api_key);

        Self {
            client: Client::with_config(config),
            model: model.into(),
            temperature: None,
            bound_tools: Vec::new(),
        }
    }

    /// Set temperature for generation (0.0 - 2.0).
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Bind tools to this model for function calling.
    pub fn bind_tools(mut self, tools: Vec<ToolInfo>) -> Self {
        self.bound_tools = tools;
        self
    }

    /// Convert our Message to OpenAI message format.
    fn to_openai_message(msg: &Message) -> ChatCompletionRequestMessage {
        use async_openai::types::{
            ChatCompletionRequestAssistantMessageContent, ChatCompletionRequestSystemMessageContent,
            ChatCompletionRequestToolMessageContent,
        };

        match msg.role.as_str() {
            "system" => ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(msg.content.clone()),
                name: msg.name.clone(),
                ..Default::default()
            }),
            "user" => ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: msg.content.clone().into(),
                name: msg.name.clone(),
                ..Default::default()
            }),
            "assistant" => {
                let tool_calls = if let Some(ref calls) = msg.tool_calls {
                    if !calls.is_empty() {
                        Some(
                            calls
                                .iter()
                                .map(|tc| async_openai::types::ChatCompletionMessageToolCall {
                                    id: tc.id.clone(),
                                    r#type: ChatCompletionToolType::Function,
                                    function: async_openai::types::FunctionCall {
                                        name: tc.name.clone(),
                                        arguments: tc.arguments.to_string(),
                                    },
                                })
                                .collect(),
                        )
                    } else {
                        None
                    }
                } else {
                    None
                };

                ChatCompletionRequestMessage::Assistant(ChatCompletionRequestAssistantMessage {
                    content: Some(ChatCompletionRequestAssistantMessageContent::Text(
                        msg.content.clone(),
                    )),
                    name: msg.name.clone(),
                    tool_calls,
                    ..Default::default()
                })
            }
            "tool" => ChatCompletionRequestMessage::Tool(ChatCompletionRequestToolMessage {
                content: ChatCompletionRequestToolMessageContent::Text(msg.content.clone()),
                tool_call_id: msg.tool_call_id.clone().unwrap_or_default(),
                ..Default::default()
            }),
            _ => {
                // Default to user message for unknown roles
                ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                    content: msg.content.clone().into(),
                    name: msg.name.clone(),
                    ..Default::default()
                })
            }
        }
    }

    /// Convert OpenAI response to our Message format.
    fn from_openai_response(
        response: async_openai::types::CreateChatCompletionResponse,
    ) -> Result<Message> {
        let choice = response
            .choices
            .first()
            .ok_or_else(|| Error::execution("No response from OpenAI"))?;

        let content = choice.message.content.clone().unwrap_or_default();
        let tool_calls: Vec<ToolCall> = choice
            .message
            .tool_calls
            .as_ref()
            .map(|calls| {
                calls
                    .iter()
                    .map(|tc| {
                        let args = serde_json::from_str(&tc.function.arguments)
                            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                        ToolCall::new(tc.id.clone(), tc.function.name.clone(), args)
                    })
                    .collect()
            })
            .unwrap_or_default();

        if tool_calls.is_empty() {
            Ok(Message::assistant(content))
        } else {
            Ok(Message::assistant(content).with_tool_calls(tool_calls))
        }
    }

    /// Convert ToolInfo to OpenAI tool format.
    fn to_openai_tool(tool: &ToolInfo) -> ChatCompletionTool {
        ChatCompletionTool {
            r#type: ChatCompletionToolType::Function,
            function: FunctionObject {
                name: tool.name.clone(),
                description: Some(tool.description.clone()),
                parameters: Some(tool.parameters.clone()),
                ..Default::default()
            },
        }
    }
}

#[async_trait]
impl ChatModel for OpenAIAdapter {
    async fn invoke(&self, messages: &[Message]) -> Result<Message> {
        let openai_messages: Vec<_> = messages.iter().map(Self::to_openai_message).collect();

        let mut request = CreateChatCompletionRequestArgs::default();
        request.model(&self.model);
        request.messages(openai_messages);

        if let Some(temp) = self.temperature {
            request.temperature(temp);
        }

        // Add tools if bound
        if !self.bound_tools.is_empty() {
            let tools: Vec<_> = self.bound_tools.iter().map(Self::to_openai_tool).collect();
            request.tools(tools);
        }

        let request = request
            .build()
            .map_err(|e| Error::execution(format!("Failed to build OpenAI request: {}", e)))?;

        let response = self
            .client
            .chat()
            .create(request)
            .await
            .map_err(|e| Error::execution(format!("OpenAI API error: {}", e)))?;

        Self::from_openai_response(response)
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
        let adapter = OpenAIAdapter::new("gpt-4");
        assert_eq!(adapter.model, "gpt-4");
        assert_eq!(adapter.name(), "gpt-4");
    }

    #[test]
    fn test_with_temperature() {
        let adapter = OpenAIAdapter::new("gpt-4").with_temperature(0.5);
        assert_eq!(adapter.temperature, Some(0.5));
    }

    #[test]
    fn test_message_conversion_user() {
        let msg = Message::user("Hello");
        let openai_msg = OpenAIAdapter::to_openai_message(&msg);

        match openai_msg {
            ChatCompletionRequestMessage::User(user_msg) => match user_msg.content {
                async_openai::types::ChatCompletionRequestUserMessageContent::Text(s) => {
                    assert_eq!(s, "Hello");
                }
                async_openai::types::ChatCompletionRequestUserMessageContent::Array(_) => {
                    panic!("Expected text user content");
                }
            },
            _ => panic!("Expected user message"),
        }
    }

    #[test]
    fn test_message_conversion_assistant_with_tools() {
        let msg = Message::assistant("Let me search").with_tool_calls(vec![ToolCall::new(
            "call-1",
            "search",
            serde_json::json!({"query": "rust"}),
        )]);

        let openai_msg = OpenAIAdapter::to_openai_message(&msg);

        match openai_msg {
            ChatCompletionRequestMessage::Assistant(asst_msg) => {
                assert!(asst_msg.tool_calls.is_some());
                let calls = asst_msg.tool_calls.unwrap();
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].function.name, "search");
            }
            _ => panic!("Expected assistant message"),
        }
    }

    #[test]
    fn test_tool_message_conversion() {
        let msg = Message::tool("Result: found!", "call-1");
        let openai_msg = OpenAIAdapter::to_openai_message(&msg);

        match openai_msg {
            ChatCompletionRequestMessage::Tool(tool_msg) => {
                assert_eq!(tool_msg.content, "Result: found!");
                assert_eq!(tool_msg.tool_call_id, "call-1");
            }
            _ => panic!("Expected tool message"),
        }
    }

    #[test]
    fn test_tool_info_conversion() {
        let tool_info = ToolInfo::new(
            "search",
            "Search the web",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"}
                }
            }),
        );

        let openai_tool = OpenAIAdapter::to_openai_tool(&tool_info);

        assert_eq!(openai_tool.function.name, "search");
        assert_eq!(openai_tool.function.description, Some("Search the web".to_string()));
    }
}
