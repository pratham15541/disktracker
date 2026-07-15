//! Message validation utilities for agent workflows.
//!
//! This module provides validation for chat histories, ensuring
//! that tool calls and tool responses are properly paired.

use crate::errors::{Error, Result};
use crate::state::Message;
use std::collections::HashSet;

/// Validate that all tool calls have corresponding tool messages.
///
/// This matches Python LangGraph's `_validate_chat_history` logic,
/// ensuring that every assistant message with tool_calls has
/// corresponding tool response messages.
///
/// # Arguments
///
/// * `messages` - The message history to validate
///
/// # Returns
///
/// Ok(()) if validation passes, or an Error if tool calls are missing responses
///
/// # Example
///
/// ```rust
/// use rust_langgraph::prebuilt::validation::validate_chat_history;
/// use rust_langgraph::state::{Message, ToolCall};
/// use serde_json::json;
///
/// let messages = vec![
///     Message::assistant("Calling tool").with_tool_calls(vec![
///         ToolCall::new("call-1", "search", json!({"query": "rust"}))
///     ]),
///     Message::tool("Results found", "call-1"),
/// ];
///
/// assert!(validate_chat_history(&messages).is_ok());
/// ```
pub fn validate_chat_history(messages: &[Message]) -> Result<()> {
    // Collect all tool call IDs from assistant messages
    let mut all_tool_call_ids = HashSet::new();
    for msg in messages {
        if msg.role == "assistant" {
            if let Some(tool_calls) = &msg.tool_calls {
                for tc in tool_calls {
                    all_tool_call_ids.insert(tc.id.clone());
                }
            }
        }
    }

    // Collect tool call IDs that have responses
    let mut tool_call_ids_with_results = HashSet::new();
    for msg in messages {
        if msg.role == "tool" {
            if let Some(ref id) = msg.tool_call_id {
                tool_call_ids_with_results.insert(id.clone());
            }
        }
    }

    // Find tool calls without responses
    let missing: Vec<_> = all_tool_call_ids
        .difference(&tool_call_ids_with_results)
        .collect();

    if !missing.is_empty() {
        return Err(Error::invalid_update(format!(
            "Found AIMessages with tool_calls that do not have corresponding ToolMessage. \
            Missing tool_call_ids: {:?}. \
            Every tool call must have a corresponding ToolMessage.",
            missing
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ToolCall;
    use serde_json::json;

    #[test]
    fn test_valid_chat_history() {
        let messages = vec![
            Message::user("Hello"),
            Message::assistant("Calling tool").with_tool_calls(vec![
                ToolCall::new("call-1", "search", json!({"query": "rust"}))
            ]),
            Message::tool("Results", "call-1"),
            Message::assistant("Here are the results"),
        ];

        assert!(validate_chat_history(&messages).is_ok());
    }

    #[test]
    fn test_missing_tool_response() {
        let messages = vec![
            Message::user("Hello"),
            Message::assistant("Calling tool").with_tool_calls(vec![
                ToolCall::new("call-1", "search", json!({"query": "rust"}))
            ]),
            // Missing tool response
            Message::assistant("Continuing..."),
        ];

        let result = validate_chat_history(&messages);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("call-1"));
    }

    #[test]
    fn test_multiple_tool_calls() {
        let messages = vec![
            Message::assistant("Calling tools").with_tool_calls(vec![
                ToolCall::new("call-1", "search", json!({})),
                ToolCall::new("call-2", "calc", json!({})),
            ]),
            Message::tool("Result 1", "call-1"),
            Message::tool("Result 2", "call-2"),
        ];

        assert!(validate_chat_history(&messages).is_ok());
    }

    #[test]
    fn test_multiple_tool_calls_one_missing() {
        let messages = vec![
            Message::assistant("Calling tools").with_tool_calls(vec![
                ToolCall::new("call-1", "search", json!({})),
                ToolCall::new("call-2", "calc", json!({})),
            ]),
            Message::tool("Result 1", "call-1"),
            // Missing call-2 response
        ];

        let result = validate_chat_history(&messages);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("call-2"));
    }

    #[test]
    fn test_no_tool_calls() {
        let messages = vec![
            Message::user("Hello"),
            Message::assistant("Hi there!"),
        ];

        assert!(validate_chat_history(&messages).is_ok());
    }
}
