# Rust LangGraph — implementation summary

## Status

**Rust LangGraph** is a community Rust library inspired by [LangGraph](https://github.com/langchain-ai/langgraph) (not affiliated with LangChain). It lives in the `rust-langgraph/` crate.

### Architecture highlights

- **Single crate** with optional Cargo features (checkpoint backends, LLM providers, prebuilt agents)
- **Channel-centric Pregel execution**: supersteps, triggers, merges
- **Builder API**: `StateGraph` → `CompiledGraph`
- **Type-safe state** with explicit `merge` semantics

### Implemented areas

1. **StateGraph** — `add_node`, `add_edge`, `add_conditional_edges`, `compile`
2. **Channels** — `LastValue`, `Topic`, `BinaryOperatorAggregate`, `EphemeralValue`
3. **Pregel engine** — superstep loop, parallel node execution, channel read/write (v0.1.1+: next superstep triggers from this step’s writes; edge fan-out copies state to `{target}_input`; `read_state_for_node` merges inputs / `__start__` for entry; `get_final_state` prefers finish `{node}_output`)
4. **Checkpointing** — `Checkpoint`, `BaseCheckpointSaver`, `MemorySaver`; DB backends are feature-gated / in progress
5. **Branching** — `Branch`, `BranchResult` (including `Send` for dynamic routing)
6. **LLM** — `ChatModel`, `ToolInfo`; `OllamaAdapter`, `OpenAIAdapter`, `OpenRouterAdapter`, `AnthropicAdapter` (feature-gated)
7. **Prebuilt** — `create_react_agent`, `Tool`, `ToolNode`, `validate_chat_history`
8. **Docs** — README.md, AGENTS.md, examples, rustdoc

### Commands

```bash
cargo check --manifest-path rust-langgraph/Cargo.toml
cargo check --manifest-path rust-langgraph/Cargo.toml --all-features

cargo run --manifest-path rust-langgraph/Cargo.toml --example simple_graph
cargo run --manifest-path rust-langgraph/Cargo.toml --example ollama_chat --features ollama
cargo run --manifest-path rust-langgraph/Cargo.toml --example react_agent_ollama --features ollama,prebuilt

cargo test --manifest-path rust-langgraph/Cargo.toml
cargo test --manifest-path rust-langgraph/Cargo.toml --test test_ollama_integration --features ollama,prebuilt -- --ignored
```

### Possible future work

- SQLite / PostgreSQL checkpoint backends (full implementation)
- Deeper interrupt / `Command` integration in the Pregel loop
- More integration tests against Python LangGraph behavior
- Provider streaming improvements
- Subgraphs, benchmarks

### Project layout

```
rust-langgraph/
├── Cargo.toml
├── README.md
├── AGENTS.md
├── LICENSE
├── src/           # library
├── examples/
└── tests/
```

### Design notes

- State and checkpoints are serde-friendly where applicable for persistence and interoperability goals.
- The public API is organized around `rust_langgraph::prelude::*` for common types.

See **README.md** for the user guide and **AGENTS.md** for agent/contributor conventions.
