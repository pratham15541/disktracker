use keyring::Entry;
use std::fs;
use config_mgr::{load_config, save_config};

pub mod graph;
pub mod session_store;

pub fn set_api_key(key: &str) -> Result<(), String> {
    if cfg!(windows) {
        let entry = Entry::new("disktracker-ai", "api-key")
            .map_err(|e| format!("Failed to create keyring entry: {:?}", e))?;
        entry.set_password(key)
            .map_err(|e| format!("Failed to store API key in Windows Credential Manager: {:?}", e))?;
    } else {
        // Fallback for WSL/Linux
        let mut key_path = storage::get_db_dir().map_err(|e| e.to_string())?;
        key_path.push("ai_key");
        fs::write(&key_path, key)
            .map_err(|e| format!("Failed to write AI key fallback: {:?}", e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600));
        }
    }
    Ok(())
}

pub fn get_api_key() -> Result<String, String> {
    if cfg!(windows) {
        let entry = Entry::new("disktracker-ai", "api-key")
            .map_err(|e| format!("Failed to create keyring entry: {:?}", e))?;
        entry.get_password()
            .map_err(|e| format!("Failed to retrieve API key from Windows Credential Manager: {:?}", e))
    } else {
        // Fallback for WSL/Linux
        let mut key_path = storage::get_db_dir().map_err(|e| e.to_string())?;
        key_path.push("ai_key");
        if !key_path.exists() {
            return Err("API key not found. Run 'disktracker ai config --api-key <key>' to set it.".to_string());
        }
        let key = fs::read_to_string(&key_path)
            .map_err(|e| format!("Failed to read AI key fallback: {:?}", e))?;
        Ok(key.trim().to_string())
    }
}

pub fn check_ai_configuration_validity() -> Result<(String, String, String), String> {
    let config = load_config();
    let base_url = match config.ai_base_url {
        Some(ref url) if !url.trim().is_empty() => url.trim().to_string(),
        _ => return Err("AI cannot be used because base-url is not set. Run 'disktracker ai config --base-url <url>' to configure it.".to_string()),
    };
    let model = match config.ai_model {
        Some(ref m) if !m.trim().is_empty() => m.trim().to_string(),
        _ => return Err("AI cannot be used because model is not set. Run 'disktracker ai config --model <model>' to configure it.".to_string()),
    };
    let api_key = match get_api_key() {
        Ok(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => return Err("AI cannot be used because api-key is not set. Run 'disktracker ai config --api-key <key>' to configure it.".to_string()),
    };
    Ok((base_url, model, api_key))
}

pub async fn run_ai_test() -> Result<(), String> {
    let (base_url, model, api_key) = check_ai_configuration_validity()?;

    let client = reqwest::Client::new();
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

    let payload = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "user", "content": "ping"}
        ],
        "max_tokens": 5
    });

    let resp = client.post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {:?}", e))?;

    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("Failed to read response body: {:?}", e))?;

    if status.is_success() {
        Ok(())
    } else {
        Err(format!("Handshake failed (HTTP {}): {}", status, text))
    }
}

pub fn update_ai_config(
    base_url: Option<String>,
    model: Option<String>,
    chat_session_store: Option<bool>,
) -> Result<(), String> {
    let mut config = load_config();
    if let Some(url) = base_url {
        config.ai_base_url = Some(url);
    }
    if let Some(m) = model {
        config.ai_model = Some(m);
    }
    if let Some(store) = chat_session_store {
        config.ai_chat_session_store = store;
    }
    save_config(&config)
}

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub async fn query_daemon_rpc(
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let pipe_path = if cfg!(windows) {
        r"\\.\pipe\disktracker"
    } else {
        "/tmp/disktracker.sock"
    };

    let mut stream = platform_windows::connect_client(pipe_path)
        .await
        .map_err(|e| format!("Failed to connect to daemon: {}", e))?;

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
        "id": 1
    });

    let mut req_bytes = serde_json::to_vec(&request)
        .map_err(|e| format!("Failed to serialize request: {}", e))?;
    req_bytes.push(b'\n');

    stream.write_all(&req_bytes)
        .await
        .map_err(|e| format!("Failed to write to daemon: {}", e))?;
    stream.flush()
        .await
        .map_err(|e| format!("Failed to flush: {}", e))?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)
        .await
        .map_err(|e| format!("Failed to read response line: {}", e))?;

    let response: serde_json::Value = serde_json::from_str(&line)
        .map_err(|e| format!("Failed to parse JSON response: {}", e))?;

    if let Some(err) = response.get("error") {
        let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("Unknown error");
        return Err(msg.to_string());
    }

    if let Some(res) = response.get("result") {
        Ok(res.clone())
    } else {
        Err("Missing result and error in JSON-RPC response".to_string())
    }
}

pub async fn execute_command_interactively(mode: &str, command: &str) -> Result<String, String> {
    loop {
        let method = if mode == "write" {
            "cli_write_command"
        } else {
            "cli_read_command"
        };

        match query_daemon_rpc(method, serde_json::json!({ "command": command })).await {
            Ok(res) => {
                if let Some(output) = res.get("output").and_then(|o| o.as_str()) {
                    return Ok(output.to_string());
                }
                return Ok("".to_string());
            }
            Err(e) => {
                if e.starts_with("E_COMMAND_NOT_WHITELISTED: ") {
                    let first_word = e.trim_start_matches("E_COMMAND_NOT_WHITELISTED: ").trim().to_string();
                    
                    println!("\n[Security] Command '{}' is not in the {} whitelist.", first_word, mode);
                    print!("Should I add '{}' to the {} whitelist and execute it? [y/N]: ", first_word, mode);
                    std::io::Write::flush(&mut std::io::stdout()).unwrap();

                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input).unwrap();
                    let answer = input.trim().to_lowercase();
                    if answer == "y" || answer == "yes" {
                        match query_daemon_rpc("whitelist_add", serde_json::json!({
                            "mode": mode,
                            "command": first_word
                        })).await {
                            Ok(_) => {
                                println!("[Security] Added '{}' to the {} whitelist. Retrying command...", first_word, mode);
                                continue;
                            }
                            Err(we) => {
                                return Err(format!("Failed to add command to whitelist: {}", we));
                            }
                        }
                    } else {
                        return Err(format!("Command '{}' execution denied by user.", first_word));
                    }
                }
                return Err(e);
            }
        }
    }
}

pub async fn run_agent_query(
    question: &str,
    interactive: bool,
    initial_state: Option<graph::AskState>,
) -> Result<graph::AskState, String> {
    let mut graph = graph::build_agent_graph()
        .map_err(|e| format!("Failed to build agent graph: {}", e))?;

    let state = match initial_state {
        Some(mut s) => {
            s.question = question.to_string();
            s.messages.push(rust_langgraph::state::Message::user(question));
            s.interactive = interactive;
            s.final_answer = None;
            s.round_count = 0;
            s
        }
        None => graph::AskState {
            question: question.to_string(),
            messages: vec![rust_langgraph::state::Message::user(question)],
            round_count: 0,
            interactive,
            data_used: Vec::new(),
            final_answer: None,
        },
    };

    let result = graph.invoke(state, rust_langgraph::config::Config::default())
        .await
        .map_err(|e| format!("Agent execution failed: {}", e))?;

    Ok(result)
}


