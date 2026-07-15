//! Simple Ollama chat example demonstrating basic LLM integration.
//!
//! This example shows how to use the OllamaAdapter to have a simple
//! conversation with a local Ollama model.
//!
//! # Requirements
//!
//! - Ollama must be running locally (default: http://localhost:11434)
//! - A model must be available (e.g., `ollama pull llama3.1:8b`)
//!
//! # Usage
//!
//! ```bash
//! cargo run --example 05_ollama_chat --features ollama
//! ```
//!
//! Or with custom settings:
//!
//! ```bash
//! OLLAMA_MODEL=llama2 OLLAMA_BASE_URL=http://127.0.0.1:11434 \
//!     cargo run --example 05_ollama_chat --features ollama
//! ```

use rust_langgraph::llm::ollama::OllamaAdapter;
use rust_langgraph::llm::ChatModel;
use rust_langgraph::state::Message;
use rust_langgraph::errors::Result;

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Ollama Chat Example ===\n");

    // Get Ollama settings from env or use defaults
    let model = std::env::var("OLLAMA_MODEL")
        .unwrap_or_else(|_| "llama3.1:8b".to_string());
    let base_url = std::env::var("OLLAMA_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:11434".to_string());

    println!("Using Ollama at {}, model {}\n", base_url, model);

    // Create adapter
    let adapter = OllamaAdapter::with_base_url(&model, &base_url);

    // Simple direct call (no graph needed for single message)
    let messages = vec![Message::user("What is Rust programming language in one sentence?")];

    println!("User: {}\n", messages[0].content);
    println!("Calling Ollama...\n");

    match adapter.invoke(&messages).await {
        Ok(response) => {
            println!("Assistant: {}\n", response.content);
            
            // Verify response structure
            assert_eq!(response.role, "assistant");
            assert!(!response.content.is_empty());
            
            println!("✓ Success! Received valid response from Ollama");
        }
        Err(e) => {
            eprintln!("✗ Error calling Ollama: {}", e);
            eprintln!("\nTroubleshooting:");
            eprintln!("  1. Make sure Ollama is running: ollama serve");
            eprintln!("  2. Make sure the model is available: ollama pull {}", model);
            eprintln!("  3. Check the base URL: {}", base_url);
            return Err(e);
        }
    }

    Ok(())
}
