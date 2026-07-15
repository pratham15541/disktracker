//! LLM integration traits and provider adapters.

use crate::errors::Result;
use crate::state::Message;
use async_trait::async_trait;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

#[cfg(feature = "openai")]
pub mod openai;

#[cfg(feature = "openrouter")]
pub mod openrouter;

#[cfg(feature = "anthropic")]
pub mod anthropic;

#[cfg(feature = "ollama")]
pub mod ollama;

/// Tool information for LLM function calling.
///
/// This structure defines a tool that can be called by an LLM,
/// including its name, description, and parameter schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    /// Tool name
    pub name: String,
    
    /// Tool description (helps LLM understand when to use it)
    pub description: String,
    
    /// JSON Schema for tool parameters
    pub parameters: serde_json::Value,
}

impl ToolInfo {
    /// Create a new ToolInfo
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

/// A chunk of a streaming message response.
#[derive(Debug, Clone)]
pub struct MessageChunk {
    /// The delta content
    pub content: String,

    /// Whether this is the final chunk
    pub is_final: bool,

    /// Optional finish reason
    pub finish_reason: Option<String>,
}

/// Trait for chat model implementations.
///
/// This trait abstracts over different LLM providers, allowing
/// you to swap providers without changing your graph logic.
///
/// # Example
///
/// ```rust,ignore
/// use rust_langgraph::llm::ChatModel;
/// use rust_langgraph::state::Message;
///
/// async fn use_model<M: ChatModel>(model: &M) {
///     let messages = vec![Message::user("Hello!")];
///     let response = model.invoke(&messages).await.unwrap();
///     println!("Response: {}", response.content);
/// }
/// ```
#[async_trait]
pub trait ChatModel: Send + Sync {
    /// Invoke the model with a list of messages and get a single response.
    ///
    /// # Arguments
    ///
    /// * `messages` - The conversation history
    ///
    /// # Returns
    ///
    /// The model's response message
    async fn invoke(&self, messages: &[Message]) -> Result<Message>;

    /// Stream the model's response as it's generated.
    ///
    /// # Arguments
    ///
    /// * `messages` - The conversation history
    ///
    /// # Returns
    ///
    /// A stream of message chunks
    async fn stream(
        &self,
        messages: &[Message],
    ) -> Result<Pin<Box<dyn Stream<Item = Result<MessageChunk>> + Send>>> {
        // Default implementation: invoke and yield one chunk
        let message = self.invoke(messages).await?;
        let chunk = MessageChunk {
            content: message.content,
            is_final: true,
            finish_reason: Some("stop".to_string()),
        };

        Ok(Box::pin(futures::stream::once(async move { Ok(chunk) })))
    }

    /// Get the model name/identifier
    fn name(&self) -> &str {
        "unknown"
    }

    /// Clone this model as a trait object.
    ///
    /// This is needed because ChatModel needs to be cloned when
    /// used in nodes that are executed multiple times.
    fn clone_box(&self) -> Box<dyn ChatModel>;
}

impl Clone for Box<dyn ChatModel> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[derive(Clone)]
    struct MockModel;

    #[async_trait]
    impl ChatModel for MockModel {
        async fn invoke(&self, _messages: &[Message]) -> Result<Message> {
            Ok(Message::assistant("Mock response"))
        }

        fn clone_box(&self) -> Box<dyn ChatModel> {
            Box::new(self.clone())
        }

        fn name(&self) -> &str {
            "mock"
        }
    }

    #[tokio::test]
    async fn test_mock_model() {
        let model = MockModel;
        let messages = vec![Message::user("Hello")];
        
        let response = model.invoke(&messages).await.unwrap();
        assert_eq!(response.content, "Mock response");
        assert_eq!(response.role, "assistant");
        assert_eq!(model.name(), "mock");
    }

    #[tokio::test]
    async fn test_default_stream() {
        let model = MockModel;
        let messages = vec![Message::user("Hello")];
        
        let mut stream = model.stream(&messages).await.unwrap();
        let chunk = stream.next().await.unwrap().unwrap();
        
        assert!(chunk.is_final);
        assert!(chunk.content.contains("Mock response"));
    }
}
