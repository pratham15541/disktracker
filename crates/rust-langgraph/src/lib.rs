//! # Rust LangGraph — community graph runtime for LLM apps
//!
//! **Rust LangGraph** is an independent Rust library inspired by [LangGraph](https://github.com/langchain-ai/langgraph)
//! (not affiliated with LangChain). It helps you build stateful, multi-actor LLM workflows with a graph execution
//! model similar in spirit to Google’s Pregel, with checkpointing, streaming, and conditional routing.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use rust_langgraph::prelude::*;
//!
//! #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
//! struct MyState {
//!     count: i32,
//! }
//!
//! impl State for MyState {
//!     fn merge(&mut self, other: Self) -> Result<(), Error> {
//!         self.count += other.count;
//!         Ok(())
//!     }
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let mut graph = StateGraph::new();
//!     
//!     graph.add_node("increment", |state: MyState, _config: &Config| async move {
//!         Ok(MyState { count: state.count + 1 })
//!     });
//!     
//!     graph.set_entry_point("increment");
//!     graph.set_finish_point("increment");
//!     
//!     let app = graph.compile(None)?;
//!     let result = app.invoke(MyState { count: 0 }, Config::default()).await?;
//!     
//!     println!("Final count: {}", result.count);
//!     Ok(())
//! }
//! ```
//!
//! ## Features
//!
//! - **Stateful Execution**: Build graphs where nodes communicate through shared state
//! - **Checkpointing**: Save and resume execution at any point
//! - **Streaming**: Stream events as the graph executes
//! - **Conditional Logic**: Dynamic routing based on state
//! - **Parallel Execution**: Execute independent nodes concurrently
//! - **Human-in-the-Loop**: Interrupt and resume with human input
//! - **LLM Integration**: OpenAI, OpenRouter, Anthropic, and Ollama adapters (feature-gated)
//!
//! ## More documentation
//!
//! - **README.md** — full user guide (install, tutorial, API table, examples)
//! - **AGENTS.md** — cheat sheet for AI coding agents and contributors (correct crate name, features, patterns, pitfalls)

pub mod config;
pub mod errors;
pub mod state;
pub mod types;

pub mod channels;
pub mod nodes;
pub mod pregel;
pub mod graph;
pub mod checkpoint;

#[cfg(feature = "memory-checkpoint")]
pub mod checkpoint_backends;

#[cfg(any(
    feature = "openai",
    feature = "openrouter",
    feature = "anthropic",
    feature = "ollama"
))]
pub mod llm;

#[cfg(feature = "prebuilt")]
pub mod prebuilt;

pub mod runtime;

/// Re-exports of commonly used types for convenient imports
pub mod prelude {
    pub use crate::config::Config;
    pub use crate::errors::{Error, Result};
    pub use crate::state::{State, MessagesState, Message, add_messages};
    pub use crate::types::{Send, Command, StreamMode, StreamEvent};
    pub use crate::nodes::Node;
    pub use crate::graph::{StateGraph, CompiledGraph};
    pub use crate::checkpoint::{BaseCheckpointSaver, Checkpoint};
    
    #[cfg(feature = "memory-checkpoint")]
    pub use crate::checkpoint_backends::memory::MemorySaver;
    
    #[cfg(feature = "prebuilt")]
    pub use crate::prebuilt::{create_react_agent, Tool, ToolNode};
    
    #[cfg(any(
        feature = "openai",
        feature = "openrouter",
        feature = "anthropic",
        feature = "ollama"
    ))]
    pub use crate::llm::ChatModel;
}

// Re-export commonly used types at the crate root
pub use config::Config;
pub use errors::{Error, Result};
pub use state::State;
pub use graph::StateGraph;
