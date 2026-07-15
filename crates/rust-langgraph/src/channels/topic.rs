//! Topic channel implementation.
//!
//! A channel that accumulates all written values as a sequence.

use super::BaseChannel;
use crate::errors::Result;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::marker::PhantomData;

/// A channel that accumulates all written values.
///
/// Unlike LastValue which keeps only the most recent value, Topic
/// appends all writes to a list. This is useful for collecting
/// multiple results or building up a history.
///
/// # Example
///
/// ```rust
/// use rust_langgraph::channels::{BaseChannel, Topic};
///
/// let mut channel = Topic::<String>::new();
/// channel.update(vec![serde_json::json!("first")]).unwrap();
/// channel.update(vec![serde_json::json!("second")]).unwrap();
///
/// let values: Vec<String> = serde_json::from_value(
///     channel.get().unwrap().unwrap()
/// ).unwrap();
/// assert_eq!(values, vec!["first", "second"]);
/// ```
#[derive(Debug, Clone)]
pub struct Topic<T> {
    values: Vec<T>,
    _phantom: PhantomData<T>,
}

impl<T> Topic<T> {
    /// Create a new empty Topic channel
    pub fn new() -> Self {
        Self {
            values: Vec::new(),
            _phantom: PhantomData,
        }
    }

    /// Create a Topic with initial values
    pub fn with_values(values: Vec<T>) -> Self {
        Self {
            values,
            _phantom: PhantomData,
        }
    }

    /// Get the number of accumulated values
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Check if the topic is empty
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl<T> Default for Topic<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> BaseChannel for Topic<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone + Send + Sync + Debug + 'static,
{
    fn get(&self) -> Result<Option<serde_json::Value>> {
        if self.values.is_empty() {
            Ok(None)
        } else {
            Ok(Some(serde_json::to_value(&self.values)?))
        }
    }

    fn update(&mut self, values: Vec<serde_json::Value>) -> Result<()> {
        for value in values {
            let typed_value: T = serde_json::from_value(value)?;
            self.values.push(typed_value);
        }
        Ok(())
    }

    fn checkpoint(&self) -> Result<serde_json::Value> {
        serde_json::to_value(&self.values).map_err(Into::into)
    }

    fn from_checkpoint(data: serde_json::Value) -> Result<Box<dyn BaseChannel>> {
        let values: Vec<T> = serde_json::from_value(data)?;
        Ok(Box::new(Self::with_values(values)))
    }

    fn type_name(&self) -> &'static str {
        "Topic"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topic_basic() {
        let mut channel = Topic::<i32>::new();
        assert!(channel.get().unwrap().is_none());
        assert_eq!(channel.len(), 0);

        channel.update(vec![serde_json::json!(1)]).unwrap();
        assert_eq!(channel.len(), 1);

        let values: Vec<i32> = serde_json::from_value(channel.get().unwrap().unwrap()).unwrap();
        assert_eq!(values, vec![1]);
    }

    #[test]
    fn test_topic_accumulation() {
        let mut channel = Topic::<String>::new();

        channel.update(vec![serde_json::json!("first")]).unwrap();
        channel.update(vec![serde_json::json!("second")]).unwrap();
        channel
            .update(vec![serde_json::json!("third"), serde_json::json!("fourth")])
            .unwrap();

        let values: Vec<String> =
            serde_json::from_value(channel.get().unwrap().unwrap()).unwrap();
        assert_eq!(values, vec!["first", "second", "third", "fourth"]);
    }

    #[test]
    fn test_topic_checkpoint() {
        let mut channel = Topic::<i32>::new();
        channel
            .update(vec![serde_json::json!(1), serde_json::json!(2)])
            .unwrap();

        let checkpoint = channel.checkpoint().unwrap();
        let restored = Topic::<i32>::from_checkpoint(checkpoint).unwrap();

        let values: Vec<i32> = serde_json::from_value(restored.get().unwrap().unwrap()).unwrap();
        assert_eq!(values, vec![1, 2]);
    }
}
