//! Minimal OpenRouter chat — uses the OpenAI-compatible API at openrouter.ai.
//!
//! Set `OPENROUTER_API_KEY` and optionally `OPENROUTER_MODEL` (default: `openai/gpt-4o-mini`).
//!
//! ```bash
//! set OPENROUTER_API_KEY=sk-or-v1-...
//! cargo run --example openrouter_chat --features openrouter
//! ```
//!
//! Docs: <https://openrouter.ai/docs/quickstart>

use rust_langgraph::errors::Result;
use rust_langgraph::llm::openrouter::OpenRouterAdapter;
use rust_langgraph::llm::ChatModel;
use rust_langgraph::state::Message;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENROUTER_API_KEY").map_err(|_| {
        rust_langgraph::Error::execution(
            "Set OPENROUTER_API_KEY (see https://openrouter.ai/docs/quickstart)",
        )
    })?;

    let model = std::env::var("OPENROUTER_MODEL")
        .unwrap_or_else(|_| "openai/gpt-4o-mini".to_string());

    println!("OpenRouter model: {}\n", model);

    let adapter = OpenRouterAdapter::with_api_key(&model, api_key);
    let messages = vec![Message::user("Reply in one short sentence: what is Rust?")];

    let response = adapter.invoke(&messages).await?;
    println!("{}", response.content);

    assert_eq!(response.role, "assistant");
    Ok(())
}
