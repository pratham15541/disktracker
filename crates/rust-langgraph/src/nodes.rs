//! Node abstraction for graph execution.
//!
//! Nodes are the computational units in a LangGraph. Each node takes state
//! as input and produces updated state as output.

use crate::config::Config;
use crate::errors::Result;
use crate::state::State;
use async_trait::async_trait;
use std::fmt::Debug;
use std::future::Future;
use std::sync::Arc;

/// The core trait for graph nodes.
///
/// A node is a unit of computation that takes state and produces
/// updated state. Nodes can be async functions, closures, or
/// custom types implementing this trait.
///
/// # Example
///
/// ```rust
/// use rust_langgraph::{Node, Config, Error};
/// use async_trait::async_trait;
///
/// #[derive(Clone)]
/// struct MyState {
///     count: i32,
/// }
///
/// struct IncrementNode;
///
/// #[async_trait]
/// impl Node<MyState> for IncrementNode {
///     async fn invoke(&self, mut state: MyState, _config: &Config) -> Result<MyState, Error> {
///         state.count += 1;
///         Ok(state)
///     }
/// }
/// ```
#[async_trait]
pub trait Node<S: State>: Send + Sync {
    /// Execute the node with the given state and configuration.
    ///
    /// # Arguments
    ///
    /// * `state` - The current state
    /// * `config` - Execution configuration
    ///
    /// # Returns
    ///
    /// The updated state or an error
    async fn invoke(&self, state: S, config: &Config) -> Result<S>;
}

// Implement Node for async closures
#[async_trait]
impl<S, F, Fut> Node<S> for F
where
    S: State,
    F: Fn(S, &Config) -> Fut + Send + Sync,
    Fut: Future<Output = Result<S>> + Send,
{
    async fn invoke(&self, state: S, config: &Config) -> Result<S> {
        self(state, config).await
    }
}

/// Type alias for boxed nodes
pub type NodeBox<S> = Box<dyn Node<S>>;

/// Type alias for arc'd nodes (more efficient for shared ownership)
pub type NodeArc<S> = Arc<dyn Node<S>>;

/// A node in the Pregel execution engine.
///
/// PregelNode wraps a user's node with metadata about which channels
/// it reads from and writes to, and what triggers it.
#[derive(Clone)]
pub struct PregelNode<S: State> {
    /// The name of this node
    pub name: String,

    /// The channels this node reads from
    pub channels: Vec<String>,

    /// The channels that trigger this node when written to
    pub triggers: Vec<String>,

    /// The actual node computation
    pub bound: NodeArc<S>,

    /// The channels this node writes to
    pub writers: Vec<ChannelWrite>,
}

impl<S: State> Debug for PregelNode<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PregelNode")
            .field("name", &self.name)
            .field("channels", &self.channels)
            .field("triggers", &self.triggers)
            .field("bound", &"<node>")
            .field("writers", &self.writers)
            .finish()
    }
}

impl<S: State> PregelNode<S> {
    /// Create a new PregelNode
    pub fn new(
        name: impl Into<String>,
        channels: Vec<String>,
        triggers: Vec<String>,
        bound: NodeArc<S>,
        writers: Vec<ChannelWrite>,
    ) -> Self {
        Self {
            name: name.into(),
            channels,
            triggers,
            bound,
            writers,
        }
    }

    /// Create a PregelNode from a concrete node implementation
    pub fn from_node(
        name: impl Into<String>,
        channels: Vec<String>,
        triggers: Vec<String>,
        bound: impl Node<S> + 'static,
        writers: Vec<ChannelWrite>,
    ) -> Self {
        Self {
            name: name.into(),
            channels,
            triggers,
            bound: Arc::new(bound),
            writers,
        }
    }

    /// Check if this node is triggered by the given channel writes
    pub fn is_triggered(&self, written_channels: &[String]) -> bool {
        self.triggers.iter().any(|t| written_channels.contains(t))
    }
}

/// Specification for writing to a channel after node execution
#[derive(Debug, Clone)]
pub struct ChannelWrite {
    /// The channel to write to
    pub channel: String,

    /// Whether to skip writing if the value is None
    pub skip_none: bool,

    /// Optional mapper function name
    pub mapper: Option<String>,
}

impl ChannelWrite {
    /// Create a new channel write specification
    pub fn new(channel: impl Into<String>) -> Self {
        Self {
            channel: channel.into(),
            skip_none: true,
            mapper: None,
        }
    }

    /// Set whether to skip None values
    pub fn with_skip_none(mut self, skip: bool) -> Self {
        self.skip_none = skip;
        self
    }
}

/// Helper to create a simple node from an async function
pub fn node_fn<S, F, Fut>(f: F) -> impl Node<S>
where
    S: State,
    F: Fn(S, &Config) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<S>> + Send + 'static,
{
    f
}

/// Helper to create a node that doesn't use config
pub fn simple_node<S, F, Fut>(f: F) -> impl Node<S>
where
    S: State,
    F: Fn(S) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<S>> + Send + 'static,
{
    move |state: S, _config: &Config| f(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State as StateTrait;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, Serialize, Deserialize)]
    struct TestState {
        count: i32,
    }

    impl StateTrait for TestState {
        fn merge(&mut self, other: Self) -> Result<()> {
            self.count += other.count;
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_node_from_closure() {
        let node = |mut state: TestState, _config: &Config| async move {
            state.count += 1;
            Ok(state)
        };

        let state = TestState { count: 0 };
        let result = node.invoke(state, &Config::default()).await.unwrap();
        assert_eq!(result.count, 1);
    }

    #[tokio::test]
    async fn test_simple_node() {
        let node = simple_node(|mut state: TestState| async move {
            state.count += 10;
            Ok(state)
        });

        let state = TestState { count: 5 };
        let result = node.invoke(state, &Config::default()).await.unwrap();
        assert_eq!(result.count, 15);
    }

    struct CustomNode;

    #[async_trait]
    impl Node<TestState> for CustomNode {
        async fn invoke(&self, mut state: TestState, _config: &Config) -> Result<TestState> {
            state.count *= 2;
            Ok(state)
        }
    }

    #[tokio::test]
    async fn test_custom_node() {
        let node = CustomNode;
        let state = TestState { count: 5 };
        let result = node.invoke(state, &Config::default()).await.unwrap();
        assert_eq!(result.count, 10);
    }

    #[test]
    fn test_pregel_node_is_triggered() {
        let node = PregelNode::from_node(
            "test",
            vec!["in".to_string()],
            vec!["trigger_a".to_string(), "trigger_b".to_string()],
            |state: TestState, _: &Config| async move { Ok(state) },
            vec![],
        );

        assert!(node.is_triggered(&["trigger_a".to_string()]));
        assert!(node.is_triggered(&["trigger_b".to_string()]));
        assert!(node.is_triggered(&["trigger_a".to_string(), "other".to_string()]));
        assert!(!node.is_triggered(&["other".to_string()]));
        assert!(!node.is_triggered(&[]));
    }

    #[test]
    fn test_channel_write() {
        let write = ChannelWrite::new("output").with_skip_none(false);
        assert_eq!(write.channel, "output");
        assert!(!write.skip_none);
    }
}
