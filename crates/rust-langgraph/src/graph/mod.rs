//! Graph builder API and compiled graph.

pub mod state_graph;

pub use state_graph::{StateGraph, CompiledGraph};

/// Special node name for the graph entry point
pub const START: &str = "__start__";

/// Special node name for the graph exit point
pub const END: &str = "__end__";
