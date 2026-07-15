//! Pregel execution engine and related types.

pub mod branch;
pub mod engine;

pub use branch::{Branch, BranchResult};
pub use engine::Pregel;
