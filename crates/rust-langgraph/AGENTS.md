# Agent & assistant guide — `rust-langgraph`

This file is for **AI coding agents** and **human contributors** who need a precise mental model of the crate without reading the entire tree.

---

## 1. Identity (do not confuse)

| Item | Value |
|------|--------|
| **Cargo package name** | `rust-langgraph` (in `Cargo.toml` `[dependencies]`) |
| **Rust crate import** | `use rust_langgraph::...` (underscores) |
| **NOT** | `langgraph` as the crate name — that would imply the official Python/LangChain product |

Always generate **`rust_langgraph`**, never `langgraph::`, in Rust code.

---

## 2. Prelude (default imports)

```rust
use rust_langgraph::prelude::*;
```

Exposes: `Config`, `Error`, `Result`, `State`, `MessagesState`, `Message`, `Node`, `StateGraph`, `CompiledGraph`, `Checkpoint`, `BaseCheckpointSaver`, `StreamMode`, `StreamEvent`, `Send`, `Command`, optional `MemorySaver`, optional `create_react_agent` / `Tool` / `ToolNode`, optional `ChatModel`.

**Feature-gated:** `MemorySaver` needs `memory-checkpoint` (default). Prebuilt agents need `prebuilt`. `ChatModel` in prelude needs one of `openai` | `openrouter` | `anthropic` | `ollama`.

---

## 3. `State` trait bounds

Graph state `S` must implement:

- `State` from this crate
- Typically `Clone + Serialize + Deserialize + Debug` (see `state.rs` for the exact bound)

Implement **`merge(&mut self, other: Self)`** — defines how parallel or sequential node outputs combine.

---

## 4. Building graphs

**Pattern:**

```rust
let mut graph = StateGraph::<MyState>::new();
graph.add_node("id", |s: MyState, cfg: &Config| async move { Ok(s) });
graph.set_entry_point("id");
graph.set_finish_point("id"); // optional but common
let mut app = graph.compile(None)?; // or Some(checkpointer)
let out = app.invoke(initial, Config::default()).await?;
```

**`CompiledGraph::invoke`** takes **`&mut self`**.

**Conditional edges:** closure signature is `Fn(&S) -> Future<Output = Result<BranchResult>>` + `Send + Sync`. If the async block captures state, **clone the needed fields before `async move`** to avoid lifetime errors (see `prebuilt/react_agent.rs`).

**BranchResult:** `BranchResult::single("node")`, `BranchResult::end()`, etc. (`pregel::branch.rs`).

---

## 5. Nodes

- Closures: `|state: S, &Config| async move -> Result<S>`
- Structs: `impl Node<S>` with `async fn invoke(&self, state: S, config: &Config) -> Result<S>` (`async_trait`)

Do not confuse with **`ChatModel`** — LLMs implement `ChatModel`, not `Node`, unless you wrap them.

---

## 6. LLM adapters (feature-gated)

| Module | Feature | Type |
|--------|---------|------|
| `rust_langgraph::llm::ollama` | `ollama` | `OllamaAdapter` |
| `rust_langgraph::llm::openai` | `openai` | `OpenAIAdapter` |
| `rust_langgraph::llm::openrouter` | `openrouter` | `OpenRouterAdapter` ([OpenRouter](https://openrouter.ai/docs/quickstart), OpenAI-compatible base URL) |
| `rust_langgraph::llm::anthropic` | `anthropic` | `AnthropicAdapter` |

**ChatModel** methods: `invoke(&[Message])`, `stream(...)`, `name()`, **`clone_box()`** for trait objects.

**Tools:** build `ToolInfo` or use **`Tool::to_tool_info()`** after **`with_schema(json)`**. Bind tools with **`OllamaAdapter::with_tools`**, **`OpenAIAdapter::bind_tools`**, or **`OpenRouterAdapter::bind_tools`** (same OpenAI-style tool schema).

**Messages:** `Message::user`, `Message::assistant`, `Message::tool`, `with_tool_calls`, roles are **strings**: `"user"`, `"assistant"`, `"tool"`, `"system"`.

---

## 7. Prebuilt ReAct agent

**Requires:** `prebuilt` + an LLM feature for real runs.

```rust
use rust_langgraph::prebuilt::{create_react_agent, Tool};
// create_react_agent(model, vec![tool1, tool2])? 
```

Internally builds a **`StateGraph<MessagesState>`** with nodes **`agent`** and **`tools`**, conditional edge from **`agent`**, edge **`tools` → `agent`**.

User must pass a model **already bound with the same tools** as passed to `create_react_agent` (e.g. `with_tools` / `bind_tools`).

---

## 8. Checkpoints

- Trait: **`BaseCheckpointSaver`**
- In-memory: **`MemorySaver`** (`checkpoint_backends::memory`), feature **`memory-checkpoint`**
- Pass **`Some(Arc::new(MemorySaver::new()))`** to **`compile`**
- Use **`Config::with_thread_id`** for isolation

---

## 9. Common mistakes (avoid when generating code)

1. Wrong crate name: `langgraph::` instead of `rust_langgraph::`
2. Missing features: using `llm::ollama` without `ollama` in `Cargo.toml`
3. Missing `prebuilt` when using `create_react_agent` / `Tool`
4. **`State`** not implementing serde/debug bounds where required
5. Conditional branch closure capturing `&state` across `async move` — **clone first**
6. Forgetting **`mut`** on **`CompiledGraph`** when calling **`invoke`**

---

## 10. Verification commands

```bash
cargo check -p rust-langgraph
cargo check -p rust-langgraph --all-features
cargo test -p rust-langgraph --lib
cargo doc -p rust-langgraph --no-deps --open
```

LLM integration tests (need local Ollama):

```bash
cargo test -p rust-langgraph --test test_ollama_integration --features ollama,prebuilt -- --ignored
```

---

## 11. File map (where to look)

| Concern | Path |
|---------|------|
| Graph builder | `src/graph/state_graph.rs` |
| Pregel loop | `src/pregel/engine.rs` |
| Branching | `src/pregel/branch.rs` |
| State / messages | `src/state.rs` |
| Errors | `src/errors.rs` |
| ReAct + tools | `src/prebuilt/react_agent.rs`, `src/prebuilt/tool.rs` |
| Validation | `src/prebuilt/validation.rs` |
| LLM | `src/llm/*.rs` |

---

## 12. When to suggest alternatives

- User needs **official** LangChain/LangGraph support → point to Python LangGraph / LangChain docs.
- User needs only raw HTTP to an LLM → they can use adapters standalone **without** a graph.
- User needs a **single** LLM call → **`ChatModel::invoke`** is enough; **no `StateGraph`** required.

---

*End of AGENTS.md*
