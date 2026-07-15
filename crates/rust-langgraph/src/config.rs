//! Configuration for graph execution.
//!
//! The `Config` type contains settings that control how graphs execute,
//! including checkpointing, recursion limits, and metadata.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for graph execution.
///
/// Config controls various aspects of graph execution including which
/// checkpoint to load, recursion limits, and custom metadata.
///
/// # Example
///
/// ```rust
/// use rust_langgraph::Config;
///
/// let config = Config::new()
///     .with_thread_id("user-123")
///     .with_recursion_limit(100)
///     .with_metadata("user_name", "Alice");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Thread ID for checkpoint isolation
    pub thread_id: Option<String>,
    
    /// Specific checkpoint ID to load (for time travel)
    pub checkpoint_id: Option<String>,
    
    /// Maximum recursion depth before error
    pub recursion_limit: usize,
    
    /// Custom metadata
    pub metadata: HashMap<String, serde_json::Value>,
    
    /// Tags for categorizing runs
    pub tags: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            thread_id: None,
            checkpoint_id: None,
            recursion_limit: 25,
            metadata: HashMap::new(),
            tags: Vec::new(),
        }
    }
}

impl Config {
    /// Create a new default configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the thread ID for checkpoint isolation
    pub fn with_thread_id(mut self, thread_id: impl Into<String>) -> Self {
        self.thread_id = Some(thread_id.into());
        self
    }

    /// Set a specific checkpoint ID to load (for time travel)
    pub fn with_checkpoint_id(mut self, checkpoint_id: impl Into<String>) -> Self {
        self.checkpoint_id = Some(checkpoint_id.into());
        self
    }

    /// Set the recursion limit
    pub fn with_recursion_limit(mut self, limit: usize) -> Self {
        self.recursion_limit = limit;
        self
    }

    /// Add metadata
    pub fn with_metadata(
        mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Add a tag
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Get or create a thread ID
    pub fn ensure_thread_id(&mut self) -> &str {
        if self.thread_id.is_none() {
            self.thread_id = Some(uuid::Uuid::new_v4().to_string());
        }
        self.thread_id.as_ref().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_builder() {
        let config = Config::new()
            .with_thread_id("test-thread")
            .with_recursion_limit(100)
            .with_metadata("key", "value")
            .with_tag("test");

        assert_eq!(config.thread_id.as_deref(), Some("test-thread"));
        assert_eq!(config.recursion_limit, 100);
        assert_eq!(config.metadata.len(), 1);
        assert_eq!(config.tags.len(), 1);
    }

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.recursion_limit, 25);
        assert!(config.thread_id.is_none());
        assert!(config.metadata.is_empty());
    }

    #[test]
    fn test_ensure_thread_id() {
        let mut config = Config::new();
        assert!(config.thread_id.is_none());
        
        let thread_id = config.ensure_thread_id().to_string();
        assert!(!thread_id.is_empty());
        
        // Should return same thread_id on second call
        let thread_id2 = config.ensure_thread_id().to_string();
        assert_eq!(thread_id, thread_id2);
    }

    #[test]
    fn test_config_serialization() {
        let config = Config::new()
            .with_thread_id("test")
            .with_recursion_limit(50);

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: Config = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.thread_id, config.thread_id);
        assert_eq!(deserialized.recursion_limit, config.recursion_limit);
    }
}
