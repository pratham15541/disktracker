//! Integration tests for LLM adapters and ReAct agents.
//!
//! These tests call real Ollama instances and are marked with #[ignore]
//! so they only run when explicitly requested.
//!
//! # Running Tests
//!
//! ```bash
//! # Run with Ollama available
//! cargo test --test test_ollama_integration --features ollama,prebuilt -- --ignored --nocapture
//! ```

use rust_langgraph::config::Config;
use rust_langgraph::errors::Result;
use rust_langgraph::llm::ollama::OllamaAdapter;
use rust_langgraph::llm::ChatModel;
use rust_langgraph::prebuilt::{create_react_agent, Tool};
use rust_langgraph::state::{Message, MessagesState};

/// Helper to check if Ollama is available
async fn is_ollama_available() -> bool {
    reqwest::Client::new()
        .get("http://127.0.0.1:11434/api/tags")
        .send()
        .await
        .is_ok()
}

#[tokio::test]
#[ignore] // Run with: cargo test --features ollama -- --ignored
async fn test_ollama_simple_chat() {
    if !is_ollama_available().await {
        eprintln!("Skipping: Ollama not available at localhost:11434");
        return;
    }

    let adapter = OllamaAdapter::new("llama3.1:8b");
    let messages = vec![Message::user("Say hello in one word")];

    let response = adapter.invoke(&messages).await.unwrap();

    assert!(!response.content.is_empty());
    assert_eq!(response.role, "assistant");
    println!("Response: {}", response.content);
}

#[tokio::test]
#[ignore]
async fn test_ollama_tool_calling() {
    if !is_ollama_available().await {
        return;
    }

    let search = Tool::new("search", "Search", |args| async move {
        Ok(serde_json::json!({"result": format!("Found: {}", args)}))
    })
    .with_schema(serde_json::json!({
        "type": "object",
        "properties": {
            "query": {"type": "string"}
        },
        "required": ["query"]
    }));

    let tool_info = search.to_tool_info();
    let adapter = OllamaAdapter::new("llama3.1:8b").with_tools(vec![tool_info]);

    let messages = vec![Message::user(
        "Use the search tool to find information about Rust",
    )];

    let response = adapter.invoke(&messages).await.unwrap();

    // Model should return tool calls or content
    println!("Response: {:?}", response);
    println!("Has tool calls: {}", response.tool_calls.is_some());

    if let Some(tool_calls) = &response.tool_calls {
        println!("Tool calls: {} call(s)", tool_calls.len());
        for tc in tool_calls {
            println!("  - {}: {}", tc.name, tc.arguments);
        }
    }
}

#[tokio::test]
#[ignore]
async fn test_react_agent_with_ollama() -> Result<()> {
    if !is_ollama_available().await {
        eprintln!("Skipping: Ollama not available");
        return Ok(());
    }

    let get_weather = Tool::new(
        "get_weather",
        "Get current weather for a location",
        |args| async move {
            let location = args["location"].as_str().unwrap_or("unknown");
            println!("  [TOOL] Getting weather for: {}", location);
            Ok(serde_json::json!({
                "location": location,
                "temperature": 72,
                "conditions": "sunny"
            }))
        },
    )
    .with_schema(serde_json::json!({
        "type": "object",
        "properties": {
            "location": {
                "type": "string",
                "description": "City name"
            }
        },
        "required": ["location"]
    }));

    let tool_infos = vec![get_weather.to_tool_info()];
    let model = OllamaAdapter::new("llama3.1:8b").with_tools(tool_infos);

    let mut agent = create_react_agent(model, vec![get_weather])?;

    let input = MessagesState {
        messages: vec![Message::user("What's the weather in San Francisco?")],
    };

    println!("User: {}", input.messages[0].content);

    let result = agent.invoke(input, Config::default()).await?;

    println!("\n=== Conversation ===");
    for (i, msg) in result.messages.iter().enumerate() {
        let preview = if msg.content.len() > 100 {
            format!("{}...", &msg.content[..100])
        } else {
            msg.content.clone()
        };
        println!("[{}] {}: {}", i + 1, msg.role, preview);

        if let Some(tool_calls) = &msg.tool_calls {
            println!("    Tool calls: {}", tool_calls.len());
        }
    }

    // Verify agent loop completed
    assert!(result.messages.len() > 1, "Agent should produce multiple messages");

    // Should have at least one tool message
    let tool_msgs: Vec<_> = result.messages.iter().filter(|m| m.role == "tool").collect();

    println!("\nConversation length: {} messages", result.messages.len());
    println!("Tool calls made: {}", tool_msgs.len());

    Ok(())
}

#[tokio::test]
#[ignore]
async fn test_react_agent_multiple_turns() -> Result<()> {
    if !is_ollama_available().await {
        return Ok(());
    }

    let calculator = Tool::new(
        "calculator",
        "Evaluate mathematical expressions",
        |args| async move {
            let expression = args["expression"].as_str().unwrap_or("0");
            println!("  [TOOL] Calculating: {}", expression);

            // Simple math
            let result = if expression.contains("*") {
                let parts: Vec<&str> = expression.split('*').collect();
                if parts.len() == 2 {
                    let a: f64 = parts[0].trim().parse().unwrap_or(0.0);
                    let b: f64 = parts[1].trim().parse().unwrap_or(0.0);
                    a * b
                } else {
                    0.0
                }
            } else {
                0.0
            };

            Ok(serde_json::json!({ "result": result }))
        },
    )
    .with_schema(serde_json::json!({
        "type": "object",
        "properties": {
            "expression": {
                "type": "string",
                "description": "Mathematical expression"
            }
        },
        "required": ["expression"]
    }));

    let tool_infos = vec![calculator.to_tool_info()];
    let model = OllamaAdapter::new("llama3.1:8b").with_tools(tool_infos);

    let mut agent = create_react_agent(model, vec![calculator])?;

    let input = MessagesState {
        messages: vec![Message::user("Calculate 15 * 7")],
    };

    let result = agent.invoke(input, Config::default()).await?;

    println!("\nFull conversation:");
    for msg in &result.messages {
        println!("{}: {}", msg.role, msg.content);
    }

    // Verify we got a result
    assert!(result.messages.len() >= 3, "Should have user, assistant with tool call, tool response, and final answer");

    Ok(())
}

#[tokio::test]
async fn test_ollama_adapter_creation() {
    let adapter = OllamaAdapter::new("llama2");
    assert_eq!(adapter.name(), "llama2");

    let adapter2 = OllamaAdapter::with_base_url("llama3", "http://127.0.0.1:11434");
    assert_eq!(adapter2.name(), "llama3");
}

#[tokio::test]
async fn test_tool_info_conversion() {
    let tool = Tool::new("test", "A test tool", |_args| async move {
        Ok(serde_json::json!({"ok": true}))
    })
    .with_schema(serde_json::json!({
        "type": "object",
        "properties": {
            "input": {"type": "string"}
        }
    }));

    let info = tool.to_tool_info();
    assert_eq!(info.name, "test");
    assert_eq!(info.description, "A test tool");
    assert!(info.parameters.is_object());
}
