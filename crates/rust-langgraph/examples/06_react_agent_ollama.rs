//! ReAct agent with Ollama and tool calling.
//!
//! This example demonstrates a complete ReAct (Reasoning and Acting) agent
//! using Ollama with real tool calling capabilities.
//!
//! # Requirements
//!
//! - Ollama must be running locally
//! - A model with tool calling support (e.g., llama3.1:8b, llama3.2, mistral)
//!
//! # Usage
//!
//! ```bash
//! cargo run --example 06_react_agent_ollama --features ollama,prebuilt
//! ```

use rust_langgraph::errors::Result;
use rust_langgraph::llm::ollama::OllamaAdapter;
use rust_langgraph::prebuilt::{create_react_agent, Tool};
use rust_langgraph::state::{Message, MessagesState};
use rust_langgraph::config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== ReAct Agent with Ollama ===\n");

    // Create tools
    let search = Tool::new(
        "search",
        "Search the web for information",
        |args| async move {
            let query = args["query"].as_str().unwrap_or("unknown");
            println!("  [TOOL] Searching for: {}", query);
            
            // Simulate search results
            Ok(serde_json::json!({
                "results": format!(
                    "Search results for '{}': Found 5 articles about Rust programming, \
                    including official docs at rust-lang.org and tutorials on various platforms.",
                    query
                )
            }))
        },
    ).with_schema(serde_json::json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "The search query"
            }
        },
        "required": ["query"]
    }));

    let calculator = Tool::new(
        "calculator",
        "Evaluate mathematical expressions",
        |args| async move {
            let expression = args["expression"].as_str().unwrap_or("0");
            println!("  [TOOL] Calculating: {}", expression);
            
            // Simple calculator (in real code use a proper expression parser)
            let result = match expression {
                expr if expr.contains("+") => {
                    let parts: Vec<&str> = expr.split('+').collect();
                    if parts.len() == 2 {
                        let a: f64 = parts[0].trim().parse().unwrap_or(0.0);
                        let b: f64 = parts[1].trim().parse().unwrap_or(0.0);
                        a + b
                    } else {
                        0.0
                    }
                }
                expr if expr.contains("*") => {
                    let parts: Vec<&str> = expr.split('*').collect();
                    if parts.len() == 2 {
                        let a: f64 = parts[0].trim().parse().unwrap_or(0.0);
                        let b: f64 = parts[1].trim().parse().unwrap_or(0.0);
                        a * b
                    } else {
                        0.0
                    }
                }
                _ => expression.parse().unwrap_or(0.0),
            };
            
            Ok(serde_json::json!({
                "result": result
            }))
        },
    ).with_schema(serde_json::json!({
        "type": "object",
        "properties": {
            "expression": {
                "type": "string",
                "description": "Math expression to evaluate (e.g., '15 * 7' or '10 + 5')"
            }
        },
        "required": ["expression"]
    }));

    // Extract tool schemas for LLM
    let tool_infos = vec![search.to_tool_info(), calculator.to_tool_info()];

    // Create Ollama adapter WITH tools bound
    let model = std::env::var("OLLAMA_MODEL")
        .unwrap_or_else(|_| "llama3.1:8b".to_string());
    
    println!("Using model: {}\n", model);
    
    let adapter = OllamaAdapter::new(&model).with_tools(tool_infos);

    // Create ReAct agent
    let mut agent = create_react_agent(adapter, vec![search, calculator])?;

    // Run agent task
    let input = MessagesState {
        messages: vec![
            Message::user("What is 15 * 7? Then search for information about Rust programming.")
        ],
    };

    println!("User: {}\n", input.messages[0].content);
    println!("Running agent...\n");

    match agent.invoke(input, Config::default()).await {
        Ok(result) => {
            println!("\n=== Full Conversation ===");
            for (i, msg) in result.messages.iter().enumerate() {
                println!("\n[{}] {}: {}", i + 1, msg.role, msg.content);

                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        println!("    └─ Tool Call: {}({})", tc.name, tc.arguments);
                    }
                }

                if let Some(tool_call_id) = &msg.tool_call_id {
                    println!("    └─ Tool Response ID: {}", tool_call_id);
                }
            }

            // Extract final answer (last assistant message)
            if let Some(final_msg) = result.messages.iter()
                .rev()
                .find(|m| m.role == "assistant")
            {
                println!("\n=== Final Answer ===");
                println!("{}", final_msg.content);
            }

            println!("\n✓ Agent completed successfully!");
        }
        Err(e) => {
            eprintln!("\n✗ Error running agent: {}", e);
            eprintln!("\nTroubleshooting:");
            eprintln!("  1. Make sure Ollama is running: ollama serve");
            eprintln!("  2. Make sure the model supports tool calling: {}", model);
            eprintln!("  3. Try using llama3.1:8b or llama3.2 which have good tool support");
            return Err(e);
        }
    }

    Ok(())
}
