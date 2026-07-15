//! In-memory checkpoint backend.
//!
//! A simple checkpoint saver that stores checkpoints in memory.
//! Useful for development and testing, but checkpoints are lost
//! when the process exits.

use crate::checkpoint::{BaseCheckpointSaver, Checkpoint, CheckpointMetadata, CheckpointTuple};
use crate::config::Config;
use crate::errors::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// In-memory checkpoint storage.
///
/// Stores checkpoints in a HashMap in memory. Fast and simple,
/// but does not persist across process restarts.
///
/// # Example
///
/// ```rust
/// use rust_langgraph::checkpoint_backends::memory::MemorySaver;
/// use rust_langgraph::checkpoint::BaseCheckpointSaver;
/// use rust_langgraph::Config;
///
/// #[tokio::main]
/// async fn main() {
///     let saver = MemorySaver::new();
///     let config = Config::new().with_thread_id("test-123");
///     
///     // Use saver with graph...
/// }
/// ```
#[derive(Debug, Clone)]
pub struct MemorySaver {
    storage: Arc<RwLock<MemoryStorage>>,
}

#[derive(Debug, Default)]
struct MemoryStorage {
    // Map from thread_id -> list of checkpoint tuples (oldest to newest)
    threads: HashMap<String, Vec<CheckpointTuple>>,
    // Map from checkpoint_id -> checkpoint tuple
    by_id: HashMap<String, CheckpointTuple>,
}

impl MemorySaver {
    /// Create a new in-memory checkpoint saver
    pub fn new() -> Self {
        Self {
            storage: Arc::new(RwLock::new(MemoryStorage::default())),
        }
    }

    /// Get the number of checkpoints stored
    pub async fn len(&self) -> usize {
        let storage = self.storage.read().await;
        storage.by_id.len()
    }

    /// Check if storage is empty
    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }

    /// Clear all stored checkpoints
    pub async fn clear(&self) {
        let mut storage = self.storage.write().await;
        storage.threads.clear();
        storage.by_id.clear();
    }
}

impl Default for MemorySaver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BaseCheckpointSaver for MemorySaver {
    async fn get_tuple(&self, config: &Config) -> Result<Option<CheckpointTuple>> {
        let storage = self.storage.read().await;

        // If a specific checkpoint ID is requested
        if let Some(checkpoint_id) = &config.checkpoint_id {
            return Ok(storage.by_id.get(checkpoint_id).cloned());
        }

        // Otherwise, return the latest checkpoint for the thread
        if let Some(thread_id) = &config.thread_id {
            if let Some(tuples) = storage.threads.get(thread_id) {
                return Ok(tuples.last().cloned());
            }
        }

        Ok(None)
    }

    async fn put(
        &self,
        checkpoint: &Checkpoint,
        metadata: &CheckpointMetadata,
        config: &Config,
    ) -> Result<Config> {
        let mut storage = self.storage.write().await;

        let thread_id = checkpoint
            .thread_id
            .clone()
            .or_else(|| config.thread_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let tuple = CheckpointTuple {
            checkpoint: checkpoint.clone(),
            metadata: metadata.clone(),
            config: config.clone(),
            parent: None,
        };

        // Store by ID
        storage
            .by_id
            .insert(checkpoint.id.clone(), tuple.clone());

        // Store in thread history
        storage
            .threads
            .entry(thread_id.clone())
            .or_default()
            .push(tuple);

        // Return config with checkpoint ID set
        Ok(config.clone().with_checkpoint_id(&checkpoint.id))
    }

    async fn list(&self, config: &Config, limit: Option<usize>) -> Result<Vec<CheckpointTuple>> {
        let storage = self.storage.read().await;

        if let Some(thread_id) = &config.thread_id {
            if let Some(tuples) = storage.threads.get(thread_id) {
                let mut result: Vec<_> = tuples.iter().rev().cloned().collect();

                if let Some(limit) = limit {
                    result.truncate(limit);
                }

                return Ok(result);
            }
        }

        Ok(Vec::new())
    }

    async fn get(&self, checkpoint_id: &str) -> Result<Option<Checkpoint>> {
        let storage = self.storage.read().await;
        Ok(storage.by_id.get(checkpoint_id).map(|t| t.checkpoint.clone()))
    }

    async fn delete_thread(&self, thread_id: &str) -> Result<()> {
        let mut storage = self.storage.write().await;

        // Remove from thread list
        if let Some(tuples) = storage.threads.remove(thread_id) {
            // Remove all checkpoint IDs from by_id map
            for tuple in tuples {
                storage.by_id.remove(&tuple.checkpoint.id);
            }
        }

        Ok(())
    }

    async fn prune(&self, thread_id: &str, keep: usize) -> Result<usize> {
        let mut storage = self.storage.write().await;

        if let Some(tuples) = storage.threads.get_mut(thread_id) {
            if tuples.len() <= keep {
                return Ok(0);
            }

            let to_remove = tuples.len() - keep;
            let removed: Vec<_> = tuples.drain(0..to_remove).collect();

            // Remove from by_id map
            for tuple in &removed {
                storage.by_id.remove(&tuple.checkpoint.id);
            }

            Ok(removed.len())
        } else {
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[tokio::test]
    async fn test_memory_saver_basic() {
        let saver = MemorySaver::new();
        assert_eq!(saver.len().await, 0);
        assert!(saver.is_empty().await);

        let mut checkpoint = Checkpoint::new().with_thread_id("test-thread");
        checkpoint.set_channel("count", serde_json::json!(5));

        let metadata = CheckpointMetadata {
            created_at: Utc::now(),
            step: 1,
            source: "test".to_string(),
            extra: HashMap::new(),
        };

        let config = Config::new().with_thread_id("test-thread");
        let updated_config = saver.put(&checkpoint, &metadata, &config).await.unwrap();

        assert!(updated_config.checkpoint_id.is_some());
        assert_eq!(saver.len().await, 1);
    }

    #[tokio::test]
    async fn test_memory_saver_get_tuple() {
        let saver = MemorySaver::new();

        let checkpoint = Checkpoint::new().with_thread_id("thread-1");
        let metadata = CheckpointMetadata::default();
        let config = Config::new().with_thread_id("thread-1");

        saver.put(&checkpoint, &metadata, &config).await.unwrap();

        // Get latest for thread
        let tuple = saver.get_tuple(&config).await.unwrap();
        assert!(tuple.is_some());
        assert_eq!(tuple.unwrap().checkpoint.id, checkpoint.id);

        // Get by checkpoint ID
        let config_with_id = Config::new().with_checkpoint_id(&checkpoint.id);
        let tuple = saver.get_tuple(&config_with_id).await.unwrap();
        assert!(tuple.is_some());
    }

    #[tokio::test]
    async fn test_memory_saver_list() {
        let saver = MemorySaver::new();
        let config = Config::new().with_thread_id("thread-1");

        // Add multiple checkpoints
        for i in 0..5 {
            let mut checkpoint = Checkpoint::new().with_thread_id("thread-1");
            checkpoint.set_channel("step", serde_json::json!(i));
            let metadata = CheckpointMetadata {
                step: i,
                ..Default::default()
            };
            saver.put(&checkpoint, &metadata, &config).await.unwrap();
        }

        // List all
        let list = saver.list(&config, None).await.unwrap();
        assert_eq!(list.len(), 5);

        // Checkpoints should be in reverse order (newest first)
        assert_eq!(list[0].metadata.step, 4);
        assert_eq!(list[4].metadata.step, 0);

        // List with limit
        let list = saver.list(&config, Some(2)).await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].metadata.step, 4);
        assert_eq!(list[1].metadata.step, 3);
    }

    #[tokio::test]
    async fn test_memory_saver_delete_thread() {
        let saver = MemorySaver::new();

        let checkpoint1 = Checkpoint::new().with_thread_id("thread-1");
        let checkpoint2 = Checkpoint::new().with_thread_id("thread-2");
        let metadata = CheckpointMetadata::default();

        saver
            .put(
                &checkpoint1,
                &metadata,
                &Config::new().with_thread_id("thread-1"),
            )
            .await
            .unwrap();
        saver
            .put(
                &checkpoint2,
                &metadata,
                &Config::new().with_thread_id("thread-2"),
            )
            .await
            .unwrap();

        assert_eq!(saver.len().await, 2);

        // Delete thread-1
        saver.delete_thread("thread-1").await.unwrap();
        assert_eq!(saver.len().await, 1);

        // thread-2 should still exist
        let tuple = saver
            .get_tuple(&Config::new().with_thread_id("thread-2"))
            .await
            .unwrap();
        assert!(tuple.is_some());
    }

    #[tokio::test]
    async fn test_memory_saver_prune() {
        let saver = MemorySaver::new();
        let config = Config::new().with_thread_id("thread-1");

        // Add 10 checkpoints
        for i in 0..10 {
            let checkpoint = Checkpoint::new().with_thread_id("thread-1");
            let metadata = CheckpointMetadata {
                step: i,
                ..Default::default()
            };
            saver.put(&checkpoint, &metadata, &config).await.unwrap();
        }

        assert_eq!(saver.len().await, 10);

        // Keep only 3 most recent
        let removed = saver.prune("thread-1", 3).await.unwrap();
        assert_eq!(removed, 7);
        assert_eq!(saver.len().await, 3);

        // Verify we kept the most recent ones
        let list = saver.list(&config, None).await.unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].metadata.step, 9); // newest
        assert_eq!(list[2].metadata.step, 7); // oldest of the kept ones
    }

    #[tokio::test]
    async fn test_memory_saver_clear() {
        let saver = MemorySaver::new();

        let checkpoint = Checkpoint::new().with_thread_id("test");
        let metadata = CheckpointMetadata::default();
        let config = Config::new().with_thread_id("test");

        saver.put(&checkpoint, &metadata, &config).await.unwrap();
        assert!(!saver.is_empty().await);

        saver.clear().await;
        assert!(saver.is_empty().await);
    }
}
