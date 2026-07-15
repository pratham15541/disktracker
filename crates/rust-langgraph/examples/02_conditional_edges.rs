//! Conditional edges example demonstrating branching logic.
//!
//! This example shows how to use conditional edges to route
//! execution based on state values.

use rust_langgraph::prelude::*;
use rust_langgraph::pregel::BranchResult;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct NumberState {
    value: i32,
    result: String,
}

impl State for NumberState {
    fn merge(&mut self, other: Self) -> Result<()> {
        self.value = other.value;
        if !other.result.is_empty() {
            self.result = other.result;
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Conditional Edges Example ===\n");

    let mut graph = StateGraph::new();

    // Check node: examines the value
    graph.add_node("check", |state: NumberState, _config: &Config| async move {
        println!("Checking value: {}", state.value);
        Ok(state)
    });

    // Positive node: handles positive numbers
    graph.add_node("positive", |mut state: NumberState, _config: &Config| async move {
        println!("Value is positive!");
        state.result = format!("{} is a positive number", state.value);
        Ok(state)
    });

    // Negative node: handles negative numbers
    graph.add_node("negative", |mut state: NumberState, _config: &Config| async move {
        println!("Value is negative!");
        state.result = format!("{} is a negative number", state.value);
        Ok(state)
    });

    // Zero node: handles zero
    graph.add_node("zero", |mut state: NumberState, _config: &Config| async move {
        println!("Value is zero!");
        state.result = "The value is zero".to_string();
        Ok(state)
    });

    // Set entry point
    graph.set_entry_point("check");

    // Add conditional routing from check node
    graph.add_conditional_edges("check", |state: &NumberState| async move {
        if state.value > 0 {
            Ok(BranchResult::single("positive"))
        } else if state.value < 0 {
            Ok(BranchResult::single("negative"))
        } else {
            Ok(BranchResult::single("zero"))
        }
    });

    // Set all outcome nodes as finish points
    graph.add_finish_points(vec!["positive", "negative", "zero"]);

    // Compile the graph
    let mut app = graph.compile(None)?;

    // Test with different values
    for value in &[42, -17, 0] {
        println!("\nTesting with value: {}", value);
        let state = NumberState {
            value: *value,
            result: String::new(),
        };

        let result = app.invoke(state, Config::default()).await?;
        println!("Result: {}\n", result.result);
    }

    Ok(())
}
