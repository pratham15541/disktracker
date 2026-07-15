//! Ephemeral value channel implementation.
//!
//! A channel that clears its value after each superstep.

use super::BaseChannel;
use crate::errors::Result;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::marker::PhantomData;

/// A channel that is cleared after each superstep.
///
/// Ephemeral channels are useful for temporary data that should only
/// be visible to nodes within a single superstep. After the step completes,
/// the value is automatically cleared.
///
/// # Example
///
/// ```rust
/// use rust_langgraph::channels::{BaseChannel, EphemeralValue};
///
/// let mut channel = EphemeralValue::<String>::new();
/// channel.update(vec![serde_json::json!("temporary")]).unwrap();
/// assert!(channel.get().unwrap().is_some());
///
/// // After superstep (simulated by calling clear)
/// channel.clear();
/// assert!(channel.get().unwrap().is_none());
/// ```
#[derive(Debug, Clone)]
pub struct EphemeralValue<T> {
    value: Option<T>,
    _phantom: PhantomData<T>,
}

impl<T> EphemeralValue<T> {
    /// Create a new empty EphemeralValue channel
    pub fn new() -> Self {
        Self {
            value: None,
            _phantom: PhantomData,
        }
    }

    /// Clear the ephemeral value
    pub fn clear(&mut self) {
        self.value = None;
    }
}

impl<T> Default for EphemeralValue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> BaseChannel for EphemeralValue<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone + Send + Sync + Debug + 'static,
{
    fn get(&self) -> Result<Option<serde_json::Value>> {
        match &self.value {
            Some(v) => Ok(Some(serde_json::to_value(v)?)),
            None => Ok(None),
        }
    }

    fn update(&mut self, values: Vec<serde_json::Value>) -> Result<()> {
        // Like LastValue, keep only the last value
        if let Some(last) = values.last() {
            self.value = Some(serde_json::from_value(last.clone())?);
        }
        Ok(())
    }

    fn checkpoint(&self) -> Result<serde_json::Value> {
        // Ephemeral values are not persisted in checkpoints
        Ok(serde_json::Value::Null)
    }

    fn from_checkpoint(_data: serde_json::Value) -> Result<Box<dyn BaseChannel>> {
        // Always start empty
        Ok(Box::new(Self::new()))
    }

    fn type_name(&self) -> &'static str {
        "EphemeralValue"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ephemeral_basic() {
        let mut channel = EphemeralValue::<i32>::new();
        assert!(channel.get().unwrap().is_none());

        channel.update(vec![serde_json::json!(42)]).unwrap();
        assert_eq!(channel.get().unwrap(), Some(serde_json::json!(42)));
    }

    #[test]
    fn test_ephemeral_clear() {
        let mut channel = EphemeralValue::<String>::new();
        channel.update(vec![serde_json::json!("temporary")]).unwrap();
        assert!(channel.get().unwrap().is_some());

        channel.clear();
        assert!(channel.get().unwrap().is_none());
    }

    #[test]
    fn test_ephemeral_checkpoint() {
        let mut channel = EphemeralValue::<i32>::new();
        channel.update(vec![serde_json::json!(100)]).unwrap();

        // Ephemeral values should not be saved in checkpoints
        let checkpoint = channel.checkpoint().unwrap();
        assert_eq!(checkpoint, serde_json::Value::Null);

        // Restoring from checkpoint should give empty channel
        let restored = EphemeralValue::<i32>::from_checkpoint(checkpoint).unwrap();
        assert!(restored.get().unwrap().is_none());
    }

    #[test]
    fn test_ephemeral_last_write_wins() {
        let mut channel = EphemeralValue::<i32>::new();
        channel
            .update(vec![
                serde_json::json!(1),
                serde_json::json!(2),
                serde_json::json!(3),
            ])
            .unwrap();

        assert_eq!(channel.get().unwrap(), Some(serde_json::json!(3)));
    }
}
