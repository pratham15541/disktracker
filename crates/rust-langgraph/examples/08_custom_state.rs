//! Custom state example demonstrating different state types.
//!
//! This example shows how to define custom state types with
//! sophisticated merge logic.

use rust_langgraph::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AnalyticsState {
    total_views: u64,
    unique_users: HashMap<String, u64>,
    tags: Vec<String>,
}

impl State for AnalyticsState {
    fn merge(&mut self, other: Self) -> Result<()> {
        // Sum total views
        self.total_views += other.total_views;

        // Merge unique users (sum their view counts)
        for (user, count) in other.unique_users {
            *self.unique_users.entry(user).or_insert(0) += count;
        }

        // Merge tags (deduplicate)
        for tag in other.tags {
            if !self.tags.contains(&tag) {
                self.tags.push(tag);
            }
        }

        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Custom State Example ===\n");

    let mut graph = StateGraph::new();

    // Node that processes page views
    graph.add_node("process_views", |mut state: AnalyticsState, _config: &Config| async move {
        println!("Processing page views...");
        state.total_views += 100;
        state.unique_users.insert("user_alice".to_string(), 15);
        state.unique_users.insert("user_bob".to_string(), 25);
        Ok(state)
    });

    // Node that processes tags
    graph.add_node("process_tags", |mut state: AnalyticsState, _config: &Config| async move {
        println!("Processing tags...");
        state.tags.push("trending".to_string());
        state.tags.push("featured".to_string());
        Ok(state)
    });

    // Node that adds more user activity
    graph.add_node("process_activity", |mut state: AnalyticsState, _config: &Config| async move {
        println!("Processing user activity...");
        state.unique_users.insert("user_alice".to_string(), 5); // Alice views 5 more pages
        state.unique_users.insert("user_charlie".to_string(), 10); // New user
        Ok(state)
    });

    graph.set_entry_point("process_views");
    graph.add_edge("process_views", "process_tags");
    graph.add_edge("process_views", "process_activity");
    graph.add_finish_points(vec!["process_tags", "process_activity"]);

    let mut app = graph.compile(None)?;

    let initial_state = AnalyticsState {
        total_views: 1000,
        unique_users: HashMap::new(),
        tags: vec!["news".to_string()],
    };

    println!("Initial state:");
    println!("  Total views: {}", initial_state.total_views);
    println!("  Unique users: {}", initial_state.unique_users.len());
    println!("  Tags: {:?}\n", initial_state.tags);

    let result = app.invoke(initial_state, Config::default()).await?;

    println!("\nFinal state after merge:");
    println!("  Total views: {}", result.total_views);
    println!("  Unique users: {}", result.unique_users.len());
    println!("  User details:");
    for (user, count) in &result.unique_users {
        println!("    - {}: {} views", user, count);
    }
    println!("  Tags: {:?}", result.tags);

    Ok(())
}
