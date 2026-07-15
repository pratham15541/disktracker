//! Runtime context for node execution.
//!
//! The Runtime provides nodes with access to execution context like
//! the current step, checkpoint ID, and configuration.

use crate::config::Config;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Execution context available to nodes during execution.
///
/// Runtime provides nodes with information about the current execution
/// state and allows them to interact with the execution environment.
///
/// # Example
///
/// ```rust
/// use rust_langgraph::runtime::Runtime;
/// use rust_langgraph::Config;
///
/// let config = Config::new().with_thread_id("test-123");
/// let runtime = Runtime::new(config);
///
/// assert_eq!(runtime.step(), 0);
/// ```
#[derive(Debug, Clone)]
pub struct Runtime {
    config: Config,
    checkpoint_id: Option<String>,
    step: usize,
}

impl Runtime {
    /// Create a new runtime context
    pub fn new(config: Config) -> Self {
        Self {
            config,
            checkpoint_id: None,
            step: 0,
        }
    }

    /// Get the configuration
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Get the current checkpoint ID
    pub fn checkpoint_id(&self) -> Option<&str> {
        self.checkpoint_id.as_deref()
    }

    /// Get the current step number
    pub fn step(&self) -> usize {
        self.step
    }

    /// Set the checkpoint ID
    pub fn set_checkpoint_id(&mut self, id: impl Into<String>) {
        self.checkpoint_id = Some(id.into());
    }

    /// Increment the step counter
    pub fn increment_step(&mut self) {
        self.step += 1;
    }

    /// Get the thread ID from config
    pub fn thread_id(&self) -> Option<&str> {
        self.config.thread_id.as_deref()
    }
}

/// Shared runtime context that can be passed between tasks
pub type SharedRuntime = Arc<RwLock<Runtime>>;

/// Create a shared runtime
pub fn shared_runtime(config: Config) -> SharedRuntime {
    Arc::new(RwLock::new(Runtime::new(config)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_creation() {
        let config = Config::new().with_thread_id("test");
        let runtime = Runtime::new(config);

        assert_eq!(runtime.thread_id(), Some("test"));
        assert_eq!(runtime.step(), 0);
        assert!(runtime.checkpoint_id().is_none());
    }

    #[test]
    fn test_runtime_mutations() {
        let config = Config::new();
        let mut runtime = Runtime::new(config);

        runtime.set_checkpoint_id("checkpoint-1");
        assert_eq!(runtime.checkpoint_id(), Some("checkpoint-1"));

        runtime.increment_step();
        assert_eq!(runtime.step(), 1);

        runtime.increment_step();
        assert_eq!(runtime.step(), 2);
    }

    #[tokio::test]
    async fn test_shared_runtime() {
        let config = Config::new();
        let runtime = shared_runtime(config);

        {
            let mut rt = runtime.write().await;
            rt.increment_step();
        }

        {
            let rt = runtime.read().await;
            assert_eq!(rt.step(), 1);
        }
    }
}
