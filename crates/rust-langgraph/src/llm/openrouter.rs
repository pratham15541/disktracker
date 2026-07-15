//! OpenRouter adapter — unified API for many models via OpenAI-compatible endpoints.
//!
//! OpenRouter exposes an OpenAI-compatible Chat Completions API at
//! `https://openrouter.ai/api/v1`. Use `Authorization: Bearer <OPENROUTER_API_KEY>` and
//! model IDs such as `openai/gpt-4o` or `anthropic/claude-3.5-sonnet`.
//!
//! See [OpenRouter quickstart](https://openrouter.ai/docs/quickstart).
//!
//! Optional headers (`HTTP-Referer`, `X-OpenRouter-Title`) for app attribution are not
//! set by this adapter yet; the API works without them.

use crate::errors::{Error, Result};
use crate::llm::{ChatModel, ToolInfo};
use crate::state::{Message, ToolCall};
use async_openai::{
    config::OpenAIConfig,
    types::{
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessage, ChatCompletionRequestToolMessage,
        ChatCompletionRequestUserMessage,
        ChatCompletionTool, ChatCompletionToolType, CreateChatCompletionRequestArgs,
        FunctionObject,
    },
    Client,
};
use async_trait::async_trait;

/// Default OpenRouter API base (OpenAI-compatible).
pub const OPENROUTER_API_BASE: &str = "https://openrouter.ai/api/v1";

/// OpenRouter adapter implementing [`ChatModel`].
///
/// # Example
///
/// ```rust,no_run
/// use rust_langgraph::llm::openrouter::OpenRouterAdapter;
/// use rust_langgraph::llm::ChatModel;
/// use rust_langgraph::state::Message;
///
/// #[tokio::main]
/// async fn main() {
///     let adapter = OpenRouterAdapter::with_api_key(
///         "openai/gpt-4o-mini",
///         std::env::var("OPENROUTER_API_KEY").unwrap(),
///     );
///     let messages = vec![Message::user("Hello!")];
///     let response = adapter.invoke(&messages).await.unwrap();
///     println!("{}", response.content);
/// }
/// ```
#[derive(Clone)]
pub struct OpenRouterAdapter {
    client: Client<OpenAIConfig>,
    model: String,
    temperature: Option<f32>,
    bound_tools: Vec<ToolInfo>,
}

impl OpenRouterAdapter {
    /// Build config: OpenRouter base URL + API key from `OPENROUTER_API_KEY` (may be empty).
    fn config_from_env() -> OpenAIConfig {
        OpenAIConfig::new()
            .with_api_key(
                std::env::var("OPENROUTER_API_KEY").unwrap_or_else(|_| String::new()),
            )
            .with_api_base(OPENROUTER_API_BASE)
    }

    /// New adapter using `OPENROUTER_API_KEY` from the environment and the given model id
    /// (e.g. `openai/gpt-4o-mini`).
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            client: Client::with_config(Self::config_from_env()),
            model: model.into(),
            temperature: None,
            bound_tools: Vec::new(),
        }
    }

    /// Explicit API key (recommended for clarity).
    pub fn with_api_key(model: impl Into<String>, api_key: impl Into<String>) -> Self {
        let config = OpenAIConfig::new()
            .with_api_key(api_key)
            .with_api_base(OPENROUTER_API_BASE);

        Self {
            client: Client::with_config(config),
            model: model.into(),
            temperature: None,
            bound_tools: Vec::new(),
        }
    }

    /// Custom API base (proxy or future OpenRouter URL change). Most users should use defaults.
    pub fn with_api_base(
        model: impl Into<String>,
        api_key: impl Into<String>,
        api_base: impl Into<String>,
    ) -> Self {
        let config = OpenAIConfig::new()
            .with_api_key(api_key)
            .with_api_base(api_base);

        Self {
            client: Client::with_config(config),
            model: model.into(),
            temperature: None,
            bound_tools: Vec::new(),
        }
    }

    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    pub fn bind_tools(mut self, tools: Vec<ToolInfo>) -> Self {
        self.bound_tools = tools;
        self
    }

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
            _ => ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: msg.content.clone().into(),
                name: msg.name.clone(),
                ..Default::default()
            }),
        }
    }

    fn from_openai_response(
        response: async_openai::types::CreateChatCompletionResponse,
    ) -> Result<Message> {
        let choice = response
            .choices
            .first()
            .ok_or_else(|| Error::execution("No response from OpenRouter"))?;

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
impl ChatModel for OpenRouterAdapter {
    async fn invoke(&self, messages: &[Message]) -> Result<Message> {
        let openai_messages: Vec<_> = messages.iter().map(Self::to_openai_message).collect();

        let mut request = CreateChatCompletionRequestArgs::default();
        request.model(&self.model);
        request.messages(openai_messages);

        if let Some(temp) = self.temperature {
            request.temperature(temp);
        }

        if !self.bound_tools.is_empty() {
            let tools: Vec<_> = self.bound_tools.iter().map(Self::to_openai_tool).collect();
            request.tools(tools);
        }

        let request = request
            .build()
            .map_err(|e| Error::execution(format!("Failed to build OpenRouter request: {}", e)))?;

        let response = self
            .client
            .chat()
            .create(request)
            .await
            .map_err(|e| Error::execution(format!("OpenRouter API error: {}", e)))?;

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
    fn test_default_base_constant() {
        assert!(OPENROUTER_API_BASE.contains("openrouter.ai"));
    }

    #[test]
    fn test_with_api_key() {
        let a = OpenRouterAdapter::with_api_key("openai/gpt-4o-mini", "sk-or-test");
        assert_eq!(a.name(), "openai/gpt-4o-mini");
    }

    #[test]
    fn test_user_message_roundtrip_shape() {
        let msg = Message::user("Hi");
        let m = OpenRouterAdapter::to_openai_message(&msg);
        match m {
            ChatCompletionRequestMessage::User(u) => match u.content {
                ChatCompletionRequestUserMessageContent::Text(s) => assert_eq!(s, "Hi"),
                ChatCompletionRequestUserMessageContent::Array(_) => {
                    panic!("expected text user content")
                }
            },
            _ => panic!("expected user"),
        }
    }
}
