# Rust LangGraph

<div align="center">

**Graph-native LLM workflows in Rust — inspired by [LangGraph](https://github.com/langchain-ai/langgraph), built by the community**

[![Crates.io](https://img.shields.io/crates/v/rust-langgraph.svg)](https://crates.io/crates/rust-langgraph)
[![Documentation](https://docs.rs/rust-langgraph/badge.svg)](https://docs.rs/rust-langgraph)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

*Not affiliated with LangChain. This is an independent Rust library with a similar programming model.*

</div>

---

## Table of contents

1. [Quick reference (implementers & AI agents)](#quick-reference-implementers--ai-agents)
2. [What this crate is](#what-this-crate-is)
3. [Who should use it](#who-should-use-it)
4. [Installation](#installation)
5. [Copy-paste `Cargo.toml` recipes](#copy-paste-cargotoml-recipes)
6. [Environment variables](#environment-variables)
7. [Five-minute tutorial](#five-minute-tutorial)
8. [Core concepts](#core-concepts)
9. [Graph API reference](#graph-api-reference)
10. [LLMs and agents](#llms-and-agents)
11. [Feature flags (detailed)](#feature-flags-detailed)
12. [Prelude and conditional exports](#prelude-and-conditional-exports)
13. [Common mistakes & compile errors](#common-mistakes--compile-errors)
14. [Verification commands](#verification-commands)
15. [Project layout](#project-layout)
16. [Examples](#examples)
17. [Documentation](#documentation)
18. [Comparison with Python LangGraph](#comparison-with-python-langgraph)
19. [License & acknowledgments](#license)

---

## Quick reference (implementers & AI agents)

Use this block as a **single source of truth** before writing or generating code.

| Fact | Correct value |
|------|----------------|
| **Cargo package name** (in `[dependencies]`) | `rust-langgraph` (hyphen) |
| **Rust crate path** (in `use`) | `rust_langgraph` (underscore) |
| **Wrong** | `langgraph::` — that is not this crate’s name |
| **Async runtime** | **Tokio** required (`#[tokio::main]` or equivalent) |
| **Edition** | Rust 2021 |
| **Default Cargo features** | `memory-checkpoint` (enables `MemorySaver`) |
| **LLM modules** | **Not** in the build unless you add the matching feature |

**Rule:** If you `use rust_langgraph::llm::ollama::...`, your `Cargo.toml` **must** include `features = ["ollama"]` (same for `openai`, `openrouter`, `anthropic`). If you use `create_react_agent` / `Tool`, you **must** enable `prebuilt` **and** at least one LLM feature for a real model.

**Human + agent doc:** [`AGENTS.md`](AGENTS.md) — patterns, signatures, and pitfalls in compact form.

---

## What this crate is

**Rust LangGraph** (crate name: **`rust-langgraph`**, Rust import: **`rust_langgraph`**) helps you build **stateful workflows** as a **directed graph**:

- **Nodes** are async functions (or types implementing `Node`) that read and return **state**.
- **Edges** connect nodes: fixed edges or **conditional** edges that choose the next node from state.
- **Execution** follows a Pregel-style loop: run nodes, merge state, optionally **checkpoint**, repeat until done.

Use it for multi-step LLM apps, tool-calling agents, branching pipelines, and anything that fits “steps + shared state + optional loops.”

---

## Who should use it

| You want… | Use… |
|-----------|------|
| A small graph without LLMs | `StateGraph` + custom `State` |
| Chat + tools (ReAct-style) | `prebuilt` + an LLM feature: `create_react_agent`, `Tool`, `ToolNode` |
| Local models | `ollama` → `llm::ollama::OllamaAdapter` |
| OpenAI API | `openai` → `llm::openai::OpenAIAdapter` |
| [OpenRouter](https://openrouter.ai/docs/quickstart) (many providers, one API) | `openrouter` → `llm::openrouter::OpenRouterAdapter` |
| Anthropic API | `anthropic` → `llm::anthropic::AnthropicAdapter` |
| Persistence between runs | `MemorySaver` (default feature) or `sqlite` / `postgres` |

---

## Installation

### From crates.io (use in another project)

**[`rust-langgraph` on crates.io](https://crates.io/crates/rust-langgraph)** — add it like any other dependency:

```bash
cargo add rust-langgraph
# Optional features, e.g. Ollama + ReAct:
# cargo add rust-langgraph --features ollama,prebuilt
```

Rustdoc is on **[docs.rs/rust-langgraph](https://docs.rs/rust-langgraph)** (may take a few minutes to build right after the first publish).

**Minimal `Cargo.toml`** (graph core only — checkpoints in memory):

```toml
[dependencies]
rust-langgraph = "0.1"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
futures = "0.3"  # for StreamExt when using CompiledGraph::stream
```

**Import:**

```rust
use rust_langgraph::prelude::*;
```

Enable optional features as needed (see [Copy-paste recipes](#copy-paste-cargotoml-recipes) and [Feature flags](#feature-flags-detailed)).

**Requirements:**

- Rust 2021
- **Tokio** — the library is async-first

---

## Copy-paste `Cargo.toml` recipes

Replace version pins if your workspace pins differently.

### Graph + in-memory checkpoints only (default)

```toml
[dependencies]
rust-langgraph = "0.1"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
futures = "0.3"
```

### + Ollama (local HTTP API)

```toml
rust-langgraph = { version = "0.1", features = ["ollama"] }
```

### + OpenAI (`OPENAI_API_KEY` for `OpenAIAdapter::new`)

```toml
rust-langgraph = { version = "0.1", features = ["openai"] }
```

### + OpenRouter ([quickstart](https://openrouter.ai/docs/quickstart))

```toml
rust-langgraph = { version = "0.1", features = ["openrouter"] }
```

### + Anthropic (pass key via `AnthropicAdapter::with_api_key` — no standard env in adapter)

```toml
rust-langgraph = { version = "0.1", features = ["anthropic"] }
```

### ReAct agent (tools + graph) + Ollama

```toml
rust-langgraph = { version = "0.1", features = ["ollama", "prebuilt"] }
```

### ReAct + OpenRouter

```toml
rust-langgraph = { version = "0.1", features = ["openrouter", "prebuilt"] }
```

### All optional LLM adapters (for examples or experimentation)

```toml
rust-langgraph = { version = "0.1", features = [
  "ollama", "openai", "openrouter", "anthropic", "prebuilt"
] }
```

### SQLite checkpoints

```toml
rust-langgraph = { version = "0.1", features = ["sqlite"] }
```

### PostgreSQL checkpoints

```toml
rust-langgraph = { version = "0.1", features = ["postgres"] }
```

---

## Environment variables

| Variable | Used by | Notes |
|----------|---------|--------|
| `OPENAI_API_KEY` | `OpenAIAdapter::new(...)` | `with_api_key` bypasses env |
| `OPENROUTER_API_KEY` | `OpenRouterAdapter::new(...)` | `with_api_key` bypasses env |
| *(none by default)* | `AnthropicAdapter` | Use `AnthropicAdapter::with_api_key("sk-ant-...")` |
| *(none by default)* | `OllamaAdapter` | Default base `http://localhost:11434`; override with `with_base_url` |

Set secrets in the environment or inject keys explicitly in code — do not commit API keys.

---

## Five-minute tutorial

### 1. Define state

State must implement [`State`](https://docs.rs/rust-langgraph/latest/rust_langgraph/state/trait.State.html): `Clone`, `Serialize`/`Deserialize`, `Debug`, and **`merge`** (how updates from multiple nodes combine).

```rust
use rust_langgraph::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AppState {
    n: i32,
}

impl State for AppState {
    fn merge(&mut self, other: Self) -> Result<()> {
        self.n += other.n;
        Ok(())
    }
}
```

### 2. Build a graph

```rust
let mut graph = StateGraph::new();

graph.add_node("step", |state: AppState, _config: &Config| async move {
    Ok(AppState { n: state.n + 1 })
});

graph.set_entry_point("step");
graph.set_finish_point("step");

let mut app = graph.compile(None)?;
let out = app.invoke(AppState { n: 0 }, Config::default()).await?;
// out.n == 1
```

### 3. Conditional routing (optional)

Use `pregel::BranchResult`: `single("node_id")`, `end()`, or more advanced variants.

```rust
use rust_langgraph::pregel::BranchResult;

graph.add_conditional_edges("step", |state: &AppState| {
    let n = state.n;
    async move {
        if n >= 3 {
            Ok(BranchResult::end())
        } else {
            Ok(BranchResult::single("step"))
        }
    }
});
```

---

## Core concepts

### Mental model

1. You declare **nodes** by name and pass a closure or a type implementing `Node<S>`.
2. You connect nodes with **`add_edge`** or **`add_conditional_edges`**.
3. You set **`set_entry_point`** (where execution starts) and usually **`set_finish_point`** (terminal nodes).
4. **`compile`** produces a **`CompiledGraph`** you call **`invoke`** or **`stream`** on.

### State

- **`State`** — your domain data; **`merge`** defines reducer semantics when multiple writes occur.
- **`MessagesState`** — built-in chat history for LLM flows (`messages: Vec<Message>`).
- **`Message`**, **`ToolCall`** — roles `user`, `assistant`, `system`, `tool`; tool calls and tool results.

### Graph types

| Type | Role |
|------|------|
| `StateGraph<S>` | Builder: `add_node`, `add_edge`, `add_conditional_edges`, `compile` |
| `CompiledGraph<S>` | Runnable: `invoke`, `stream`, checkpoint helpers when configured |

### Checkpointing

Pass a **`BaseCheckpointSaver`** (e.g. **`MemorySaver`** with feature `memory-checkpoint`) into **`compile(Some(checkpointer))`**. Use **`Config::with_thread_id`** so each conversation/thread has isolated checkpoints.

### Streaming

Use **`CompiledGraph::stream`** with **`StreamMode`** and handle **`StreamEvent`** variants. Add **`futures`** to your app for **`StreamExt`**:

```rust
use futures::StreamExt;
use rust_langgraph::prelude::*;

// let mut app: CompiledGraph<MyState> = ...;
let mut stream = app
    .stream(initial_state, config, StreamMode::Values)
    .await?;

while let Some(event) = stream.next().await {
    match event? {
        StreamEvent::Values { data, .. } => {
            println!("{:?}", data);
        }
        _ => {}
    }
}
```

For token-level LLM streams, call **`ChatModel::stream`** on **`OllamaAdapter`** / **`OpenAIAdapter`** / **`OpenRouterAdapter`** / **`AnthropicAdapter`** (with the matching feature enabled).

---

## Graph API reference

| Method | Purpose |
|--------|---------|
| `StateGraph::new()` | Empty graph |
| `add_node(name, node)` | Register a node (`impl Node<S>` or closure) |
| `add_edge(from, to)` | Always go `from → to` |
| `add_conditional_edges(from, branch)` | `branch` returns `BranchResult` (next node(s) or end) |
| `set_entry_point(name)` | First node(s) to run |
| `set_finish_point(name)` | Mark terminal nodes |
| `compile(checkpointer)` | Build `CompiledGraph` |

**Node closure shape:**

```rust
|state: S, config: &Config| async move { Result<S> }
```

Use an explicit `&Config` parameter (not `_`) if the compiler complains about lifetimes in complex graphs.

---

## LLMs and agents

Enable features: **`ollama`**, **`openai`**, **`openrouter`**, **`anthropic`**, and often **`prebuilt`** for agents.

### Direct chat (no graph)

**Local (Ollama):**

```rust
use rust_langgraph::llm::ollama::OllamaAdapter;
use rust_langgraph::llm::ChatModel;
use rust_langgraph::state::Message;

let model = OllamaAdapter::new("llama3.1:8b");
let reply = model.invoke(&[Message::user("Hello")]).await?;
```

**OpenRouter** — set `OPENROUTER_API_KEY` and use a router model id (e.g. `openai/gpt-4o-mini`):

```rust
use rust_langgraph::llm::openrouter::OpenRouterAdapter;
use rust_langgraph::llm::ChatModel;
use rust_langgraph::state::Message;

let model = OpenRouterAdapter::with_api_key(
    "openai/gpt-4o-mini",
    std::env::var("OPENROUTER_API_KEY").unwrap(),
);
let reply = model.invoke(&[Message::user("Hello")]).await?;
```

### ReAct agent (graph with `agent` ↔ `tools` loop)

1. Define **`Tool`** instances with **`Tool::new(...).with_schema(json_schema)`**.
2. Bind the same tools to the model (e.g. **`OllamaAdapter::with_tools(vec![t.to_tool_info(), ...])`** or **`OpenAIAdapter::bind_tools` / `OpenRouterAdapter::bind_tools`**).
3. Call **`create_react_agent(model, tools)`** → get a **`CompiledGraph<MessagesState>`** (requires **`prebuilt`**).
4. **`invoke(MessagesState { messages: vec![Message::user("...")] }, Config::default())`**.

See **`examples/06_react_agent_ollama.rs`** for a full runnable flow.

### Validation

**`prebuilt::validate_chat_history`** checks that every assistant **`tool_calls`** entry has a matching **`tool`** message (aligned with common LangGraph-style rules).

---

## Feature flags (detailed)

```toml
rust-langgraph = { version = "0.1", features = ["ollama", "prebuilt", "openai", "openrouter"] }
```

| Feature | Enables | Pulls in (transitively) |
|---------|---------|-------------------------|
| `memory-checkpoint` | **Default.** In-memory `MemorySaver` | (no extra crates beyond core) |
| `sqlite` | SQLite checkpoint backend | `sqlx` + SQLite |
| `postgres` | PostgreSQL checkpoint backend | `sqlx` + Postgres |
| `openai` | `llm::openai::OpenAIAdapter` | `reqwest`, `async-openai` |
| `openrouter` | `llm::openrouter::OpenRouterAdapter` | `reqwest`, `async-openai` |
| `anthropic` | `llm::anthropic::AnthropicAdapter` | `reqwest` |
| `ollama` | `llm::ollama::OllamaAdapter` | `reqwest` |
| `prebuilt` | `create_react_agent`, `Tool`, `ToolNode`, `validate_chat_history` | (no extra deps) |

**Import ↔ feature gate:**

| You import | Required feature |
|------------|------------------|
| `rust_langgraph::llm::ollama::*` | `ollama` |
| `rust_langgraph::llm::openai::*` | `openai` |
| `rust_langgraph::llm::openrouter::*` | `openrouter` |
| `rust_langgraph::llm::anthropic::*` | `anthropic` |
| `rust_langgraph::prelude::ChatModel` | one of `ollama`, `openai`, `openrouter`, `anthropic` |
| `rust_langgraph::prelude::{create_react_agent, Tool, ToolNode}` | `prebuilt` |
| `rust_langgraph::prelude::MemorySaver` | `memory-checkpoint` (default) |

---

## Prelude and conditional exports

```rust
use rust_langgraph::prelude::*;
```

**Always available (with default features):** `Config`, `Error`, `Result`, `State`, `MessagesState`, `Message`, `add_messages`, `Node`, `StateGraph`, `CompiledGraph`, `Checkpoint`, `BaseCheckpointSaver`, `StreamMode`, `StreamEvent`, `Send`, `Command`, and **`MemorySaver`** if `memory-checkpoint` is on.

**If `prebuilt`:** `create_react_agent`, `Tool`, `ToolNode`.

**If any LLM feature (`openai` \| `openrouter` \| `anthropic` \| `ollama`):** `ChatModel` in the prelude.

Otherwise import traits explicitly, e.g. `use rust_langgraph::llm::ChatModel` only compiles when an LLM feature is enabled.

---

## Common mistakes & compile errors

| Symptom | Cause | Fix |
|---------|--------|-----|
| `could not find llm::ollama` | Feature off | Add `features = ["ollama"]` (or the adapter you need) |
| `ChatModel` not found in prelude | No LLM feature | Enable `ollama`, `openai`, `openrouter`, or `anthropic` |
| `create_react_agent` not found | Feature off | Add `features = ["prebuilt"]` |
| Wrong crate in `use` | Confusion with Python | Use **`rust_langgraph`**, not `langgraph` |
| Lifetime errors in conditional edges | Capturing `&state` into `async move` | Clone needed fields before the async block (see `AGENTS.md`) |
| `invoke` borrow errors | Missing `mut` | `let mut app = graph.compile(...)?` |
| Example fails to link | Wrong features | Use the `--features` from the [examples table](#examples) |

---

## Verification commands

From the crate root (`rust-langgraph/`):

```bash
cargo check -p rust-langgraph
cargo check -p rust-langgraph --all-features
cargo test -p rust-langgraph --lib
cargo doc -p rust-langgraph --no-deps --open
```

**Integration tests** (real Ollama server; marked `ignore`):

```bash
cargo test -p rust-langgraph --test test_ollama_integration --features ollama,prebuilt -- --ignored
```

**Run a single example:**

```bash
cargo run -p rust-langgraph --example simple_graph
cargo run -p rust-langgraph --example ollama_chat --features ollama
```

---

## Project layout

```
src/
  lib.rs              # Crate root, prelude
  graph/              # StateGraph, CompiledGraph
  pregel/             # Execution engine, Branch, BranchResult
  state.rs            # State, Message, MessagesState
  nodes.rs            # Node trait
  checkpoint/         # Checkpoint types & saver trait
  checkpoint_backends/
  llm/                # ChatModel, Ollama / OpenAI / OpenRouter / Anthropic (feature-gated)
  prebuilt/           # ReAct agent, tools (feature-gated)
examples/             # Runnable examples (see table below)
tests/                # Integration tests (e.g. Ollama, --ignored)
AGENTS.md             # Short agent/contributor cheat sheet
```

**API reference:** [docs.rs/rust-langgraph](https://docs.rs/rust-langgraph) or `cargo doc --open`.

---

## Examples

| Example | Command | Features |
|---------|---------|----------|
| Minimal graph | `cargo run --example simple_graph` | default |
| Branching | `cargo run --example conditional_edges` | default |
| Checkpoints | `cargo run --example checkpointing` | default |
| Streaming | `cargo run --example streaming` | default |
| Ollama chat | `cargo run --example ollama_chat` | `ollama` |
| ReAct + Ollama | `cargo run --example react_agent_ollama` | `ollama`, `prebuilt` |
| OpenRouter chat | `cargo run --example openrouter_chat` | `openrouter` |
| Custom state | `cargo run --example custom_state` | default |

```bash
cd rust-langgraph
cargo run --example simple_graph
cargo run --example ollama_chat --features ollama
cargo run --example react_agent_ollama --features ollama,prebuilt

# OpenRouter (Windows PowerShell)
$env:OPENROUTER_API_KEY = "sk-or-v1-..."
cargo run --example openrouter_chat --features openrouter

# OpenRouter (Unix)
export OPENROUTER_API_KEY=sk-or-v1-...
cargo run --example openrouter_chat --features openrouter
```

---

## Documentation

- **README (this file)** — install, env vars, features, recipes, troubleshooting.
- **[`AGENTS.md`](AGENTS.md)** — condensed rules for contributors and **AI coding agents** (naming, signatures, pitfalls).
- **Rustdoc** — `cargo doc -p rust-langgraph --no-deps --open`.

The crate’s `repository` in `Cargo.toml` points to [github.com/kareem2002-k/rust-langgraph](https://github.com/kareem2002-k/rust-langgraph); change it if you maintain a fork.

---

## Comparison with Python LangGraph

This crate targets **similar ideas** (state graph, checkpoints, agents) but is a **separate implementation**. APIs and wire formats are aligned where practical; behavior may differ in edge cases. For the official Python stack, use LangChain’s LangGraph.

| Area | Rust LangGraph | Python LangGraph |
|------|----------------|------------------|
| Language | Rust | Python |
| Package | `rust-langgraph` / `rust_langgraph` | `langgraph` |
| Official? | Community | LangChain |

---

## Contributing

Issues and PRs are welcome. Please keep changes focused and match existing style.

---

## License

MIT — see [LICENSE](LICENSE).

## Acknowledgments

- Inspired by [LangGraph](https://github.com/langchain-ai/langgraph) (LangChain).
- Execution model influenced by Google’s Pregel.

## Links

- [docs.rs/rust-langgraph](https://docs.rs/rust-langgraph)
- [Examples](examples/)
