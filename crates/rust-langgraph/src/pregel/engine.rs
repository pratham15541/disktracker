//! Core Pregel execution engine.
//!
//! Channel-centric superstep execution inspired by Google’s Pregel: nodes run when
//! triggered, writes flow through channels with merge semantics, and checkpoints
//! persist progress between steps.

use crate::channels::BaseChannel;
use crate::checkpoint::{BaseCheckpointSaver, Checkpoint, CheckpointMetadata, StateSnapshot};
use crate::config::Config;
use crate::errors::{Error, Result};
use crate::graph::START;
use crate::nodes::PregelNode;
use crate::state::State;
use crate::types::{StreamEvent, StreamMode};
use futures::stream::Stream;
use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::Arc;

/// The Pregel execution engine.
///
/// This implements the core superstep loop:
/// 1. Load checkpoint (if resuming)
/// 2. Write initial input to channels
/// 3. Loop:
///    - Find triggered nodes
///    - Execute nodes in parallel
///    - Apply writes to channels with reducers
///    - Handle interrupts/commands
///    - Save checkpoint
///    - Yield stream events
/// 4. Return final state
pub struct Pregel<S: State> {
    /// The nodes in the graph
    nodes: HashMap<String, PregelNode<S>>,

    /// Channels holding graph state between supersteps
    channels: HashMap<String, Box<dyn BaseChannel>>,

    /// Optional checkpoint saver for persistence
    checkpointer: Option<Arc<dyn BaseCheckpointSaver>>,

    /// Entry point node name
    entry_point: String,

    /// Finish point node names
    finish_points: HashSet<String>,

    /// Static edges from source to targets
    edges: HashMap<String, Vec<String>>,

    /// Conditional edges branch functions
    conditional_edges: HashMap<String, Box<dyn crate::pregel::Branch<S>>>,

    /// Current step in execution
    current_step: usize,

    /// Maximum recursion depth
    recursion_limit: usize,

    /// Channels written in the current superstep
    written_channels: HashSet<String>,
}

impl<S: State> Pregel<S> {
    /// Create a new Pregel executor
    pub fn new(
        nodes: HashMap<String, PregelNode<S>>,
        channels: HashMap<String, Box<dyn BaseChannel>>,
        checkpointer: Option<Arc<dyn BaseCheckpointSaver>>,
        entry_point: String,
        finish_points: HashSet<String>,
        edges: HashMap<String, Vec<String>>,
        conditional_edges: HashMap<String, Box<dyn crate::pregel::Branch<S>>>,
    ) -> Self {
        Self {
            nodes,
            channels,
            checkpointer,
            entry_point,
            finish_points,
            edges,
            conditional_edges,
            current_step: 0,
            recursion_limit: 25,
            written_channels: HashSet::new(),
        }
    }

    /// Set the recursion limit
    pub fn with_recursion_limit(mut self, limit: usize) -> Self {
        self.recursion_limit = limit;
        self
    }

    /// Execute the graph with the given input and configuration
    pub async fn invoke(&mut self, input: S, config: Config) -> Result<S> {
        self.recursion_limit = config.recursion_limit;
        self.current_step = 0;

        // 1. Load checkpoint if resuming
        if let Some(checkpointer) = &self.checkpointer {
            if let Some(tuple) = checkpointer.get_tuple(&config).await? {
                self.restore_channels(&tuple.checkpoint)?;
                self.current_step = tuple.metadata.step;
            }
        }

        // 2. Write initial input to START channels
        self.write_input_to_channels(&input)?;

        // 3. Superstep loop
        loop {
            // Check recursion limit
            if self.current_step >= self.recursion_limit {
                return Err(Error::RecursionLimitError {
                    current: self.current_step,
                    limit: self.recursion_limit,
                });
            }

            // Find triggered nodes
            let triggered_nodes = self.find_triggered_nodes();
            if triggered_nodes.is_empty() {
                break; // No more work to do
            }

            // Execute nodes in parallel
            let mut tasks = Vec::new();
            for node_name in &triggered_nodes {
                if let Some(node) = self.nodes.get(node_name) {
                    let state = self.read_state_for_node(node)?;
                    let node_clone = node.clone();
                    let config_clone = config.clone();

                    let task = tokio::spawn(async move {
                        node_clone.bound.invoke(state, &config_clone).await
                    });

                    tasks.push((node_name.clone(), task));
                }
            }

            // Collect results
            let mut updates: HashMap<String, S> = HashMap::new();
            for (node_name, task) in tasks {
                match task.await {
                    Ok(Ok(result)) => {
                        updates.insert(node_name, result);
                    }
                    Ok(Err(e)) => return Err(e),
                    Err(e) => {
                        return Err(Error::execution(format!("Node execution panicked: {}", e)))
                    }
                }
            }

            // Apply writes to channels and collect which channels were written this superstep.
            // Those channels trigger the next wave of nodes (do not clear — replace).
            let mut written = self.apply_updates(updates.clone())?;

            // Evaluate conditional edges for completed nodes
            for (node_name, state) in updates {
                if let Some(branch) = self.conditional_edges.get(&node_name) {
                    let branch_result = branch.route(&state).await?;
                    if !branch_result.is_end() {
                        for target in branch_result.node_names() {
                            let input_ch = format!("{}_input", target);
                            if let Some(ch) = self.channels.get_mut(&input_ch) {
                                let val = state.to_value()?;
                                ch.update(vec![val])?;
                                written.insert(input_ch);
                            }
                        }
                    }
                }
            }

            self.written_channels = written;

            // Check for interrupts/commands
            // (For now, we'll implement basic interrupt support later)

            // Save checkpoint
            if let Some(checkpointer) = &self.checkpointer {
                let checkpoint = self.create_checkpoint(&config)?;
                let metadata = CheckpointMetadata {
                    step: self.current_step,
                    source: "pregel".to_string(),
                    created_at: chrono::Utc::now(),
                    extra: HashMap::new(),
                };
                checkpointer.put(&checkpoint, &metadata, &config).await?;
            }

            self.current_step += 1;
        }

        // 4. Return final state
        self.get_final_state()
    }

    /// Stream execution events
    pub async fn stream(
        &mut self,
        input: S,
        config: Config,
        mode: StreamMode,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + std::marker::Send>>> {
        self.recursion_limit = config.recursion_limit;
        self.current_step = 0;

        // For MVP: implement a basic streaming version
        // Full implementation would yield events at each step

        let (tx, rx) = tokio::sync::mpsc::channel(100);

        // Load checkpoint if resuming
        if let Some(checkpointer) = &self.checkpointer {
            if let Some(tuple) = checkpointer.get_tuple(&config).await? {
                self.restore_channels(&tuple.checkpoint)?;
                self.current_step = tuple.metadata.step;
            }
        }

        // Write initial input
        self.write_input_to_channels(&input)?;

        // Clone necessary data for the async task
        let _nodes = self.nodes.clone();
        let _channels: HashMap<String, Box<dyn BaseChannel>> = HashMap::new();
        // Note: We can't clone channels easily due to trait object limitations
        // For now, we'll implement a simpler version

        // Execute and stream
        let _checkpointer = self.checkpointer.clone();
        let _entry_point = self.entry_point.clone();
        let recursion_limit = self.recursion_limit;

        tokio::spawn(async move {
            let step = 0;
            loop {
                if step >= recursion_limit {
                    let _ = tx.send(Err(Error::RecursionLimitError {
                        current: step,
                        limit: recursion_limit,
                    })).await;
                    break;
                }

                // For now, simplified streaming
                // Full implementation would mirror invoke() but yield events

                // Emit a values event
                if matches!(mode, StreamMode::Values) {
                    let event = StreamEvent::Values {
                        ns: vec![],
                        data: serde_json::json!({"step": step}),
                        interrupts: vec![],
                    };
                    if tx.send(Ok(event)).await.is_err() {
                        break;
                    }
                }

                break; // For MVP, just one step
            }
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    /// Get the current state snapshot
    pub async fn get_state(&self, config: &Config) -> Result<Option<StateSnapshot<S>>> {
        if let Some(checkpointer) = &self.checkpointer {
            if let Some(tuple) = checkpointer.get_tuple(config).await? {
                let state = self.state_from_checkpoint(&tuple.checkpoint)?;
                return Ok(Some(StateSnapshot {
                    state,
                    checkpoint: tuple.checkpoint,
                    metadata: tuple.metadata,
                    config: tuple.config,
                }));
            }
        }
        Ok(None)
    }

    /// Get state history
    pub async fn get_state_history(
        &self,
        config: &Config,
        limit: Option<usize>,
    ) -> Result<Vec<StateSnapshot<S>>> {
        if let Some(checkpointer) = &self.checkpointer {
            let tuples = checkpointer.list(config, limit).await?;
            let mut snapshots = Vec::new();

            for tuple in tuples {
                let state = self.state_from_checkpoint(&tuple.checkpoint)?;
                snapshots.push(StateSnapshot {
                    state,
                    checkpoint: tuple.checkpoint,
                    metadata: tuple.metadata,
                    config: tuple.config,
                });
            }

            return Ok(snapshots);
        }
        Ok(Vec::new())
    }

    // === PRIVATE HELPER METHODS ===

    /// Write input state to START channels
    fn write_input_to_channels(&mut self, input: &S) -> Result<()> {
        // Convert state to JSON and write to START channel
        let value = input.to_value()?;
        if let Some(channel) = self.channels.get_mut("__start__") {
            channel.update(vec![value])?;
            self.written_channels.insert("__start__".to_string());
        }
        Ok(())
    }

    /// Find nodes that are triggered by written channels
    fn find_triggered_nodes(&self) -> Vec<String> {
        let mut triggered = Vec::new();

        for (name, node) in &self.nodes {
            if node.is_triggered(&self.written_channels.iter().cloned().collect::<Vec<_>>()) {
                triggered.push(name.clone());
            }
        }

        // If no nodes triggered but we have entry point and it's first step
        if triggered.is_empty() && self.current_step == 0 {
            triggered.push(self.entry_point.clone());
        }

        triggered
    }

    /// Read state for a specific node from its input channels (`{name}_input`), merged in order.
    /// If those channels are empty but this node is triggered by `__start__`, read the graph input from `__start__`.
    fn read_state_for_node(&self, node: &PregelNode<S>) -> Result<S> {
        let mut merged: Option<S> = None;

        for ch_name in &node.channels {
            if let Some(channel) = self.channels.get(ch_name) {
                if let Some(value) = channel.get()? {
                    let piece = S::from_value(value)?;
                    merged = match merged {
                        None => Some(piece),
                        Some(mut m) => {
                            m.merge(piece)?;
                            Some(m)
                        }
                    };
                }
            }
        }

        if merged.is_none() && node.triggers.iter().any(|t| t == START) {
            if let Some(channel) = self.channels.get(START) {
                if let Some(value) = channel.get()? {
                    merged = Some(S::from_value(value)?);
                }
            }
        }

        merged.ok_or_else(|| {
            Error::state(format!(
                "Cannot construct state for node '{}' (input channels {:?})",
                node.name, node.channels
            ))
        })
    }

    /// Apply node outputs to `{node}_output` and fan out the same state to each edge target's `{target}_input`.
    /// Returns the set of channel names written this superstep — these trigger the next superstep (replacing the previous set).
    fn apply_updates(&mut self, updates: HashMap<String, S>) -> Result<HashSet<String>> {
        let mut next_triggers = HashSet::new();

        for (node_name, state) in updates {
            let value = state.to_value()?;

            if let Some(node) = self.nodes.get(&node_name) {
                for writer in &node.writers {
                    if let Some(channel) = self.channels.get_mut(&writer.channel) {
                        channel.update(vec![value.clone()])?;
                        next_triggers.insert(writer.channel.clone());
                    }
                }
            }

            if let Some(targets) = self.edges.get(&node_name) {
                for target in targets {
                    let input_ch = format!("{}_input", target);
                    if let Some(ch) = self.channels.get_mut(&input_ch) {
                        ch.update(vec![value.clone()])?;
                    }
                }
            }
        }

        Ok(next_triggers)
    }

    /// Create a checkpoint from current channel states
    fn create_checkpoint(&self, config: &Config) -> Result<Checkpoint> {
        let mut checkpoint = Checkpoint::new();

        if let Some(thread_id) = &config.thread_id {
            checkpoint.thread_id = Some(thread_id.clone());
        }

        // Save all channel values
        for (name, channel) in &self.channels {
            let channel_data = channel.checkpoint()?;
            checkpoint.set_channel(name, channel_data);
        }

        Ok(checkpoint)
    }

    /// Restore channels from a checkpoint
    fn restore_channels(&mut self, checkpoint: &Checkpoint) -> Result<()> {
        for (name, value) in &checkpoint.channel_values {
            // We can't fully restore channels from checkpoint due to type erasure
            // In practice, the graph builder creates channels and we just update their values
            if let Some(channel) = self.channels.get_mut(name) {
                channel.update(vec![value.clone()])?;
            }
        }
        Ok(())
    }

    /// Construct state from a checkpoint
    fn state_from_checkpoint(&self, checkpoint: &Checkpoint) -> Result<S> {
        // Get the value from the main state channel
        if let Some(value) = checkpoint.get_channel("__state__") {
            return S::from_value(value.clone());
        }

        // Fallback: try to construct from START channel
        if let Some(value) = checkpoint.get_channel("__start__") {
            return S::from_value(value.clone());
        }

        Err(Error::checkpoint("Cannot construct state from checkpoint"))
    }

    /// Get the final state after execution.
    ///
    /// Prefer `__end__`, then each finish node's `{name}_output` (where compiled graphs write),
    /// then `__start__` as last resort.
    fn get_final_state(&self) -> Result<S> {
        if let Some(channel) = self.channels.get(crate::graph::END) {
            if let Some(value) = channel.get()? {
                return S::from_value(value);
            }
        }

        for fp in &self.finish_points {
            let ch_name = format!("{}_output", fp);
            if let Some(channel) = self.channels.get(&ch_name) {
                if let Some(value) = channel.get()? {
                    return S::from_value(value);
                }
            }
        }

        if let Some(channel) = self.channels.get(START) {
            if let Some(value) = channel.get()? {
                return S::from_value(value);
            }
        }

        Err(Error::state("Cannot determine final state"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::{LastValue};
    use crate::nodes::{PregelNode, ChannelWrite};
    use crate::state::State as StateTrait;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
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
    async fn test_pregel_basic() {
        let increment_node = PregelNode::from_node(
            "increment",
            vec!["__start__".to_string()],
            vec!["__start__".to_string()],
            |mut state: TestState, _config: &Config| async move {
                state.count += 1;
                Ok(state)
            },
            vec![ChannelWrite::new("__end__")],
        );

        let mut nodes = HashMap::new();
        nodes.insert("increment".to_string(), increment_node);

        let mut channels: HashMap<String, Box<dyn BaseChannel>> = HashMap::new();
        channels.insert("__start__".to_string(), Box::new(LastValue::<TestState>::new()));
        channels.insert("__end__".to_string(), Box::new(LastValue::<TestState>::new()));

        let mut pregel = Pregel::new(
            nodes,
            channels,
            None,
            "increment".to_string(),
            HashSet::from(["increment".to_string()]),
            HashMap::new(),
            HashMap::new(),
        );

        let input = TestState { count: 0 };
        let result = pregel.invoke(input, Config::default()).await.unwrap();

        assert_eq!(result.count, 1);
    }

    /// Two-node chain: second superstep must run (triggers from previous `{src}_output`), and
    /// downstream reads merged state from `{dst}_input` (not only `__start__`).
    #[tokio::test]
    async fn test_pregel_two_node_chain() {
        let a = PregelNode::from_node(
            "a",
            vec!["a_input".to_string()],
            vec![START.to_string()],
            |mut state: TestState, _config: &Config| async move {
                state.count += 1;
                Ok(state)
            },
            vec![ChannelWrite::new("a_output")],
        );
        let b = PregelNode::from_node(
            "b",
            vec!["b_input".to_string()],
            vec!["a_output".to_string()],
            |mut state: TestState, _config: &Config| async move {
                state.count *= 10;
                Ok(state)
            },
            vec![ChannelWrite::new("b_output")],
        );

        let mut nodes = HashMap::new();
        nodes.insert("a".to_string(), a);
        nodes.insert("b".to_string(), b);

        let mut channels: HashMap<String, Box<dyn BaseChannel>> = HashMap::new();
        channels.insert(START.to_string(), Box::new(LastValue::<TestState>::new()));
        channels.insert("a_input".to_string(), Box::new(LastValue::<TestState>::new()));
        channels.insert("a_output".to_string(), Box::new(LastValue::<TestState>::new()));
        channels.insert("b_input".to_string(), Box::new(LastValue::<TestState>::new()));
        channels.insert("b_output".to_string(), Box::new(LastValue::<TestState>::new()));
        channels.insert("__end__".to_string(), Box::new(LastValue::<TestState>::new()));

        let mut edges = HashMap::new();
        edges.insert("a".to_string(), vec!["b".to_string()]);

        let mut pregel = Pregel::new(
            nodes,
            channels,
            None,
            "a".to_string(),
            HashSet::from(["b".to_string()]),
            edges,
            HashMap::new(),
        );

        let result = pregel
            .invoke(TestState { count: 5 }, Config::default())
            .await
            .unwrap();
        assert_eq!(result.count, 60); // (5 + 1) * 10
    }
}
