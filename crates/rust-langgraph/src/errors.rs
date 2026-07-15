//! Error types for LangGraph operations.
//!
//! This module defines the error types used throughout the library, providing
//! detailed context for various failure modes in graph execution, checkpointing,
//! and state management.


/// The main error type for LangGraph operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Error during graph execution
    #[error("Execution error: {0}")]
    ExecutionError(String),

    /// Error during state operations
    #[error("State error: {0}")]
    StateError(String),

    /// Error during checkpoint operations
    #[error("Checkpoint error: {0}")]
    CheckpointError(String),

    /// Error during channel operations
    #[error("Channel error: {0}")]
    ChannelError(String),

    /// Graph recursion limit exceeded
    #[error("Graph recursion limit exceeded: {current} > {limit}")]
    RecursionLimitError { current: usize, limit: usize },

    /// Node not found in graph
    #[error("Node not found: {0}")]
    NodeNotFound(String),

    /// Invalid graph configuration
    #[error("Invalid graph configuration: {0}")]
    InvalidGraph(String),

    /// Serialization/deserialization error
    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    /// Interrupt was triggered
    #[error("Interrupt: {0}")]
    Interrupt(String),

    /// Invalid update to state
    #[error("Invalid update: {0}")]
    InvalidUpdate(String),

    /// Database error (when using SQL checkpointers)
    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    #[error("Database error: {0}")]
    DatabaseError(#[from] sqlx::Error),

    /// HTTP error (when using LLM providers)
    #[cfg(any(feature = "openai", feature = "anthropic", feature = "ollama"))]
    #[error("HTTP error: {0}")]
    HttpError(#[from] reqwest::Error),

    /// Generic error with context
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Create a new execution error
    pub fn execution(msg: impl Into<String>) -> Self {
        Error::ExecutionError(msg.into())
    }

    /// Create a new state error
    pub fn state(msg: impl Into<String>) -> Self {
        Error::StateError(msg.into())
    }

    /// Create a new checkpoint error
    pub fn checkpoint(msg: impl Into<String>) -> Self {
        Error::CheckpointError(msg.into())
    }

    /// Create a new channel error
    pub fn channel(msg: impl Into<String>) -> Self {
        Error::ChannelError(msg.into())
    }

    /// Create a new invalid graph error
    pub fn invalid_graph(msg: impl Into<String>) -> Self {
        Error::InvalidGraph(msg.into())
    }

    /// Create a new node not found error
    pub fn node_not_found(name: impl Into<String>) -> Self {
        Error::NodeNotFound(name.into())
    }

    /// Create an interrupt error
    pub fn interrupt(msg: impl Into<String>) -> Self {
        Error::Interrupt(msg.into())
    }

    /// Create an invalid update error
    pub fn invalid_update(msg: impl Into<String>) -> Self {
        Error::InvalidUpdate(msg.into())
    }
}

/// A specialized Result type for LangGraph operations.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_creation() {
        let err = Error::execution("test");
        assert!(matches!(err, Error::ExecutionError(_)));
        assert_eq!(err.to_string(), "Execution error: test");
    }

    #[test]
    fn test_recursion_limit_error() {
        let err = Error::RecursionLimitError {
            current: 100,
            limit: 50,
        };
        assert!(err.to_string().contains("100"));
        assert!(err.to_string().contains("50"));
    }

    #[test]
    fn test_error_from_json() {
        let json_err = serde_json::from_str::<i32>("not a number").unwrap_err();
        let err: Error = json_err.into();
        assert!(matches!(err, Error::SerializationError(_)));
    }
}
