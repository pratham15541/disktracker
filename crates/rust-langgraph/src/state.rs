//! State management and message handling.
//!
//! This module defines the `State` trait which is the core abstraction for
//! graph state, as well as built-in state types like `MessagesState` for
//! chat applications.

use crate::errors::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Debug;

/// The core trait for graph state.
///
/// Implementors of this trait define how state is merged when multiple
/// nodes produce updates that need to be combined.
///
/// # Example
///
/// ```rust
/// use rust_langgraph::{State, Error};
///
/// #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
/// struct CounterState {
///     count: i32,
/// }
///
/// impl State for CounterState {
///     fn merge(&mut self, other: Self) -> Result<(), Error> {
///         self.count += other.count;
///         Ok(())
///     }
/// }
/// ```
pub trait State: Clone + Send + Sync + Serialize + for<'de> Deserialize<'de> + std::fmt::Debug + 'static {
    /// Merge another state into this one.
    ///
    /// This is called when multiple nodes write to the same state,
    /// or when resuming from a checkpoint.
    fn merge(&mut self, other: Self) -> Result<()>;

    /// Convert state to JSON value (default implementation)
    fn to_value(&self) -> Result<serde_json::Value> {
        serde_json::to_value(self).map_err(Error::from)
    }

    /// Create state from JSON value (default implementation)
    fn from_value(value: serde_json::Value) -> Result<Self> {
        serde_json::from_value(value).map_err(Error::from)
    }
}

/// A message in a conversation.
///
/// Messages are the core unit of communication in chat applications.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    /// The role of the message sender (e.g., "user", "assistant", "system")
    pub role: String,
    
    /// The content of the message
    pub content: String,
    
    /// Optional message name/identifier
    pub name: Option<String>,
    
    /// Optional function/tool call information
    pub tool_calls: Option<Vec<ToolCall>>,
    
    /// Optional tool call ID (for tool responses)
    pub tool_call_id: Option<String>,
    
    /// Additional metadata
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Message {
    /// Create a new user message
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            metadata: HashMap::new(),
        }
    }

    /// Create a new assistant message
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            metadata: HashMap::new(),
        }
    }

    /// Create a new system message
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            metadata: HashMap::new(),
        }
    }

    /// Create a new tool message (response from a tool)
    pub fn tool(content: impl Into<String>, tool_call_id: impl Into<String>) -> Self {
        Self {
            role: "tool".to_string(),
            content: content.into(),
            name: None,
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            metadata: HashMap::new(),
        }
    }

    /// Add tool calls to this message
    pub fn with_tool_calls(mut self, tool_calls: Vec<ToolCall>) -> Self {
        self.tool_calls = Some(tool_calls);
        self
    }

    /// Add a name to this message
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// A tool call in a message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    /// Unique identifier for this tool call
    pub id: String,
    
    /// The name of the tool to call
    pub name: String,
    
    /// Arguments to pass to the tool (as JSON)
    pub arguments: serde_json::Value,
}

impl ToolCall {
    /// Create a new tool call
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}

/// State that contains a list of messages.
///
/// This is the standard state type for chat applications and agent workflows.
/// The `add_messages` function provides the reducer logic.
///
/// # Example
///
/// ```rust
/// use rust_langgraph::{State, MessagesState, Message};
///
/// let mut state = MessagesState {
///     messages: vec![Message::user("Hello!")],
/// };
///
/// let update = MessagesState {
///     messages: vec![Message::assistant("Hi there!")],
/// };
///
/// state.merge(update).unwrap();
/// assert_eq!(state.messages.len(), 2);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagesState {
    /// The list of messages
    pub messages: Vec<Message>,
}

impl State for MessagesState {
    fn merge(&mut self, other: Self) -> Result<()> {
        add_messages(&mut self.messages, other.messages);
        Ok(())
    }
}

/// Add messages to an existing list with smart merging.
///
/// This function implements the message reduction logic:
/// - Appends new messages
/// - Updates existing messages if they have the same ID
/// - Handles tool calls and responses properly
///
/// This is used as the default reducer for `MessagesState`.
pub fn add_messages(existing: &mut Vec<Message>, new: Vec<Message>) {
    // For now, simple append logic
    // In a full implementation, this would handle:
    // - Deduplication by message ID
    // - Updating tool call responses
    // - Merging metadata
    existing.extend(new);
}

/// A simple dictionary-based state.
///
/// Useful for quick prototypes or when you don't need custom merge logic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DictState {
    /// The state data
    pub data: HashMap<String, serde_json::Value>,
}

impl State for DictState {
    fn merge(&mut self, other: Self) -> Result<()> {
        // Later values overwrite earlier ones
        self.data.extend(other.data);
        Ok(())
    }
}

impl DictState {
    /// Create a new empty dict state
    pub fn new() -> Self {
        Self {
            data: HashMap::new(),
        }
    }

    /// Create dict state with initial data
    pub fn with_data(data: HashMap<String, serde_json::Value>) -> Self {
        Self { data }
    }

    /// Get a value
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.data.get(key)
    }

    /// Set a value
    pub fn set(&mut self, key: impl Into<String>, value: serde_json::Value) {
        self.data.insert(key.into(), value);
    }
}

impl Default for DictState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
    struct TestState {
        count: i32,
    }

    impl State for TestState {
        fn merge(&mut self, other: Self) -> Result<()> {
            self.count += other.count;
            Ok(())
        }
    }

    #[test]
    fn test_state_merge() {
        let mut state = TestState { count: 5 };
        let other = TestState { count: 3 };
        
        state.merge(other).unwrap();
        assert_eq!(state.count, 8);
    }

    #[test]
    fn test_message_creation() {
        let msg = Message::user("Hello");
        assert_eq!(msg.role, "user");
        assert_eq!(msg.content, "Hello");

        let msg = Message::assistant("Hi").with_name("bot");
        assert_eq!(msg.name.as_deref(), Some("bot"));
    }

    #[test]
    fn test_messages_state() {
        let mut state = MessagesState {
            messages: vec![Message::user("Hello")],
        };

        let update = MessagesState {
            messages: vec![Message::assistant("Hi there!")],
        };

        state.merge(update).unwrap();
        assert_eq!(state.messages.len(), 2);
        assert_eq!(state.messages[0].role, "user");
        assert_eq!(state.messages[1].role, "assistant");
    }

    #[test]
    fn test_dict_state() {
        let mut state = DictState::new();
        state.set("key1", serde_json::json!("value1"));
        
        let mut other = DictState::new();
        other.set("key2", serde_json::json!(42));
        
        state.merge(other).unwrap();
        
        assert_eq!(state.data.len(), 2);
        assert_eq!(state.get("key1").unwrap(), &serde_json::json!("value1"));
        assert_eq!(state.get("key2").unwrap(), &serde_json::json!(42));
    }

    #[test]
    fn test_tool_call() {
        let tool_call = ToolCall::new("call-1", "search", serde_json::json!({"query": "rust"}));
        assert_eq!(tool_call.id, "call-1");
        assert_eq!(tool_call.name, "search");
    }
}
