//! ReAct (Reasoning and Acting) agent pattern.

use crate::config::Config;
use crate::errors::Result;
use crate::graph::{StateGraph, CompiledGraph};
use crate::llm::ChatModel;
use crate::nodes::Node;
use crate::prebuilt::{Tool, ToolNode, tools_condition};
use crate::pregel::BranchResult;
use crate::state::MessagesState;
use async_trait::async_trait;
use std::sync::Arc;

/// Create a ReAct-style agent graph.
///
/// The ReAct pattern alternates between reasoning (calling the LLM)
/// and acting (executing tools). The graph structure is:
///
/// ```text
/// agent -> [tools_condition] -> tools
///   ^                             |
///   |_____________________________|
/// ```
///
/// # Arguments
///
/// * `model` - The language model to use
/// * `tools` - List of tools the agent can use
///
/// # Returns
///
/// A compiled graph ready to execute
///
/// # Example
///
/// ```rust,ignore
/// use rust_langgraph::prebuilt::{create_react_agent, Tool};
/// use rust_langgraph::llm::ChatModel;
/// use serde_json::json;
///
/// async fn create_agent<M: ChatModel + 'static>(model: M) {
///     let search = Tool::new(
///         "search",
///         "Search for information",
///         |args| async move { Ok(json!({"results": "..."})) }
///     );
///
///     let agent = create_react_agent(model, vec![search]).unwrap();
///     // Use agent.invoke(...) to run
/// }
/// ```
pub fn create_react_agent<M>(
    model: M,
    tools: Vec<Tool>,
) -> Result<CompiledGraph<MessagesState>>
where
    M: ChatModel + 'static,
{
    let mut graph = StateGraph::new();

    // Agent node: calls the LLM
    let model = Arc::new(model);
    let model_clone = model.clone();
    graph.add_node("agent", move |state: MessagesState, _config: &Config| {
        let model = model_clone.clone();
        async move {
            let response = model.invoke(&state.messages).await?;
            Ok(MessagesState {
                messages: vec![response],
            })
        }
    });

    // Tools node: executes tool calls
    let tool_node = ToolNode::new(tools);
    graph.add_node("tools", tool_node);

    // Set entry and edges
    graph.set_entry_point("agent");

    // Conditional edge: route to tools if needed, otherwise end
    graph.add_conditional_edges("agent", |state: &MessagesState| {
        let messages = state.messages.clone();
        async move {
            let route = tools_condition(&messages);
            if route == "tools" {
                Ok(BranchResult::single("tools"))
            } else {
                Ok(BranchResult::end())
            }
        }
    });

    // After tools, always go back to agent
    graph.add_edge("tools", "agent");

    // Compile and return
    graph.compile(None)
}

/// A simple agent node that wraps a ChatModel.
///
/// This can be used as a building block for custom agent patterns.
pub struct AgentNode<M: ChatModel> {
    model: Arc<M>,
}

impl<M: ChatModel> AgentNode<M> {
    /// Create a new agent node
    pub fn new(model: M) -> Self {
        Self {
            model: Arc::new(model),
        }
    }
}

#[async_trait]
impl<M: ChatModel + 'static> Node<MessagesState> for AgentNode<M> {
    async fn invoke(&self, state: MessagesState, _config: &Config) -> Result<MessagesState> {
        let response = self.model.invoke(&state.messages).await?;
        Ok(MessagesState {
            messages: vec![response],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ChatModel;

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
    }

    #[tokio::test]
    async fn test_create_react_agent() {
        let model = MockModel;
        let tool = Tool::new(
            "test",
            "Test tool",
            |_| async move { Ok(serde_json::json!({"result": "ok"})) },
        );

        let agent = create_react_agent(model, vec![tool]);
        assert!(agent.is_ok());
    }

    #[tokio::test]
    async fn test_agent_node() {
        let model = MockModel;
        let node = AgentNode::new(model);

        let state = MessagesState {
            messages: vec![Message::user("Hello")],
        };

        let result = node.invoke(state, &Config::default()).await.unwrap();
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].content, "Mock response");
    }
}
