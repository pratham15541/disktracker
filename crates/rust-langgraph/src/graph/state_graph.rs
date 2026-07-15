//! StateGraph builder and CompiledGraph.
//!
//! StateGraph provides an ergonomic builder API for creating graphs,
//! which then compile into executable CompiledGraph instances.

use crate::channels::{BaseChannel, LastValue};
use crate::checkpoint::{BaseCheckpointSaver, CheckpointMetadata, StateSnapshot};
use crate::config::Config;
use crate::errors::{Error, Result};
use crate::graph::{START, END};
use crate::nodes::{Node, PregelNode, ChannelWrite, NodeArc};
use crate::pregel::{Branch, Pregel};
use crate::state::State;
use crate::types::{StreamEvent, StreamMode};
use futures::stream::Stream;
use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::Arc;

/// Builder for creating state-based graphs.
///
/// StateGraph provides a declarative API for building graphs where nodes
/// communicate through shared state. It compiles into a CompiledGraph
/// which can be executed.
///
/// # Example
///
/// ```rust
/// use rust_langgraph::{StateGraph, Config};
/// # use rust_langgraph::{State, Error};
/// # #[derive(Clone, serde::Serialize, serde::Deserialize)]
/// # struct MyState { count: i32 }
/// # impl State for MyState {
/// #     fn merge(&mut self, other: Self) -> Result<(), Error> {
/// #         self.count += other.count;
/// #         Ok(())
/// #     }
/// # }
///
/// let mut graph = StateGraph::new();
///
/// graph.add_node("process", |mut state: MyState, _config: &Config| async move {
///     state.count += 1;
///     Ok(state)
/// });
///
/// graph.set_entry_point("process");
/// graph.set_finish_point("process");
///
/// let app = graph.compile(None).unwrap();
/// ```
pub struct StateGraph<S: State> {
    nodes: HashMap<String, Box<dyn Node<S>>>,
    edges: HashMap<String, Vec<String>>,
    conditional_edges: HashMap<String, Box<dyn Branch<S>>>,
    entry_point: Option<String>,
    finish_points: HashSet<String>,
}

impl<S: State> StateGraph<S> {
    /// Create a new StateGraph
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            edges: HashMap::new(),
            conditional_edges: HashMap::new(),
            entry_point: None,
            finish_points: HashSet::new(),
        }
    }

    /// Add a node to the graph
    ///
    /// # Arguments
    ///
    /// * `name` - Unique identifier for this node
    /// * `node` - The node implementation (function or struct implementing Node)
    ///
    /// # Example
    ///
    /// ```rust
    /// # use rust_langgraph::{StateGraph, Config, State, Error};
    /// # #[derive(Clone, serde::Serialize, serde::Deserialize)]
    /// # struct MyState { count: i32 }
    /// # impl State for MyState {
    /// #     fn merge(&mut self, other: Self) -> Result<(), Error> { Ok(()) }
    /// # }
    /// let mut graph = StateGraph::new();
    ///
    /// graph.add_node("increment", |mut state: MyState, _config: &Config| async move {
    ///     state.count += 1;
    ///     Ok(state)
    /// });
    /// ```
    pub fn add_node(&mut self, name: impl Into<String>, node: impl Node<S> + 'static) -> &mut Self {
        self.nodes.insert(name.into(), Box::new(node));
        self
    }

    /// Add a static edge from one node to another
    ///
    /// # Arguments
    ///
    /// * `from` - Source node name
    /// * `to` - Target node name
    pub fn add_edge(&mut self, from: impl Into<String>, to: impl Into<String>) -> &mut Self {
        let from = from.into();
        let to = to.into();

        self.edges.entry(from).or_default().push(to);
        self
    }

    /// Add conditional edges from a source node
    ///
    /// The branch function examines state and returns which node(s) to route to next.
    ///
    /// # Arguments
    ///
    /// * `source` - Source node name
    /// * `branch` - Branch logic that determines routing
    ///
    /// # Example
    ///
    /// ```rust
    /// # use rust_langgraph::{StateGraph, Config, State, Error};
    /// # use rust_langgraph::pregel::BranchResult;
    /// # #[derive(Clone, serde::Serialize, serde::Deserialize)]
    /// # struct MyState { value: i32 }
    /// # impl State for MyState {
    /// #     fn merge(&mut self, other: Self) -> Result<(), Error> { Ok(()) }
    /// # }
    /// let mut graph = StateGraph::new();
    ///
    /// graph.add_conditional_edges(
    ///     "check",
    ///     |state: &MyState| async move {
    ///         if state.value > 0 {
    ///             Ok(BranchResult::single("positive"))
    ///         } else {
    ///             Ok(BranchResult::single("negative"))
    ///         }
    ///     }
    /// );
    /// ```
    pub fn add_conditional_edges(
        &mut self,
        source: impl Into<String>,
        branch: impl Branch<S> + 'static,
    ) -> &mut Self {
        self.conditional_edges.insert(source.into(), Box::new(branch));
        self
    }

    /// Set the entry point for the graph
    ///
    /// This is the first node to execute when the graph is invoked.
    pub fn set_entry_point(&mut self, node: impl Into<String>) -> &mut Self {
        self.entry_point = Some(node.into());
        self
    }

    /// Set a finish point for the graph
    ///
    /// When execution reaches a finish point, the graph completes.
    pub fn set_finish_point(&mut self, node: impl Into<String>) -> &mut Self {
        self.finish_points.insert(node.into());
        self
    }

    /// Add multiple finish points
    pub fn add_finish_points(&mut self, nodes: Vec<impl Into<String>>) -> &mut Self {
        for node in nodes {
            self.finish_points.insert(node.into());
        }
        self
    }

    /// Compile the graph into an executable CompiledGraph
    ///
    /// # Arguments
    ///
    /// * `checkpointer` - Optional checkpoint saver for persistence
    ///
    /// # Returns
    ///
    /// A CompiledGraph ready to execute
    ///
    /// # Errors
    ///
    /// Returns an error if the graph configuration is invalid (e.g., no entry point)
    pub fn compile(
        self,
        checkpointer: Option<Arc<dyn BaseCheckpointSaver>>,
    ) -> Result<CompiledGraph<S>> {
        // Validate graph
        if self.entry_point.is_none() {
            return Err(Error::invalid_graph("No entry point set"));
        }

        let entry_point = self.entry_point.unwrap();

        if !self.nodes.contains_key(&entry_point) {
            return Err(Error::invalid_graph(format!(
                "Entry point '{}' is not a valid node",
                entry_point
            )));
        }

        // Build PregelNodes from our nodes
        let mut pregel_nodes = HashMap::new();

        for (name, node) in self.nodes {
            // Determine triggers: nodes triggered by their dependencies
            let mut triggers = vec![];

            // If this is the entry point, trigger on START
            if name == entry_point {
                triggers.push(START.to_string());
            }

            // Add triggers from incoming edges
            for (source, targets) in &self.edges {
                if targets.contains(&name) {
                    triggers.push(format!("{}_output", source));
                }
            }

            // If no triggers, add a default
            if triggers.is_empty() {
                triggers.push(format!("{}_input", name));
            }

            let pregel_node = PregelNode::new(
                name.clone(),
                vec![format!("{}_input", name)],
                triggers,
                Arc::from(node) as NodeArc<S>,
                vec![ChannelWrite::new(format!("{}_output", name))],
            );

            pregel_nodes.insert(name, pregel_node);
        }

        // Create channels
        let mut channels: HashMap<String, Box<dyn BaseChannel>> = HashMap::new();

        // Add START and END channels
        channels.insert(START.to_string(), Box::new(LastValue::<S>::new()));
        channels.insert(END.to_string(), Box::new(LastValue::<S>::new()));

        // Add channels for each node
        for node_name in pregel_nodes.keys() {
            channels.insert(
                format!("{}_input", node_name),
                Box::new(LastValue::<S>::new()),
            );
            channels.insert(
                format!("{}_output", node_name),
                Box::new(LastValue::<S>::new()),
            );
        }

        // Create the Pregel engine
        let pregel = Pregel::new(
            pregel_nodes,
            channels,
            checkpointer.clone(),
            entry_point,
            self.finish_points,
            self.edges.clone(),
            self.conditional_edges,
        );

        Ok(CompiledGraph {
            pregel,
            checkpointer,
        })
    }
}

impl<S: State> Default for StateGraph<S> {
    fn default() -> Self {
        Self::new()
    }
}

/// A compiled, executable graph.
///
/// CompiledGraph is the result of compiling a StateGraph. It provides
/// methods to execute the graph with different invocation patterns.
pub struct CompiledGraph<S: State> {
    pregel: Pregel<S>,
    checkpointer: Option<Arc<dyn BaseCheckpointSaver>>,
}

impl<S: State> CompiledGraph<S> {
    /// Execute the graph with the given input
    ///
    /// This runs the graph to completion and returns the final state.
    ///
    /// # Arguments
    ///
    /// * `input` - Initial state
    /// * `config` - Execution configuration
    ///
    /// # Returns
    ///
    /// The final state after graph execution
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use rust_langgraph::{StateGraph, Config, State, Error};
    /// # #[derive(Clone, serde::Serialize, serde::Deserialize)]
    /// # struct MyState { count: i32 }
    /// # impl State for MyState {
    /// #     fn merge(&mut self, other: Self) -> Result<(), Error> { Ok(()) }
    /// # }
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let mut graph = StateGraph::new();
    /// # graph.add_node("test", |s: MyState, _| async move { Ok(s) });
    /// # graph.set_entry_point("test");
    /// # graph.set_finish_point("test");
    /// let app = graph.compile(None)?;
    ///
    /// let result = app.invoke(
    ///     MyState { count: 0 },
    ///     Config::default()
    /// ).await?;
    ///
    /// println!("Final count: {}", result.count);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn invoke(&mut self, input: S, config: Config) -> Result<S> {
        self.pregel.invoke(input, config).await
    }

    /// Stream execution events
    ///
    /// Returns a stream of events as the graph executes, allowing
    /// real-time observation of progress.
    ///
    /// # Arguments
    ///
    /// * `input` - Initial state
    /// * `config` - Execution configuration
    /// * `mode` - Type of events to stream
    pub async fn stream(
        &mut self,
        input: S,
        config: Config,
        mode: StreamMode,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        self.pregel.stream(input, config, mode).await
    }

    /// Get the current state for a given configuration
    ///
    /// This retrieves the most recent checkpoint for the thread.
    pub async fn get_state(&self, config: &Config) -> Result<Option<StateSnapshot<S>>> {
        self.pregel.get_state(config).await
    }

    /// Get the state history for a thread
    ///
    /// Returns past checkpoints in reverse chronological order.
    pub async fn get_state_history(
        &self,
        config: &Config,
        limit: Option<usize>,
    ) -> Result<Vec<StateSnapshot<S>>> {
        self.pregel.get_state_history(config, limit).await
    }

    /// Update the state for a thread
    ///
    /// This allows modifying the checkpoint state, useful for
    /// human-in-the-loop patterns.
    pub async fn update_state(&mut self, config: Config, values: S) -> Result<Config> {
        if let Some(checkpointer) = &self.checkpointer {
            // Get the current checkpoint
            let mut tuple = checkpointer
                .get_tuple(&config)
                .await?
                .ok_or_else(|| Error::checkpoint("No checkpoint found for config"))?;

            // Update the state
            let mut current_state = S::from_value(
                tuple
                    .checkpoint
                    .get_channel("__start__")
                    .ok_or_else(|| Error::checkpoint("No state in checkpoint"))?
                    .clone(),
            )?;

            current_state.merge(values)?;

            // Create new checkpoint
            tuple.checkpoint.set_channel("__start__", current_state.to_value()?);

            // Save the updated checkpoint
            let metadata = CheckpointMetadata {
                step: tuple.metadata.step + 1,
                source: "update_state".to_string(),
                created_at: chrono::Utc::now(),
                extra: HashMap::new(),
            };

            checkpointer.put(&tuple.checkpoint, &metadata, &config).await
        } else {
            Err(Error::checkpoint("No checkpointer configured"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint_backends::memory::MemorySaver;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
    struct TestState {
        count: i32,
    }

    impl crate::state::State for TestState {
        fn merge(&mut self, other: Self) -> Result<()> {
            self.count += other.count;
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_state_graph_basic() {
        let mut graph = StateGraph::new();

        graph.add_node("increment", |mut state: TestState, _config: &Config| async move {
            state.count += 1;
            Ok(state)
        });

        graph.set_entry_point("increment");
        graph.set_finish_point("increment");

        let mut app = graph.compile(None).unwrap();

        let result = app.invoke(TestState { count: 0 }, Config::default()).await.unwrap();
        assert_eq!(result.count, 1);
    }

    #[tokio::test]
    async fn test_state_graph_chain() {
        let mut graph = StateGraph::new();

        graph.add_node("add_one", |mut state: TestState, _config: &Config| async move {
            state.count += 1;
            Ok(state)
        });

        graph.add_node("multiply_two", |mut state: TestState, _config: &Config| async move {
            state.count *= 2;
            Ok(state)
        });

        graph.set_entry_point("add_one");
        graph.add_edge("add_one", "multiply_two");
        graph.set_finish_point("multiply_two");

        let mut app = graph.compile(None).unwrap();

        let result = app.invoke(TestState { count: 5 }, Config::default()).await.unwrap();
        assert_eq!(result.count, 12); // (5 + 1) * 2
    }

    #[tokio::test]
    async fn test_state_graph_with_checkpointer() {
        let mut graph = StateGraph::new();

        graph.add_node("increment", |mut state: TestState, _config: &Config| async move {
            state.count += 1;
            Ok(state)
        });

        graph.set_entry_point("increment");
        graph.set_finish_point("increment");

        let checkpointer = Arc::new(MemorySaver::new());
        let mut app = graph.compile(Some(checkpointer)).unwrap();

        let config = Config::new().with_thread_id("test-123");
        let result = app.invoke(TestState { count: 0 }, config.clone()).await.unwrap();
        assert_eq!(result.count, 1);

        // Check that checkpoint was saved
        let snapshot = app.get_state(&config).await.unwrap();
        assert!(snapshot.is_some());
    }

    #[test]
    fn test_state_graph_no_entry_point() {
        let mut graph = StateGraph::<TestState>::new();
        graph.add_node("test", |s: TestState, _config: &Config| async move { Ok(s) });

        let result = graph.compile(None);
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.to_string().contains("entry point"));
        }
    }
}
