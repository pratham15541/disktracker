//! Checkpoint system for graph state persistence.
//!
//! Checkpoints allow graphs to save and restore execution state,
//! enabling features like pause/resume, time travel, and crash recovery.

use crate::config::Config;
use crate::errors::{Error, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// A checkpoint representing the state of a graph at a point in time.
///
/// Checkpoints are compatible with the Python LangGraph wire format,
/// allowing interoperability between Rust and Python implementations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Checkpoint format version
    pub v: i32,

    /// Unique checkpoint identifier
    pub id: String,

    /// Timestamp when checkpoint was created
    pub ts: String,

    /// The values of all channels at this checkpoint
    pub channel_values: HashMap<String, serde_json::Value>,

    /// Version numbers for each channel
    pub channel_versions: HashMap<String, i32>,

    /// Versions seen by each channel (for tracking updates)
    pub versions_seen: HashMap<String, HashMap<String, i32>>,

    /// Thread ID this checkpoint belongs to
    pub thread_id: Option<String>,

    /// Parent checkpoint ID (for nested graphs/subgraphs)
    pub parent_id: Option<String>,
}

impl Checkpoint {
    /// Create a new empty checkpoint
    pub fn new() -> Self {
        Self {
            v: 1,
            id: Uuid::new_v4().to_string(),
            ts: Utc::now().to_rfc3339(),
            channel_values: HashMap::new(),
            channel_versions: HashMap::new(),
            versions_seen: HashMap::new(),
            thread_id: None,
            parent_id: None,
        }
    }

    /// Create a checkpoint with a specific thread ID
    pub fn with_thread_id(mut self, thread_id: impl Into<String>) -> Self {
        self.thread_id = Some(thread_id.into());
        self
    }

    /// Set a channel value
    pub fn set_channel(&mut self, name: impl Into<String>, value: serde_json::Value) {
        let name = name.into();
        let version = self.channel_versions.get(&name).copied().unwrap_or(0) + 1;
        self.channel_values.insert(name.clone(), value);
        self.channel_versions.insert(name, version);
    }

    /// Get a channel value
    pub fn get_channel(&self, name: &str) -> Option<&serde_json::Value> {
        self.channel_values.get(name)
    }
}

impl Default for Checkpoint {
    fn default() -> Self {
        Self::new()
    }
}

/// A checkpoint along with metadata about when and where it was saved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointTuple {
    /// The checkpoint itself
    pub checkpoint: Checkpoint,

    /// Metadata about the checkpoint
    pub metadata: CheckpointMetadata,

    /// The configuration used for this checkpoint
    pub config: Config,

    /// Parent checkpoint tuple (for nested graphs)
    pub parent: Option<Box<CheckpointTuple>>,
}

/// Metadata about a checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointMetadata {
    /// When the checkpoint was created
    pub created_at: DateTime<Utc>,

    /// Step number when checkpoint was created
    pub step: usize,

    /// Source of the checkpoint (e.g., "pregel", "user")
    pub source: String,

    /// Additional custom metadata
    pub extra: HashMap<String, serde_json::Value>,
}

impl Default for CheckpointMetadata {
    fn default() -> Self {
        Self {
            created_at: Utc::now(),
            step: 0,
            source: "unknown".to_string(),
            extra: HashMap::new(),
        }
    }
}

/// A snapshot of graph state at a specific checkpoint.
///
/// This includes both the checkpoint data and the deserialized state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSnapshot<S> {
    /// The state at this snapshot
    pub state: S,

    /// The checkpoint
    pub checkpoint: Checkpoint,

    /// Metadata
    pub metadata: CheckpointMetadata,

    /// Configuration
    pub config: Config,
}

/// Trait for checkpoint storage backends.
///
/// Implementations of this trait provide persistent storage for checkpoints,
/// enabling save/resume functionality across process restarts.
#[async_trait]
pub trait BaseCheckpointSaver: Send + Sync {
    /// Get a checkpoint tuple for the given configuration.
    ///
    /// If `config.checkpoint_id` is set, returns that specific checkpoint.
    /// Otherwise, returns the latest checkpoint for the thread.
    async fn get_tuple(&self, config: &Config) -> Result<Option<CheckpointTuple>>;

    /// Save a checkpoint.
    ///
    /// Returns an updated Config with the checkpoint ID set.
    async fn put(
        &self,
        checkpoint: &Checkpoint,
        metadata: &CheckpointMetadata,
        config: &Config,
    ) -> Result<Config>;

    /// List checkpoints for a given configuration.
    ///
    /// Returns checkpoints in reverse chronological order.
    async fn list(&self, config: &Config, limit: Option<usize>) -> Result<Vec<CheckpointTuple>>;

    /// Get a specific checkpoint by ID
    async fn get(&self, checkpoint_id: &str) -> Result<Option<Checkpoint>> {
        // Default implementation uses get_tuple
        let config = Config::new().with_checkpoint_id(checkpoint_id);
        Ok(self.get_tuple(&config).await?.map(|t| t.checkpoint))
    }

    /// Delete all checkpoints for a thread
    async fn delete_thread(&self, thread_id: &str) -> Result<()> {
        // Default implementation returns not implemented error
        Err(Error::checkpoint(format!(
            "delete_thread not implemented for thread {}",
            thread_id
        )))
    }

    /// Prune old checkpoints, keeping only the most recent ones
    async fn prune(&self, thread_id: &str, keep: usize) -> Result<usize> {
        // Default implementation returns not implemented error
        let _ = (thread_id, keep);
        Err(Error::checkpoint("prune not implemented"))
    }
}

/// Type alias for boxed checkpoint savers
pub type CheckpointSaverBox = Box<dyn BaseCheckpointSaver>;

/// Type alias for Arc'd checkpoint savers
pub type CheckpointSaverArc = std::sync::Arc<dyn BaseCheckpointSaver>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checkpoint_creation() {
        let checkpoint = Checkpoint::new();
        assert_eq!(checkpoint.v, 1);
        assert!(!checkpoint.id.is_empty());
        assert!(checkpoint.channel_values.is_empty());
    }

    #[test]
    fn test_checkpoint_with_thread_id() {
        let checkpoint = Checkpoint::new().with_thread_id("thread-123");
        assert_eq!(checkpoint.thread_id.as_deref(), Some("thread-123"));
    }

    #[test]
    fn test_checkpoint_set_get_channel() {
        let mut checkpoint = Checkpoint::new();
        checkpoint.set_channel("my_channel", serde_json::json!({"value": 42}));

        let value = checkpoint.get_channel("my_channel").unwrap();
        assert_eq!(value, &serde_json::json!({"value": 42}));

        let version = checkpoint.channel_versions.get("my_channel").unwrap();
        assert_eq!(*version, 1);

        // Update the same channel
        checkpoint.set_channel("my_channel", serde_json::json!({"value": 43}));
        let version = checkpoint.channel_versions.get("my_channel").unwrap();
        assert_eq!(*version, 2);
    }

    #[test]
    fn test_checkpoint_serialization() {
        let mut checkpoint = Checkpoint::new().with_thread_id("test");
        checkpoint.set_channel("count", serde_json::json!(5));

        let json = serde_json::to_string(&checkpoint).unwrap();
        let deserialized: Checkpoint = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.thread_id, checkpoint.thread_id);
        assert_eq!(
            deserialized.get_channel("count"),
            checkpoint.get_channel("count")
        );
    }

    #[test]
    fn test_checkpoint_metadata() {
        let metadata = CheckpointMetadata {
            step: 5,
            source: "test".to_string(),
            ..Default::default()
        };

        assert_eq!(metadata.step, 5);
        assert_eq!(metadata.source, "test");
    }
}
