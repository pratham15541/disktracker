//! Branch logic for conditional routing.
//!
//! Branches enable dynamic routing in graphs based on state conditions.

use crate::errors::Result;
use crate::state::State;
use crate::types::Send as SendType;
use async_trait::async_trait;
use std::future::Future;

/// The result of evaluating a branch condition.
///
/// Branches can route to a single node, multiple nodes in parallel,
/// dynamic Send targets, or end execution.
#[derive(Debug, Clone)]
pub enum BranchResult {
    /// Go to a single next node
    Single(String),

    /// Go to multiple nodes in parallel
    Multiple(Vec<String>),

    /// Dynamic routing with Send (map-reduce pattern)
    Send(Vec<SendType>),

    /// End execution
    End,
}

impl BranchResult {
    /// Create a Single variant
    pub fn single(node: impl Into<String>) -> Self {
        BranchResult::Single(node.into())
    }

    /// Create a Multiple variant
    pub fn multiple(nodes: Vec<impl Into<String>>) -> Self {
        BranchResult::Multiple(nodes.into_iter().map(|n| n.into()).collect())
    }

    /// Create a Send variant
    pub fn send(sends: Vec<SendType>) -> Self {
        BranchResult::Send(sends)
    }

    /// Create an End variant
    pub fn end() -> Self {
        BranchResult::End
    }

    /// Check if this is an End result
    pub fn is_end(&self) -> bool {
        matches!(self, BranchResult::End)
    }

    /// Get the list of node names to route to (if Single or Multiple)
    pub fn node_names(&self) -> Vec<String> {
        match self {
            BranchResult::Single(name) => vec![name.clone()],
            BranchResult::Multiple(names) => names.clone(),
            BranchResult::Send(_) => vec![],
            BranchResult::End => vec![],
        }
    }
}

/// Trait for conditional routing logic.
///
/// Branches examine state and decide which node(s) to execute next.
///
/// # Example
///
/// ```rust
/// use rust_langgraph::pregel::{Branch, BranchResult};
/// use rust_langgraph::Error;
/// use async_trait::async_trait;
///
/// struct MyBranch;
///
/// #[async_trait]
/// impl<S> Branch<S> for MyBranch
/// where
///     S: Send + Sync + 'static,
/// {
///     async fn route(&self, _state: &S) -> Result<BranchResult, Error> {
///         Ok(BranchResult::single("next_node"))
///     }
/// }
/// ```
#[async_trait]
pub trait Branch<S>: std::marker::Send + Sync
where
    S: State,
{
    /// Evaluate the branch condition and return routing decision.
    ///
    /// # Arguments
    ///
    /// * `state` - The current graph state
    ///
    /// # Returns
    ///
    /// A BranchResult indicating where to route next
    async fn route(&self, state: &S) -> Result<BranchResult>;
}

// Implement Branch for async closures
#[async_trait]
impl<S, F, Fut> Branch<S> for F
where
    S: State,
    F: Fn(&S) -> Fut + std::marker::Send + Sync,
    Fut: Future<Output = Result<BranchResult>> + std::marker::Send,
{
    async fn route(&self, state: &S) -> Result<BranchResult> {
        self(state).await
    }
}

/// Type alias for boxed branches
pub type BranchBox<S> = Box<dyn Branch<S>>;

/// Helper to create a branch from a closure
pub fn branch_fn<S, F, Fut>(f: F) -> impl Branch<S>
where
    S: State,
    F: Fn(&S) -> Fut + std::marker::Send + Sync + 'static,
    Fut: Future<Output = Result<BranchResult>> + std::marker::Send + 'static,
{
    f
}

/// A simple branch that always routes to the same node
pub struct StaticBranch {
    target: String,
}

impl StaticBranch {
    /// Create a new static branch
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
        }
    }
}

#[async_trait]
impl<S: State> Branch<S> for StaticBranch {
    async fn route(&self, _state: &S) -> Result<BranchResult> {
        Ok(BranchResult::Single(self.target.clone()))
    }
}

/// A branch that always ends execution
pub struct EndBranch;

#[async_trait]
impl<S: State> Branch<S> for EndBranch {
    async fn route(&self, _state: &S) -> Result<BranchResult> {
        Ok(BranchResult::End)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DictState;

    #[test]
    fn test_branch_result_creation() {
        let single = BranchResult::single("node1");
        assert!(matches!(single, BranchResult::Single(_)));
        assert_eq!(single.node_names(), vec!["node1"]);

        let multiple = BranchResult::multiple(vec!["node1", "node2"]);
        assert!(matches!(multiple, BranchResult::Multiple(_)));
        assert_eq!(multiple.node_names(), vec!["node1", "node2"]);

        let end = BranchResult::end();
        assert!(end.is_end());
        assert!(end.node_names().is_empty());
    }

    #[tokio::test]
    async fn test_static_branch() {
        let branch = StaticBranch::new("target");
        let state = DictState::new();
        let result = branch.route(&state).await.unwrap();

        assert!(matches!(result, BranchResult::Single(_)));
        assert_eq!(result.node_names(), vec!["target"]);
    }

    #[tokio::test]
    async fn test_end_branch() {
        let branch = EndBranch;
        let state = DictState::new();
        let result = branch.route(&state).await.unwrap();

        assert!(result.is_end());
    }

    #[tokio::test]
    async fn test_branch_closure() {
        let branch = |_state: &DictState| async {
            Ok(BranchResult::single("dynamic_node"))
        };

        let state = DictState::new();
        let result = branch.route(&state).await.unwrap();

        assert_eq!(result.node_names(), vec!["dynamic_node"]);
    }

    #[tokio::test]
    async fn test_branch_with_send() {
        let sends = vec![
            SendType::new("process", serde_json::json!({"id": 1})),
            SendType::new("process", serde_json::json!({"id": 2})),
        ];

        let result = BranchResult::send(sends);
        assert!(matches!(result, BranchResult::Send(_)));
    }
}
