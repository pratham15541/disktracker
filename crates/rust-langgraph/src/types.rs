//! Core types for LangGraph execution.
//!
//! This module defines the fundamental types used throughout LangGraph,
//! including streaming modes, commands, and control flow primitives.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A command to send work to a specific node with custom input.
///
/// `Send` enables dynamic routing where the graph can decide at runtime
/// to invoke specific nodes with specific inputs, enabling map-reduce patterns.
///
/// # Example
///
/// ```rust
/// use rust_langgraph::types::Send;
/// use serde_json::json;
///
/// let send = Send::new("process_item", json!({"item_id": 42}));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Send {
    /// The name of the node to send to
    pub node: String,
    /// The input to send to the node
    pub arg: serde_json::Value,
}

impl Send {
    /// Create a new Send command
    pub fn new(node: impl Into<String>, arg: serde_json::Value) -> Self {
        Self {
            node: node.into(),
            arg,
        }
    }
}

/// A command that can be returned from a node to control execution flow.
///
/// Commands provide a way for nodes to influence graph execution beyond
/// just updating state. They can update state, change routing, or control
/// interrupts.
///
/// # Example
///
/// ```rust
/// use rust_langgraph::types::Command;
/// use serde_json::json;
///
/// // Update state and go to specific nodes
/// let cmd = Command::new()
///     .with_update("key", json!("value"))
///     .with_goto(vec!["node1", "node2"]);
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Command {
    /// State updates to apply
    pub update: Option<HashMap<String, serde_json::Value>>,
    /// Specific nodes to route to (overrides normal routing)
    pub goto: Option<Vec<String>>,
    /// Value to resume with after interrupt
    pub resume: Option<serde_json::Value>,
}

impl Command {
    /// Create a new empty command
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a state update
    pub fn with_update(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.update
            .get_or_insert_with(HashMap::new)
            .insert(key.into(), value);
        self
    }

    /// Set the goto nodes
    pub fn with_goto(mut self, nodes: Vec<impl Into<String>>) -> Self {
        self.goto = Some(nodes.into_iter().map(|n| n.into()).collect());
        self
    }

    /// Set the resume value
    pub fn with_resume(mut self, value: serde_json::Value) -> Self {
        self.resume = Some(value);
        self
    }
}

/// Streaming mode for graph execution.
///
/// Different streaming modes provide different levels of granularity
/// for observing graph execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamMode {
    /// Stream the full state after each step
    Values,
    /// Stream only the updates (deltas) to state
    Updates,
    /// Stream checkpoint information after each step
    Checkpoints,
    /// Stream task execution details
    Tasks,
    /// Stream detailed debug information
    Debug,
    /// Stream messages (useful for chat applications)
    Messages,
    /// Custom streaming mode
    Custom,
}

impl Default for StreamMode {
    fn default() -> Self {
        StreamMode::Values
    }
}

/// An event emitted during graph streaming execution.
///
/// Stream events provide visibility into graph execution, allowing
/// applications to react to intermediate states, debug issues, or
/// provide real-time feedback.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum StreamEvent {
    /// The full state values at a point in execution
    Values {
        /// Namespace (for subgraphs)
        ns: Vec<String>,
        /// The state data
        data: serde_json::Value,
        /// Any interrupts that occurred
        interrupts: Vec<Interrupt>,
    },

    /// State updates (deltas) from a step
    Updates {
        /// Namespace
        ns: Vec<String>,
        /// The update data
        data: serde_json::Value,
        /// Node that produced the update
        node: String,
    },

    /// Checkpoint saved
    Checkpoint {
        /// Namespace
        ns: Vec<String>,
        /// Checkpoint ID
        checkpoint_id: String,
        /// Step number
        step: usize,
    },

    /// Task execution started
    TaskStart {
        /// Task ID
        task_id: String,
        /// Node name
        node: String,
    },

    /// Task execution completed
    TaskEnd {
        /// Task ID
        task_id: String,
        /// Node name
        node: String,
        /// Result data
        result: serde_json::Value,
    },

    /// Debug information
    Debug {
        /// Debug message
        message: String,
        /// Additional context
        context: HashMap<String, serde_json::Value>,
    },

    /// A message (for chat applications)
    Message {
        /// The message content
        content: String,
        /// Message metadata
        metadata: HashMap<String, serde_json::Value>,
    },

    /// Custom event
    Custom {
        /// Event type
        event_type: String,
        /// Event data
        data: serde_json::Value,
    },
}

/// An interrupt that occurred during execution.
///
/// Interrupts pause graph execution and can be used for human-in-the-loop
/// patterns or to handle errors gracefully.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interrupt {
    /// The value that triggered the interrupt
    pub value: serde_json::Value,
    /// When the interrupt occurred
    pub when: InterruptType,
    /// The node where the interrupt occurred
    pub node: Option<String>,
}

/// Type of interrupt
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InterruptType {
    /// Interrupt before node execution
    Before,
    /// Interrupt during node execution
    During,
    /// Interrupt after node execution
    After,
}

/// Retry policy for node execution.
///
/// Defines how and when to retry failed node executions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts
    pub max_attempts: usize,
    /// Initial delay between retries (in milliseconds)
    pub initial_delay_ms: u64,
    /// Maximum delay between retries (in milliseconds)
    pub max_delay_ms: u64,
    /// Multiplier for exponential backoff
    pub backoff_multiplier: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_delay_ms: 100,
            max_delay_ms: 10_000,
            backoff_multiplier: 2.0,
        }
    }
}

impl RetryPolicy {
    /// Create a new retry policy with the given max attempts
    pub fn new(max_attempts: usize) -> Self {
        Self {
            max_attempts,
            ..Default::default()
        }
    }

    /// Calculate delay for a given attempt (0-indexed)
    pub fn delay_for_attempt(&self, attempt: usize) -> u64 {
        let delay = self.initial_delay_ms as f64 * self.backoff_multiplier.powi(attempt as i32);
        delay.min(self.max_delay_ms as f64) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_send_creation() {
        let send = Send::new("test_node", serde_json::json!({"key": "value"}));
        assert_eq!(send.node, "test_node");
        assert!(send.arg.is_object());
    }

    #[test]
    fn test_command_builder() {
        let cmd = Command::new()
            .with_update("key1", serde_json::json!("val1"))
            .with_update("key2", serde_json::json!(42))
            .with_goto(vec!["node1", "node2"]);

        assert_eq!(cmd.update.as_ref().unwrap().len(), 2);
        assert_eq!(cmd.goto.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn test_retry_policy_delay() {
        let policy = RetryPolicy::default();
        
        assert_eq!(policy.delay_for_attempt(0), 100);
        assert_eq!(policy.delay_for_attempt(1), 200);
        assert_eq!(policy.delay_for_attempt(2), 400);
        
        // Should cap at max_delay_ms
        assert_eq!(policy.delay_for_attempt(10), 10_000);
    }

    #[test]
    fn test_stream_mode_serialization() {
        let mode = StreamMode::Values;
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, "\"values\"");
        
        let deserialized: StreamMode = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, StreamMode::Values);
    }
}
