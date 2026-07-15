//! Tool execution for agents.

use crate::config::Config;
use crate::errors::Result;
use crate::llm::ToolInfo;
use crate::nodes::Node;
use crate::state::{Message, MessagesState, ToolCall};
use async_trait::async_trait;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::sync::Arc;

/// A tool that can be called by an agent.
///
/// Tools are functions that agents can invoke to perform actions
/// or retrieve information.
///
/// Note: Tool cannot be Clone because it contains a function pointer.
/// Use Arc<Tool> if you need to share tools across threads.
///
/// # Example
///
/// ```rust
/// use rust_langgraph::prebuilt::Tool;
/// use serde_json::json;
///
/// let search_tool = Tool::new(
///     "search",
///     "Search for information online",
///     |args: serde_json::Value| async move {
///         let query = args["query"].as_str().unwrap_or("");
///         Ok(json!({"results": format!("Results for: {}", query)}))
///     }
/// ).with_schema(json!({
///     "type": "object",
///     "properties": {
///         "query": {"type": "string", "description": "Search query"}
///     },
///     "required": ["query"]
/// }));
/// ```
pub struct Tool {
    /// Tool name
    pub name: String,

    /// Tool description (for LLM to understand when to use it)
    pub description: String,

    /// Optional JSON Schema for parameters
    pub schema: Option<JsonValue>,

    /// The tool implementation
    pub func: Arc<dyn Fn(JsonValue) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<JsonValue>> + Send>> + Send + Sync>,
}

impl Tool {
    /// Create a new tool
    pub fn new<F, Fut>(name: impl Into<String>, description: impl Into<String>, func: F) -> Self
    where
        F: Fn(JsonValue) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<JsonValue>> + Send + 'static,
    {
        Self {
            name: name.into(),
            description: description.into(),
            schema: None,
            func: Arc::new(move |args| Box::pin(func(args))),
        }
    }

    /// Set a custom parameter schema for this tool.
    ///
    /// The schema should be a JSON Schema object defining the tool's parameters.
    ///
    /// # Example
    ///
    /// ```rust
    /// use rust_langgraph::prebuilt::Tool;
    /// use serde_json::json;
    ///
    /// let tool = Tool::new("search", "Search", |args| async move { Ok(args) })
    ///     .with_schema(json!({
    ///         "type": "object",
    ///         "properties": {
    ///             "query": {"type": "string"}
    ///         },
    ///         "required": ["query"]
    ///     }));
    /// ```
    pub fn with_schema(mut self, schema: JsonValue) -> Self {
        self.schema = Some(schema);
        self
    }

    /// Execute the tool with the given arguments
    pub async fn invoke(&self, args: JsonValue) -> Result<JsonValue> {
        (self.func)(args).await
    }

    /// Convert to ToolInfo for passing to LLM adapters.
    ///
    /// This creates a ToolInfo struct that can be passed to
    /// `OllamaAdapter::with_tools()` or similar methods.
    pub fn to_tool_info(&self) -> ToolInfo {
        let parameters = self.schema.clone().unwrap_or_else(|| {
            serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            })
        });

        ToolInfo {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters,
        }
    }

    /// Get the tool schema for LLM function calling (OpenAI format).
    ///
    /// Deprecated: Use `to_tool_info()` instead.
    pub fn to_schema(&self) -> JsonValue {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.schema.clone().unwrap_or_else(|| {
                    serde_json::json!({
                        "type": "object",
                        "properties": {},
                        "required": []
                    })
                })
            }
        })
    }
}

/// A node that executes tool calls from agent messages.
///
/// ToolNode looks for tool_calls in the last message, executes them,
/// and returns tool response messages.
///
/// # Example
///
/// ```rust,no_run
/// use rust_langgraph::prebuilt::{Tool, ToolNode};
/// use serde_json::json;
///
/// let search_tool = Tool::new(
///     "search",
///     "Search the web",
///     |args| async move { Ok(json!({"result": "found it"})) }
/// );
///
/// let tool_node = ToolNode::new(vec![search_tool]);
/// ```
pub struct ToolNode {
    tools: HashMap<String, Tool>,
}

impl ToolNode {
    /// Create a new ToolNode with the given tools
    pub fn new(tools: Vec<Tool>) -> Self {
        let tools = tools
            .into_iter()
            .map(|tool| (tool.name.clone(), tool))
            .collect();

        Self { tools }
    }

    /// Execute a single tool call
    async fn execute_tool_call(&self, tool_call: &ToolCall) -> Message {
        match self.tools.get(&tool_call.name) {
            Some(tool) => {
                match tool.invoke(tool_call.arguments.clone()).await {
                    Ok(result) => Message::tool(
                        serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string()),
                        &tool_call.id,
                    ),
                    Err(e) => Message::tool(
                        format!("Error: {}", e),
                        &tool_call.id,
                    ),
                }
            }
            None => Message::tool(
                format!("Tool '{}' not found", tool_call.name),
                &tool_call.id,
            ),
        }
    }
}

#[async_trait]
impl Node<MessagesState> for ToolNode {
    async fn invoke(&self, state: MessagesState, _config: &Config) -> Result<MessagesState> {
        // Find tool calls in the last message
        let tool_calls = state
            .messages
            .last()
            .and_then(|msg| msg.tool_calls.as_ref())
            .cloned()
            .unwrap_or_default();

        if tool_calls.is_empty() {
            return Ok(MessagesState {
                messages: vec![],
            });
        }

        // Execute all tool calls sequentially (tools HashMap is not Clone)
        let mut tool_messages = Vec::new();
        for tool_call in &tool_calls {
            let message = self.execute_tool_call(tool_call).await;
            tool_messages.push(message);
        }

        Ok(MessagesState {
            messages: tool_messages,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_tool_creation() {
        let tool = Tool::new(
            "test_tool",
            "A test tool",
            |args: JsonValue| async move {
                Ok(serde_json::json!({"result": args["input"]}))
            },
        );

        assert_eq!(tool.name, "test_tool");
        assert_eq!(tool.description, "A test tool");

        let result = tool.invoke(serde_json::json!({"input": "hello"})).await.unwrap();
        assert_eq!(result["result"], "hello");
    }

    #[tokio::test]
    async fn test_tool_node() {
        let tool = Tool::new(
            "echo",
            "Echo the input",
            |args: JsonValue| async move { Ok(args) },
        );

        let tool_node = ToolNode::new(vec![tool]);

        let state = MessagesState {
            messages: vec![
                Message::assistant("").with_tool_calls(vec![
                    ToolCall::new("call-1", "echo", serde_json::json!({"msg": "test"}))
                ])
            ],
        };

        let result = tool_node.invoke(state, &Config::default()).await.unwrap();
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].role, "tool");
        assert_eq!(result.messages[0].tool_call_id.as_deref(), Some("call-1"));
    }

    #[tokio::test]
    async fn test_tool_node_unknown_tool() {
        let tool_node = ToolNode::new(vec![]);

        let state = MessagesState {
            messages: vec![
                Message::assistant("").with_tool_calls(vec![
                    ToolCall::new("call-1", "unknown", serde_json::json!({}))
                ])
            ],
        };

        let result = tool_node.invoke(state, &Config::default()).await.unwrap();
        assert_eq!(result.messages.len(), 1);
        assert!(result.messages[0].content.contains("not found"));
    }
}
