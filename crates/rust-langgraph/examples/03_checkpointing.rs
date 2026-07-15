//! Checkpointing example demonstrating save and resume functionality.
//!
//! This example shows how to use checkpointing to save graph state
//! and resume execution from where it left off.

use rust_langgraph::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TaskState {
    tasks_completed: Vec<String>,
    current_task: String,
}

impl State for TaskState {
    fn merge(&mut self, other: Self) -> Result<()> {
        if !other.current_task.is_empty() {
            self.tasks_completed.push(self.current_task.clone());
            self.current_task = other.current_task;
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Checkpointing Example ===\n");

    // Create a memory-based checkpoint saver
    let checkpointer = Arc::new(MemorySaver::new());

    // Create a simple workflow graph
    let mut graph = StateGraph::new();

    graph.add_node("task1", |mut state: TaskState, _config: &Config| async move {
        println!("Executing Task 1");
        state.current_task = "Task 1".to_string();
        Ok(state)
    });

    graph.add_node("task2", |mut state: TaskState, _config: &Config| async move {
        println!("Executing Task 2");
        state.current_task = "Task 2".to_string();
        Ok(state)
    });

    graph.add_node("task3", |mut state: TaskState, _config: &Config| async move {
        println!("Executing Task 3");
        state.current_task = "Task 3".to_string();
        Ok(state)
    });

    graph.set_entry_point("task1");
    graph.add_edge("task1", "task2");
    graph.add_edge("task2", "task3");
    graph.set_finish_point("task3");

    // Compile with checkpointer
    let mut app = graph.compile(Some(checkpointer.clone()))?;

    // First execution: run the complete workflow
    println!("Running workflow for thread 'workflow-1':\n");
    let config = Config::new().with_thread_id("workflow-1");

    let initial_state = TaskState {
        tasks_completed: vec![],
        current_task: "Starting".to_string(),
    };

    let result = app.invoke(initial_state, config.clone()).await?;

    println!("\nWorkflow completed!");
    println!("Tasks completed: {:?}", result.tasks_completed);
    println!("Current task: {}", result.current_task);

    // Retrieve the saved checkpoint
    println!("\n--- Checking saved state ---");
    if let Some(snapshot) = app.get_state(&config).await? {
        println!("Checkpoint ID: {}", snapshot.checkpoint.id);
        println!("Step: {}", snapshot.metadata.step);
        println!("Saved state: {:?}", snapshot.state);
    }

    // List checkpoint history
    println!("\n--- Checkpoint History ---");
    let history = app.get_state_history(&config, Some(5)).await?;
    for (i, snapshot) in history.iter().enumerate() {
        println!(
            "{}. Step {}: {} tasks completed",
            i + 1,
            snapshot.metadata.step,
            snapshot.state.tasks_completed.len()
        );
    }

    // Run another workflow with a different thread ID
    println!("\n\nRunning a second workflow with thread 'workflow-2':\n");
    let config2 = Config::new().with_thread_id("workflow-2");

    let initial_state2 = TaskState {
        tasks_completed: vec![],
        current_task: "Starting second workflow".to_string(),
    };

    let result2 = app.invoke(initial_state2, config2.clone()).await?;
    println!("\nSecond workflow completed!");
    println!("Tasks: {:?}", result2.tasks_completed);

    // Verify both threads have separate checkpoints
    println!("\n--- Verifying Thread Isolation ---");
    let checkpoint_count = checkpointer.len().await;
    println!("Total checkpoints saved: {}", checkpoint_count);

    Ok(())
}
