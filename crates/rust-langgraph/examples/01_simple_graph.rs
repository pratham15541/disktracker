//! Simple graph example demonstrating basic node and edge usage.
//!
//! This example shows how to create a basic graph with two nodes:
//! one that adds to a counter, and one that multiplies it.

use rust_langgraph::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CounterState {
    count: i32,
}

impl State for CounterState {
    fn merge(&mut self, other: Self) -> Result<()> {
        self.count += other.count;
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Simple Graph Example ===\n");

    // Create a new graph
    let mut graph = StateGraph::new();

    // Add a node that increments the counter
    graph.add_node("add_five", |mut state: CounterState, _config: &Config| async move {
        println!("Adding 5 to count: {}", state.count);
        state.count += 5;
        Ok(state)
    });

    // Add a node that multiplies the counter
    graph.add_node("multiply_two", |mut state: CounterState, _config: &Config| async move {
        println!("Multiplying count by 2: {}", state.count);
        state.count *= 2;
        Ok(state)
    });

    // Set the entry point
    graph.set_entry_point("add_five");

    // Add an edge from add_five to multiply_two
    graph.add_edge("add_five", "multiply_two");

    // Set finish point
    graph.set_finish_point("multiply_two");

    // Compile the graph
    let mut app = graph.compile(None)?;

    // Run the graph with initial state
    let initial_state = CounterState { count: 10 };
    println!("Initial count: {}\n", initial_state.count);

    let result = app.invoke(initial_state, Config::default()).await?;

    println!("\nFinal count: {}", result.count);
    println!("Calculation: (10 + 5) * 2 = {}", result.count);

    Ok(())
}
