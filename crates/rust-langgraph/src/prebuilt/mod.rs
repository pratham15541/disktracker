//! Prebuilt graph patterns and utilities.

use crate::state::Message;

pub mod tool;
pub mod react_agent;
pub mod validation;

pub use tool::{Tool, ToolNode};
pub use react_agent::create_react_agent;
pub use validation::validate_chat_history;

/// Routing function for tool execution in agent graphs.
///
/// Returns "tools" if the last message has tool calls, otherwise END.
pub fn tools_condition(messages: &[Message]) -> &'static str {
    if let Some(last_message) = messages.last() {
        if let Some(tool_calls) = &last_message.tool_calls {
            if !tool_calls.is_empty() {
                return "tools";
            }
        }
    }
    "__end__"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ToolCall;

    #[test]
    fn test_tools_condition_with_tools() {
        let messages = vec![
            Message::assistant("").with_tool_calls(vec![
                ToolCall::new("1", "search", serde_json::json!({"query": "test"}))
            ])
        ];

        assert_eq!(tools_condition(&messages), "tools");
    }

    #[test]
    fn test_tools_condition_without_tools() {
        let messages = vec![Message::assistant("Done")];
        assert_eq!(tools_condition(&messages), "__end__");
    }

    #[test]
    fn test_tools_condition_empty() {
        let messages = vec![];
        assert_eq!(tools_condition(&messages), "__end__");
    }
}
