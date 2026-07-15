//! Streaming example demonstrating real-time event observation.
//!
//! This example shows how to stream events from a graph execution
//! to observe progress in real-time.

use rust_langgraph::prelude::*;
use rust_langgraph::types::{StreamMode, StreamEvent};
use serde::{Deserialize, Serialize};
use futures::StreamExt;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ProcessingState {
    step: i32,
    data: String,
}

impl State for ProcessingState {
    fn merge(&mut self, other: Self) -> Result<()> {
        self.step = other.step;
        if !other.data.is_empty() {
            self.data = other.data;
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Streaming Example ===\n");

    let mut graph = StateGraph::new();

    graph.add_node("step1", |mut state: ProcessingState, _config: &Config| async move {
        println!("[Node step1] Processing...");
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        state.step = 1;
        state.data = "Completed step 1".to_string();
        Ok(state)
    });

    graph.add_node("step2", |mut state: ProcessingState, _config: &Config| async move {
        println!("[Node step2] Processing...");
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        state.step = 2;
        state.data = "Completed step 2".to_string();
        Ok(state)
    });

    graph.add_node("step3", |mut state: ProcessingState, _config: &Config| async move {
        println!("[Node step3] Processing...");
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        state.step = 3;
        state.data = "Completed step 3".to_string();
        Ok(state)
    });

    graph.set_entry_point("step1");
    graph.add_edge("step1", "step2");
    graph.add_edge("step2", "step3");
    graph.set_finish_point("step3");

    let mut app = graph.compile(None)?;

    let initial_state = ProcessingState {
        step: 0,
        data: "Starting".to_string(),
    };

    println!("Streaming graph execution:\n");

    // Stream events
    let mut stream = app.stream(initial_state, Config::default(), StreamMode::Values).await?;

    let mut event_count = 0;
    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => {
                event_count += 1;
                match event {
                    StreamEvent::Values { data, .. } => {
                        println!("📦 Event {}: {:?}", event_count, data);
                    }
                    StreamEvent::Updates { data, node, .. } => {
                        println!("🔄 Update from node '{}': {:?}", node, data);
                    }
                    StreamEvent::Checkpoint { checkpoint_id, step, .. } => {
                        println!("💾 Checkpoint saved: {} (step {})", checkpoint_id, step);
                    }
                    _ => {
                        println!("ℹ️  Other event: {:?}", event);
                    }
                }
            }
            Err(e) => {
                eprintln!("❌ Error: {}", e);
                break;
            }
        }
    }

    println!("\n✅ Streaming completed! Received {} events", event_count);

    Ok(())
}
