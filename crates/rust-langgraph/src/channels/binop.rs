//! Binary operator aggregate channel implementation.
//!
//! A channel that reduces multiple writes using a binary operator function.

use super::BaseChannel;
use crate::errors::Result;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::sync::Arc;

/// A channel that reduces values using a binary operator.
///
/// This channel maintains a single value and applies a reduction function
/// when new values are written. This is useful for operations like summing,
/// finding max/min, or any other associative binary operation.
///
/// # Example
///
/// ```rust
/// use rust_langgraph::channels::{BaseChannel, BinaryOperatorAggregate};
///
/// // Sum reducer
/// let mut channel = BinaryOperatorAggregate::new(0, |a, b| a + b);
/// channel.update(vec![serde_json::json!(1), serde_json::json!(2), serde_json::json!(3)]).unwrap();
/// assert_eq!(channel.get().unwrap(), Some(serde_json::json!(6)));
/// ```
pub struct BinaryOperatorAggregate<T, F>
where
    F: Fn(T, T) -> T + Send + Sync,
{
    value: T,
    reducer: Arc<F>,
}

impl<T, F> BinaryOperatorAggregate<T, F>
where
    T: Clone,
    F: Fn(T, T) -> T + Send + Sync + 'static,
{
    /// Create a new BinaryOperatorAggregate with an initial value and reducer
    pub fn new(initial: T, reducer: F) -> Self {
        Self {
            value: initial,
            reducer: Arc::new(reducer),
        }
    }

    /// Get a reference to the current value
    pub fn value(&self) -> &T {
        &self.value
    }
}

impl<T, F> Debug for BinaryOperatorAggregate<T, F>
where
    T: Debug,
    F: Fn(T, T) -> T + Send + Sync,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BinaryOperatorAggregate")
            .field("value", &self.value)
            .field("reducer", &"<function>")
            .finish()
    }
}

impl<T, F> BaseChannel for BinaryOperatorAggregate<T, F>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone + Send + Sync + Debug + 'static,
    F: Fn(T, T) -> T + Send + Sync + 'static,
{
    fn get(&self) -> Result<Option<serde_json::Value>> {
        Ok(Some(serde_json::to_value(&self.value)?))
    }

    fn update(&mut self, values: Vec<serde_json::Value>) -> Result<()> {
        for value_json in values {
            let new_value: T = serde_json::from_value(value_json)?;
            self.value = (self.reducer)(self.value.clone(), new_value);
        }
        Ok(())
    }

    fn checkpoint(&self) -> Result<serde_json::Value> {
        serde_json::to_value(&self.value).map_err(Into::into)
    }

    fn from_checkpoint(_data: serde_json::Value) -> Result<Box<dyn BaseChannel>> {
        // Note: We can't fully restore without the reducer function
        // This is a limitation of the type-erased channel system
        // In practice, channels are created by the graph and checkpoints
        // only restore the data, not the channel instances themselves
        Err(crate::errors::Error::channel(
            "BinaryOperatorAggregate cannot be restored from checkpoint alone - requires reducer function",
        ))
    }

    fn type_name(&self) -> &'static str {
        "BinaryOperatorAggregate"
    }

    fn is_empty(&self) -> bool {
        false // Always has a value (at least the initial value)
    }
}

// Common reducer implementations

/// Create a sum reducer channel
pub fn sum_channel<T>(initial: T) -> BinaryOperatorAggregate<T, impl Fn(T, T) -> T + Send + Sync>
where
    T: std::ops::Add<Output = T> + Serialize + for<'de> Deserialize<'de> + Clone + Send + Sync + Debug + 'static,
{
    BinaryOperatorAggregate::new(initial, |a, b| a + b)
}

/// Create a max reducer channel
pub fn max_channel<T>(initial: T) -> BinaryOperatorAggregate<T, impl Fn(T, T) -> T + Send + Sync>
where
    T: Ord + Serialize + for<'de> Deserialize<'de> + Clone + Send + Sync + Debug + 'static,
{
    BinaryOperatorAggregate::new(initial, |a, b| a.max(b))
}

/// Create a min reducer channel
pub fn min_channel<T>(initial: T) -> BinaryOperatorAggregate<T, impl Fn(T, T) -> T + Send + Sync>
where
    T: Ord + Serialize + for<'de> Deserialize<'de> + Clone + Send + Sync + Debug + 'static,
{
    BinaryOperatorAggregate::new(initial, |a, b| a.min(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_binop_sum() {
        let mut channel = BinaryOperatorAggregate::new(0, |a, b| a + b);
        assert_eq!(channel.get().unwrap(), Some(serde_json::json!(0)));

        channel
            .update(vec![
                serde_json::json!(1),
                serde_json::json!(2),
                serde_json::json!(3),
            ])
            .unwrap();

        assert_eq!(channel.get().unwrap(), Some(serde_json::json!(6)));
    }

    #[test]
    fn test_binop_max() {
        let mut channel = max_channel(0);
        channel
            .update(vec![
                serde_json::json!(5),
                serde_json::json!(2),
                serde_json::json!(8),
                serde_json::json!(3),
            ])
            .unwrap();

        assert_eq!(channel.get().unwrap(), Some(serde_json::json!(8)));
    }

    #[test]
    fn test_binop_min() {
        let mut channel = min_channel(100);
        channel
            .update(vec![
                serde_json::json!(50),
                serde_json::json!(75),
                serde_json::json!(25),
            ])
            .unwrap();

        assert_eq!(channel.get().unwrap(), Some(serde_json::json!(25)));
    }

    #[test]
    fn test_binop_custom() {
        // Product reducer
        let mut channel = BinaryOperatorAggregate::new(1, |a: i32, b: i32| a * b);
        channel
            .update(vec![
                serde_json::json!(2),
                serde_json::json!(3),
                serde_json::json!(4),
            ])
            .unwrap();

        assert_eq!(channel.get().unwrap(), Some(serde_json::json!(24)));
    }

    #[test]
    fn test_binop_checkpoint() {
        let mut channel = sum_channel(0);
        channel.update(vec![serde_json::json!(10)]).unwrap();

        let checkpoint = channel.checkpoint().unwrap();
        assert_eq!(checkpoint, serde_json::json!(10));
    }
}
