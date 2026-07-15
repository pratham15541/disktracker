//! LastValue channel implementation.
//!
//! A channel that stores only the most recent value written to it.

use super::BaseChannel;
use crate::errors::Result;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::marker::PhantomData;

/// A channel that stores the last written value.
///
/// When multiple values are written in a single step, only the last
/// one is kept. This is the most common channel type.
///
/// # Example
///
/// ```rust
/// use rust_langgraph::channels::{BaseChannel, LastValue};
///
/// let mut channel = LastValue::<i32>::new();
/// channel.update(vec![serde_json::json!(1), serde_json::json!(2)]).unwrap();
/// assert_eq!(channel.get().unwrap(), Some(serde_json::json!(2)));
/// ```
#[derive(Debug, Clone)]
pub struct LastValue<T> {
    value: Option<T>,
    _phantom: PhantomData<T>,
}

impl<T> LastValue<T> {
    /// Create a new empty LastValue channel
    pub fn new() -> Self {
        Self {
            value: None,
            _phantom: PhantomData,
        }
    }

    /// Create a LastValue channel with an initial value
    pub fn with_value(value: T) -> Self {
        Self {
            value: Some(value),
            _phantom: PhantomData,
        }
    }
}

impl<T> Default for LastValue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> BaseChannel for LastValue<T>
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
        if let Some(last) = values.last() {
            self.value = Some(serde_json::from_value(last.clone())?);
        }
        Ok(())
    }

    fn checkpoint(&self) -> Result<serde_json::Value> {
        match &self.value {
            Some(v) => serde_json::to_value(v).map_err(Into::into),
            None => Ok(serde_json::Value::Null),
        }
    }

    fn from_checkpoint(data: serde_json::Value) -> Result<Box<dyn BaseChannel>> {
        if data.is_null() {
            Ok(Box::new(Self::new()))
        } else {
            let value: T = serde_json::from_value(data)?;
            Ok(Box::new(Self::with_value(value)))
        }
    }

    fn type_name(&self) -> &'static str {
        "LastValue"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_last_value_basic() {
        let mut channel = LastValue::<i32>::new();
        assert!(channel.get().unwrap().is_none());

        channel.update(vec![serde_json::json!(42)]).unwrap();
        assert_eq!(channel.get().unwrap(), Some(serde_json::json!(42)));
    }

    #[test]
    fn test_last_value_multiple_writes() {
        let mut channel = LastValue::<i32>::new();
        channel
            .update(vec![
                serde_json::json!(1),
                serde_json::json!(2),
                serde_json::json!(3),
            ])
            .unwrap();

        assert_eq!(channel.get().unwrap(), Some(serde_json::json!(3)));
    }

    #[test]
    fn test_last_value_checkpoint() {
        let mut channel = LastValue::<String>::new();
        channel.update(vec![serde_json::json!("hello")]).unwrap();

        let checkpoint = channel.checkpoint().unwrap();
        assert_eq!(checkpoint, serde_json::json!("hello"));

        let restored = LastValue::<String>::from_checkpoint(checkpoint).unwrap();
        assert_eq!(
            restored.get().unwrap(),
            Some(serde_json::json!("hello"))
        );
    }

    #[test]
    fn test_last_value_with_value() {
        let channel = LastValue::with_value(100);
        assert_eq!(channel.get().unwrap(), Some(serde_json::json!(100)));
    }
}
