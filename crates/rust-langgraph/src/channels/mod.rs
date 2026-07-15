//! Channels for state communication in graphs.
//!
//! Channels are the core mechanism for state management in LangGraph.
//! Unlike typical graph systems where nodes pass data directly, in LangGraph
//! nodes write to channels and read from channels. This enables powerful
//! patterns like automatic state reduction and checkpoint/resume.
//!
//! # Channel Types
//!
//! - **LastValue**: Stores only the last written value
//! - **Topic**: Accumulates all written values as a sequence
//! - **BinaryOperatorAggregate**: Reduces multiple writes with a custom function
//! - **EphemeralValue**: Cleared after each superstep

use crate::errors::Result;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

pub mod last_value;
pub mod topic;
pub mod binop;
pub mod ephemeral;

pub use last_value::LastValue;
pub use topic::Topic;
pub use binop::BinaryOperatorAggregate;
pub use ephemeral::EphemeralValue;

/// The base trait for all channels.
///
/// Channels manage how state flows through the graph. Each channel
/// has its own semantics for how it handles multiple writes in a
/// single superstep.
pub trait BaseChannel: Send + Sync + Debug {
    /// Get the current value as JSON
    fn get(&self) -> Result<Option<serde_json::Value>>;

    /// Update the channel with new values
    ///
    /// If multiple values are provided, the channel applies its
    /// reduction logic (e.g., last-write-wins, sum, append).
    fn update(&mut self, values: Vec<serde_json::Value>) -> Result<()>;

    /// Serialize the channel state for checkpointing
    fn checkpoint(&self) -> Result<serde_json::Value>;

    /// Restore the channel state from a checkpoint
    fn from_checkpoint(data: serde_json::Value) -> Result<Box<dyn BaseChannel>>
    where
        Self: Sized;

    /// Get the channel's type name for debugging
    fn type_name(&self) -> &'static str;

    /// Check if the channel is empty
    fn is_empty(&self) -> bool {
        self.get().ok().flatten().is_none()
    }
}

/// A wrapper for type-erased channels
pub type ChannelBox = Box<dyn BaseChannel>;

/// Helper to create a LastValue channel
pub fn last_value<T>() -> LastValue<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone + Send + Sync + Debug + 'static,
{
    LastValue::new()
}

/// Helper to create a Topic channel
pub fn topic<T>() -> Topic<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone + Send + Sync + Debug + 'static,
{
    Topic::new()
}

/// Helper to create a BinaryOperatorAggregate channel
pub fn binop<T, F>(initial: T, reducer: F) -> BinaryOperatorAggregate<T, F>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone + Send + Sync + Debug + 'static,
    F: Fn(T, T) -> T + Send + Sync + 'static,
{
    BinaryOperatorAggregate::new(initial, reducer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_helpers() {
        let mut lv = last_value::<i32>();
        lv.update(vec![serde_json::json!(42)]).unwrap();
        assert_eq!(lv.get().unwrap(), Some(serde_json::json!(42)));

        let mut topic = topic::<String>();
        topic.update(vec![serde_json::json!("hello")]).unwrap();
        topic.update(vec![serde_json::json!("world")]).unwrap();
        let values: Vec<String> = serde_json::from_value(topic.get().unwrap().unwrap()).unwrap();
        assert_eq!(values.len(), 2);
    }
}
