use crate::{check_ai_configuration_validity, query_daemon_rpc};
use rust_langgraph::llm::openrouter::OpenRouterAdapter;
use rust_langgraph::llm::ChatModel;
use rust_langgraph::llm::ToolInfo;
use rust_langgraph::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AskState {
    pub question: String,
    pub messages: Vec<Message>,
    pub round_count: u32,
    pub interactive: bool,
    pub json: bool,
    pub data_used: Vec<String>,
    pub final_answer: Option<String>,
}

impl State for AskState {
    fn merge(&mut self, other: Self) -> rust_langgraph::errors::Result<()> {
        if !other.question.is_empty() {
            self.question = other.question;
        }
        if !other.messages.is_empty() {
            if other.messages.len() > self.messages.len() {
                self.messages = other.messages;
            } else if other.messages.len() == self.messages.len() && other.messages != self.messages
            {
                self.messages = other.messages;
            }
        }
        self.round_count = other.round_count;
        self.interactive = other.interactive;
        self.json = other.json;
        for entry in other.data_used {
            if !self.data_used.contains(&entry) {
                self.data_used.push(entry);
            }
        }
        if other.final_answer.is_some() {
            self.final_answer = other.final_answer;
        }
        Ok(())
    }
}

pub fn detect_shell() -> String {
    if cfg!(windows) {
        if std::env::var("PSModulePath").is_ok() {
            "PowerShell".to_string()
        } else {
            "CMD".to_string()
        }
    } else {
        "Bash/Sh".to_string()
    }
}

fn parse_time_param(s: &str) -> std::result::Result<i64, String> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(dt.timestamp());
    }

    let s_clean = s.trim().to_lowercase();
    let mut num_str = String::new();
    let mut unit_str = String::new();
    for c in s_clean.chars() {
        if c.is_digit(10) {
            num_str.push(c);
        } else {
            unit_str.push(c);
        }
    }

    if num_str.is_empty() || unit_str.is_empty() {
        return Err(format!("Invalid datetime/duration format: '{}'", s));
    }

    let num: i64 = num_str
        .parse()
        .map_err(|_| format!("Invalid number in '{}'", s))?;
    let now = chrono::Utc::now();
    let duration = match unit_str.as_str() {
        "h" | "hr" | "hour" | "hours" => chrono::Duration::hours(num),
        "d" | "day" | "days" => chrono::Duration::days(num),
        "m" | "month" | "months" => chrono::Duration::days(num * 30),
        "y" | "year" | "years" => chrono::Duration::days(num * 365),
        _ => return Err(format!("Unknown unit '{}' in '{}'", unit_str, s)),
    };

    Ok((now - duration).timestamp())
}

fn resolve_absolute_path(p: &str) -> String {
    let p_buf = std::path::Path::new(p);
    if p_buf.is_absolute() {
        p.to_string()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(p_buf).to_string_lossy().to_string(),
            Err(_) => p.to_string(),
        }
    }
}

pub fn get_tools() -> Vec<ToolInfo> {
    vec![
        ToolInfo::new(
            "sqlite_read_query",
            "Executes a SELECT query on the read-only database to search files, uninstalls, installer footprints, and runtime artifacts.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The exact SQL SELECT query to run."
                    }
                },
                "required": ["query"]
            })
        ),
        ToolInfo::new(
            "fetch_signature",
            "Resolves heuristics and signatures for directories or executable names (e.g. GLCache, Steam, system32).",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "The directory name, executable name, or full file path."
                    }
                },
                "required": ["target"]
            })
        ),
        ToolInfo::new(
            "cli_read_command",
            "Executes passive, read-only system shell commands (e.g. ls, dir, df, du, cat, ps, free, type) to inspect the live OS state.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The command string to execute."
                    }
                },
                "required": ["command"]
            })
        ),
        ToolInfo::new(
            "cli_write_command",
            "Executes mutating OS filesystem commands (e.g. rm, del, remove-item, rmdir, erase). Note: Mutating commands trigger human approval prompts.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The mutating command string to execute."
                    }
                },
                "required": ["command"]
            })
        ),
        ToolInfo::new(
            "snapshot_manage",
            "Manages DiskTracker snapshots (delete or compress action). Note: Mutating actions trigger human approval prompts.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["delete", "compress"],
                        "description": "The snapshot action."
                    },
                    "label": {
                        "type": "string",
                        "description": "The label of the snapshot to target."
                    }
                },
                "required": ["action", "label"]
            })
        ),
        ToolInfo::new(
            "disktracker_status",
            "Queries the status of the DiskTracker daemon (state, volumes monitored, and database paths).",
            serde_json::json!({
                "type": "object",
                "properties": {}
            })
        ),
        ToolInfo::new(
            "disktracker_doctor",
            "Runs diagnostic checks on DiskTracker (checks SQLite database integrity and recent pruning logs).",
            serde_json::json!({
                "type": "object",
                "properties": {}
            })
        ),
        ToolInfo::new(
            "disktracker_search",
            "Searches indexed files/directories in the database using the Tantivy search index.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Matches filenames/substrings. Use '*' for matching everything."
                    },
                    "path": {
                        "type": "string",
                        "description": "Filter by folder path prefix (e.g. 'C:/Users')."
                    },
                    "ext": {
                        "type": "string",
                        "description": "Filter by exact file extension (e.g. 'txt')."
                    },
                    "volume": {
                        "type": "string",
                        "description": "Filter by volume (e.g. 'C:')."
                    },
                    "min_size": {
                        "type": "integer",
                        "description": "Filter by minimum size in bytes."
                    },
                    "max_size": {
                        "type": "integer",
                        "description": "Filter by maximum size in bytes."
                    },
                    "modified_after": {
                        "type": "string",
                        "description": "Filter files modified after a given duration (e.g. '24h', '2d') or UTC datetime (RFC3339)."
                    },
                    "modified_before": {
                        "type": "string",
                        "description": "Filter files modified before a given duration (e.g. '24h', '2d') or UTC datetime (RFC3339)."
                    },
                    "hidden": {
                        "type": "boolean",
                        "description": "Filter hidden files."
                    },
                    "system": {
                        "type": "boolean",
                        "description": "Filter system files."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max number of search results to return (default 100)."
                    }
                },
                "required": ["query"]
            })
        ),
        ToolInfo::new(
            "disktracker_history",
            "Queries the mutation history of a specific file or directory.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to query history for (defaults to current directory if empty)."
                    },
                    "since": {
                        "type": "string",
                        "description": "Filter history since a duration (e.g. '24h', '2d') or UTC datetime (RFC3339)."
                    },
                    "until": {
                        "type": "string",
                        "description": "Filter history until a duration (e.g. '24h', '2d') or UTC datetime (RFC3339)."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["Created", "Modified", "Deleted", "Renamed"],
                        "description": "Filter by mutation kind."
                    },
                    "collapse": {
                        "type": "boolean",
                        "description": "Collapse consecutive same-kind entries."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max number of history entries (default 100)."
                    }
                }
            })
        ),
        ToolInfo::new(
            "disktracker_top",
            "Ranks files/folders by size, growth, or churn.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "volume": {
                        "type": "string",
                        "description": "Restrict search to specific volume (e.g. 'C:')."
                    },
                    "path": {
                        "type": "string",
                        "description": "Restrict to specific folder path (e.g. 'C:/Windows')."
                    },
                    "folders": {
                        "type": "boolean",
                        "description": "Folder-only rollup (conflicts with files)."
                    },
                    "files": {
                        "type": "boolean",
                        "description": "File-only rollup, no folder rollup (conflicts with folders)."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max number of top results (default 20)."
                    },
                    "since": {
                        "type": "string",
                        "description": "Filter by duration (e.g. '7d', '24h') since mutations occurred."
                    },
                    "between_a": {
                        "type": "string",
                        "description": "Compare mutations starting from this snapshot (label or ID)."
                    },
                    "between_b": {
                        "type": "string",
                        "description": "Compare mutations ending at this snapshot (label or ID, required if between_a is set)."
                    },
                    "growth": {
                        "type": "boolean",
                        "description": "Rank by size delta (growth)."
                    },
                    "churn": {
                        "type": "boolean",
                        "description": "Rank by modification count (churn)."
                    }
                }
            })
        ),
        ToolInfo::new(
            "disktracker_snapshot_list",
            "Lists all snapshots in the database.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "volume": {
                        "type": "string",
                        "description": "Filter snapshots by volume."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max number of snapshots (default 20)."
                    }
                }
            })
        ),
        ToolInfo::new(
            "disktracker_snapshot_diff",
            "Diffs two snapshots to see net file mutations.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "snapshot_a": {
                        "type": "string",
                        "description": "First snapshot label or ID."
                    },
                    "snapshot_b": {
                        "type": "string",
                        "description": "Second snapshot label or ID."
                    },
                    "path": {
                        "type": "string",
                        "description": "Filter diff results by path prefix."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max number of diff results (default 100)."
                    }
                },
                "required": ["snapshot_a", "snapshot_b"]
            })
        ),
        ToolInfo::new(
            "disktracker_snapshot_create",
            "Creates a new snapshot. Note: Mutating action triggers HIL human approval.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "volumes": {
                        "type": "array",
                        "items": {
                            "type": "string"
                        },
                        "description": "Volumes to snapshot. Defaults to all monitored volumes if empty/omitted."
                    },
                    "label": {
                        "type": "string",
                        "description": "Label for the snapshot (auto-generated if omitted)."
                    }
                }
            })
        ),
        ToolInfo::new(
            "disktracker_snapshot_delete",
            "Deletes a snapshot. Note: Mutating action triggers HIL human approval.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "label": {
                        "type": "string",
                        "description": "The label of the snapshot to delete."
                    }
                },
                "required": ["label"]
            })
        ),
        ToolInfo::new(
            "disktracker_websearch",
            "Searches the web for information about an application's publisher, creator, default installation directories, registry keys, and runtime folders.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query (e.g. 'valorant publisher', 'valorant installation folder layout')."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max number of search results to return (default 5)."
                    }
                },
                "required": ["query"]
            })
        ),
        ToolInfo::new(
            "disktracker_human_feedback",
            "Asks the user a clarifying question or requests additional input during interactive mode, returning the user's feedback to the agent.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "The question or message to present to the user."
                    }
                },
                "required": ["prompt"]
            })
        )
    ]
}

pub fn build_agent_graph(
) -> std::result::Result<CompiledGraph<AskState>, rust_langgraph::errors::Error> {
    let mut graph = StateGraph::new();

    // 1. Planner Node
    graph.add_node("planner", |mut state: AskState, _config: &Config| async move {
        if state.round_count >= 12 {
            return Ok(state);
        }

        let (base_url, model_name, api_key) = match check_ai_configuration_validity() {
            Ok(vals) => vals,
            Err(e) => return Err(rust_langgraph::errors::Error::execution(e)),
        };

        let shell = detect_shell();
        let os = std::env::consts::OS;

        let system_prompt = format!(
            "You are AI AGENT, a natural-language orchestration agent for DiskTracker.\n\
             You analyze application file installations and runtime footprints to help users clean up their disks safely.\n\
             \n\
             DATABASE SCHEMA:\n\
             - app_install_footprints (app_name TEXT, file_path TEXT, install_time TEXT) -- files written by installers\n\
             - app_runtime_artifacts (process_name TEXT, target_path TEXT, access_time TEXT) -- files/folders generated by running apps\n\
             - facts (volume TEXT, path TEXT, size INTEGER, modified_at INTEGER, is_directory BOOLEAN) -- file system metadata captured at the last snapshot\n\
             - volume_snapshots / parent_snapshots -- historical snapshots. Each volume snapshot records a `sequence_number`, which is the checkpoint sequence number of the mutation log at the time of the snapshot.\n\
             \n\
             DYNAMIC CONTEXT:\n\
             - Shell Mode: {}\n\
             - Operating System: {}\n\
             \n\
             INSTRUCTIONS:\n\
             1. Query high-level DiskTracker tools (e.g. disktracker_search, disktracker_top, disktracker_history, disktracker_status, disktracker_doctor, disktracker_snapshot_list, disktracker_snapshot_diff) first as they are highly optimized and correct. Check existing database data using `disktracker_search` first to see if details are already indexed before deciding to take a new snapshot.\n\
             2. Be extremely dynamic and persistent in your search strategy: If searching for a specific application name directly yields no results or incomplete data, do not stop. You must dynamically expand your search to identify and query associated publisher names, parent/creator directories, or related executable names (for example, files might be stored under parent publisher folders or run under different process names). Use `fetch_signature` to resolve these name associations, and dynamically fallback to query the raw database tables (`app_install_footprints`, `app_runtime_artifacts`) via `sqlite_read_query` if the high-level search index returns nothing.\n\
             3. Use the `disktracker_websearch` tool to search the internet for external knowledge about applications, such as identifying who the publisher or creator of a game/app is, what their default installation folders are, or what process names they run under. This allows you to find associated directories and footprints to check in the database.\n\
             4. If you decide to execute a shell command (via cli_read_command or cli_write_command), you MUST explicitly state in your text response which shell you are executing the command in (e.g. 'Executing command in PowerShell:' or 'Executing command in Bash:') *before* making the tool call. Do not ask the user which shell they are using; you must use the shell specified in the Shell Mode dynamically provided to you.\n\
             5. Under Action Mode (interactive), you can suggest file deletions (cli_write_command) or snapshot operations (disktracker_snapshot_create, disktracker_snapshot_delete).\n\
             6. You do NOT have permission to execute mutating actions directly (such as creating/deleting snapshots or deleting files using `disktracker_snapshot_create`, `disktracker_snapshot_delete`, or `cli_write_command`). If a mutating action is required, you must never invoke these mutating tools. Instead, output a clear natural language message instructing the human to perform the work (e.g. telling them to run the command or delete the files manually).\n\
             7. Perform multi-step reasoning and tool calls if necessary. Do not hesitate to call multiple tools in sequence, try different tools or directions if one fails, reloop or go back-and-forth in any direction as needed (up to 10 rounds) to gather all necessary facts before formulating your final answer.\n\
             8. In interactive mode, if you are unsure of the user's intent, need clarification, or want to ask a question before proceeding, do NOT output the question in text. Instead, immediately invoke the `disktracker_human_feedback` tool with your question. It will display the question to the user and return their input directly to you so you can continue reasoning.\n\
             9. Terminate and produce your final natural language answer as soon as you have the answer. DO NOT query in infinite loops.",
            shell,
            os
        );

        let mut request_messages = vec![Message::system(system_prompt)];
        request_messages.extend(state.messages.clone());

        let adapter = OpenRouterAdapter::with_api_base(&model_name, &api_key, &base_url)
            .bind_tools(get_tools());

        let mut attempts = 0;
        let max_attempts = 3;
        let mut delay = std::time::Duration::from_millis(500);
        let stream_res = loop {
            match adapter.stream(&request_messages).await {
                Ok(s) => break Ok(s),
                Err(e) => {
                    attempts += 1;
                    if attempts >= max_attempts {
                        break Err(e);
                    }
                    if !state.json {
                        println!("\n\x1b[33m⚠️  [Warning] OpenRouter request failed ({:?}). Retrying in {:?}... ({}/{})\x1b[0m", e, delay, attempts, max_attempts);
                    }
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
            }
        };

        match stream_res {
            Ok(mut stream) => {
                use futures::StreamExt;
                let mut full_content = String::new();
                let mut final_tool_calls = Vec::new();
                while let Some(chunk_res) = stream.next().await {
                    match chunk_res {
                        Ok(chunk) => {
                            if !chunk.content.is_empty() {
                                full_content.push_str(&chunk.content);
                            }
                            if let Some(mut tc) = chunk.tool_calls {
                                final_tool_calls.append(&mut tc);
                            }
                        }
                        Err(e) => return Err(rust_langgraph::errors::Error::execution(format!("Stream error: {}", e))),
                    }
                }

                let mut reply = Message::assistant(full_content);
                if !final_tool_calls.is_empty() {
                    reply = reply.with_tool_calls(final_tool_calls);
                }
                state.messages.push(reply.clone());

                let has_no_tool_calls = reply.tool_calls.as_ref().map(|tc| tc.is_empty()).unwrap_or(true);
                if has_no_tool_calls {
                    state.final_answer = Some(reply.content);
                }
                Ok(state)
            }
            Err(e) => Err(rust_langgraph::errors::Error::execution(format!("LLM invocation failed: {}", e))),
        }
    });

    // 2. Read-only Tool Execution Node
    graph.add_node("tool_exec", |mut state: AskState, _config: &Config| async move {
        state.round_count += 1;

        let last_msg = match state.messages.last().cloned() {
            Some(m) => m,
            None => return Ok(state),
        };

        if let Some(tool_calls) = last_msg.tool_calls {
            for tc in tool_calls {
                let tool_name = tc.name.as_str();
                let tool_call_id = tc.id.clone();

                match tool_name {
                    "sqlite_read_query" => {
                        let query = tc.arguments["query"].as_str().unwrap_or("");
                        state.data_used.push(format!("sqlite_read_query(query={:?})", query));
                        match query_daemon_rpc("sqlite_read_query", serde_json::json!({ "query": query })).await {
                            Ok(res) => {
                                state.messages.push(Message::tool(res.to_string(), tool_call_id));
                            }
                            Err(e) => {
                                state.messages.push(Message::tool(format!("Error: {}", e), tool_call_id));
                            }
                        }
                    }
                    "fetch_signature" => {
                        let target = tc.arguments["target"].as_str().unwrap_or("");
                        state.data_used.push(format!("fetch_signature(target={:?})", target));
                        match query_daemon_rpc("fetch_signature", serde_json::json!({ "target": target })).await {
                            Ok(res) => {
                                let sig = res.get("signature").and_then(|s| s.as_str()).unwrap_or("");
                                state.messages.push(Message::tool(sig.to_string(), tool_call_id));
                            }
                            Err(e) => {
                                state.messages.push(Message::tool(format!("Error: {}", e), tool_call_id));
                            }
                        }
                    }
                    "cli_read_command" => {
                        let command = tc.arguments["command"].as_str().unwrap_or("");
                        state.data_used.push(format!("cli_read_command(command={:?})", command));
                        match crate::execute_command_interactively("read", command).await {
                            Ok(out) => {
                                state.messages.push(Message::tool(out, tool_call_id));
                            }
                            Err(e) => {
                                state.messages.push(Message::tool(format!("Error: {}", e), tool_call_id));
                            }
                        }
                    }
                    "disktracker_status" => {
                        state.data_used.push("disktracker_status".to_string());
                        match query_daemon_rpc("status", serde_json::Value::Null).await {
                            Ok(res) => {
                                state.messages.push(Message::tool(res.to_string(), tool_call_id));
                            }
                            Err(e) => {
                                state.messages.push(Message::tool(format!("Error: {}", e), tool_call_id));
                            }
                        }
                    }
                    "disktracker_doctor" => {
                        state.data_used.push("disktracker_doctor".to_string());
                        let integrity = query_daemon_rpc("check_db_integrity", serde_json::Value::Null).await;
                        let pruning = query_daemon_rpc("get_pruning_logs", serde_json::Value::Null).await;
                        let combined = serde_json::json!({
                            "db_integrity": integrity.unwrap_or(serde_json::Value::Null),
                            "pruning_logs": pruning.unwrap_or(serde_json::Value::Null),
                        });
                        state.messages.push(Message::tool(combined.to_string(), tool_call_id));
                    }
                    "disktracker_search" => {
                        let query = tc.arguments["query"].as_str().unwrap_or("*");
                        let path = tc.arguments.get("path").and_then(|p| p.as_str());
                        let ext = tc.arguments.get("ext").and_then(|e| e.as_str());
                        let volume = tc.arguments.get("volume").and_then(|v| v.as_str());
                        let min_size = tc.arguments.get("min_size").and_then(|m| m.as_u64());
                        let max_size = tc.arguments.get("max_size").and_then(|m| m.as_u64());
                        let hidden = tc.arguments.get("hidden").and_then(|h| h.as_bool());
                        let system = tc.arguments.get("system").and_then(|s| s.as_bool());
                        let limit = tc.arguments.get("limit").and_then(|l| l.as_u64());
                        let cursor = tc.arguments.get("cursor").and_then(|c| c.as_str());

                        let modified_after_ts = tc.arguments.get("modified_after")
                            .and_then(|m| m.as_str())
                            .and_then(|s| parse_time_param(s).ok());

                        let modified_before_ts = tc.arguments.get("modified_before")
                            .and_then(|m| m.as_str())
                            .and_then(|s| parse_time_param(s).ok());

                        let mut search_volume = volume.map(|v| v.to_uppercase());
                        let mut search_path = path.map(|p| p.to_string());
                        if let Some(ref p) = search_path {
                            let normalized = p.replace('\\', "/");
                            if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
                                search_volume = Some(normalized[0..2].to_uppercase());
                                let remaining = normalized[2..].trim_start_matches('/').to_string();
                                search_path = if remaining.is_empty() { None } else { Some(remaining) };
                            }
                        }

                        let search_params = serde_json::json!({
                            "query": query,
                            "path": search_path,
                            "ext": ext,
                            "volume": search_volume,
                            "min_size": min_size,
                            "max_size": max_size,
                            "modified_after": modified_after_ts,
                            "modified_before": modified_before_ts,
                            "hidden": hidden,
                            "system": system,
                            "limit": limit,
                            "cursor": cursor,
                        });

                        state.data_used.push(format!("disktracker_search(query={:?})", query));

                        match query_daemon_rpc("search_query", search_params).await {
                            Ok(res) => {
                                state.messages.push(Message::tool(res.to_string(), tool_call_id));
                            }
                            Err(e) => {
                                state.messages.push(Message::tool(format!("Error: {}", e), tool_call_id));
                            }
                        }
                    }
                    "disktracker_history" => {
                        let path = tc.arguments.get("path").and_then(|p| p.as_str()).unwrap_or("");
                        let since = tc.arguments.get("since").and_then(|s| s.as_str());
                        let until = tc.arguments.get("until").and_then(|u| u.as_str());
                        let kind = tc.arguments.get("kind").and_then(|k| k.as_str());
                        let collapse = tc.arguments.get("collapse").and_then(|c| c.as_bool()).unwrap_or(true);
                        let limit = tc.arguments.get("limit").and_then(|l| l.as_u64());
                        let cursor = tc.arguments.get("cursor").and_then(|c| c.as_str());

                        let since_ts = since.and_then(|s| parse_time_param(s).ok());
                        let until_ts = until.and_then(|u| parse_time_param(u).ok());

                        let resolved_path = resolve_absolute_path(path);

                        let history_params = serde_json::json!({
                            "path": resolved_path,
                            "since": since_ts,
                            "until": until_ts,
                            "kind": kind,
                            "collapse": collapse,
                            "limit": limit,
                            "cursor": cursor,
                        });

                        state.data_used.push(format!("disktracker_history(path={:?})", path));

                        match query_daemon_rpc("get_history", history_params).await {
                            Ok(res) => {
                                state.messages.push(Message::tool(res.to_string(), tool_call_id));
                            }
                            Err(e) => {
                                state.messages.push(Message::tool(format!("Error: {}", e), tool_call_id));
                            }
                        }
                    }
                    "disktracker_top" => {
                        let volume = tc.arguments.get("volume").and_then(|v| v.as_str());
                        let path = tc.arguments.get("path").and_then(|p| p.as_str());
                        let folders = tc.arguments.get("folders").and_then(|f| f.as_bool()).unwrap_or(false);
                        let files = tc.arguments.get("files").and_then(|f| f.as_bool()).unwrap_or(false);
                        let limit = tc.arguments.get("limit").and_then(|l| l.as_u64());
                        let since = tc.arguments.get("since").and_then(|s| s.as_str());
                        let between_a = tc.arguments.get("between_a").and_then(|b| b.as_str());
                        let between_b = tc.arguments.get("between_b").and_then(|b| b.as_str());
                        let growth = tc.arguments.get("growth").and_then(|g| g.as_bool()).unwrap_or(false);
                        let churn = tc.arguments.get("churn").and_then(|c| c.as_bool()).unwrap_or(false);
                        let cursor = tc.arguments.get("cursor").and_then(|c| c.as_str());

                        let since_ts = since.and_then(|s| parse_time_param(s).ok());
                        let resolved_path = path.map(|p| resolve_absolute_path(p));

                        let mut resolved_vol = volume.map(|v| {
                            if v.len() == 1 {
                                format!("{}:", v.to_ascii_uppercase())
                            } else {
                                v.to_uppercase()
                            }
                        });

                        if let Some(ref p) = resolved_path {
                            let normalized = p.replace('\\', "/");
                            if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
                                let vol_from_path = normalized[0..2].to_uppercase();
                                if resolved_vol.is_none() {
                                    resolved_vol = Some(vol_from_path);
                                }
                            }
                        }

                        let top_params = serde_json::json!({
                            "path": resolved_path,
                            "volume": resolved_vol,
                            "folders": folders,
                            "files": files,
                            "limit": limit,
                            "since": since_ts,
                            "between_a": between_a,
                            "between_b": between_b,
                            "growth": growth,
                            "churn": churn,
                            "cursor": cursor,
                        });

                        state.data_used.push(format!("disktracker_top(volume={:?}, path={:?})", volume, path));

                        match query_daemon_rpc("get_top", top_params).await {
                            Ok(res) => {
                                state.messages.push(Message::tool(res.to_string(), tool_call_id));
                            }
                            Err(e) => {
                                state.messages.push(Message::tool(format!("Error: {}", e), tool_call_id));
                            }
                        }
                    }
                    "disktracker_snapshot_list" => {
                        let volume = tc.arguments.get("volume").and_then(|v| v.as_str());
                        let limit = tc.arguments.get("limit").and_then(|l| l.as_u64());
                        let cursor = tc.arguments.get("cursor").and_then(|c| c.as_str());

                        let list_params = serde_json::json!({
                            "volume": volume,
                            "limit": limit,
                            "cursor": cursor,
                        });

                        state.data_used.push(format!("disktracker_snapshot_list(volume={:?})", volume));

                        match query_daemon_rpc("snapshot_list", list_params).await {
                            Ok(res) => {
                                state.messages.push(Message::tool(res.to_string(), tool_call_id));
                            }
                            Err(e) => {
                                state.messages.push(Message::tool(format!("Error: {}", e), tool_call_id));
                            }
                        }
                    }
                    "disktracker_snapshot_diff" => {
                        let snapshot_a = tc.arguments["snapshot_a"].as_str().unwrap_or("");
                        let snapshot_b = tc.arguments["snapshot_b"].as_str().unwrap_or("");
                        let path = tc.arguments.get("path").and_then(|p| p.as_str());
                        let limit = tc.arguments.get("limit").and_then(|l| l.as_u64());

                        let resolved_path = path.map(|p| resolve_absolute_path(p));

                        let diff_params = serde_json::json!({
                            "snapshot_a": snapshot_a,
                            "snapshot_b": snapshot_b,
                            "path": resolved_path,
                            "limit": limit,
                        });

                        state.data_used.push(format!("disktracker_snapshot_diff(a={:?}, b={:?})", snapshot_a, snapshot_b));

                        match query_daemon_rpc("snapshot_diff", diff_params).await {
                            Ok(res) => {
                                state.messages.push(Message::tool(res.to_string(), tool_call_id));
                            }
                            Err(e) => {
                                state.messages.push(Message::tool(format!("Error: {}", e), tool_call_id));
                            }
                        }
                    }
                    "disktracker_websearch" => {
                        let query = tc.arguments["query"].as_str().unwrap_or("");
                        let limit = tc.arguments.get("limit").and_then(|l| l.as_u64()).unwrap_or(5) as usize;

                        state.data_used.push(format!("disktracker_websearch(query={:?})", query));

                        let cfg = config_mgr::load_config();
                        let provider = cfg.ai_websearch_provider.as_deref().unwrap_or("duckduckgo").to_lowercase();
                        let api_key = crate::get_websearch_api_key().unwrap_or_default();

                        let mut results = Vec::new();
                        let client = reqwest::Client::new();

                        if provider == "tavily" && !api_key.is_empty() {
                            let url = "https://api.tavily.com/search";
                            let payload = serde_json::json!({
                                "api_key": api_key,
                                "query": query,
                                "max_results": limit,
                            });
                            if let Ok(resp) = client.post(url).json(&payload).send().await {
                                if let Ok(json) = resp.json::<serde_json::Value>().await {
                                    if let Some(arr) = json.get("results").and_then(|r| r.as_array()) {
                                        for (idx, item) in arr.iter().enumerate() {
                                            let title = item.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
                                            let url = item.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
                                            let snippet = item.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
                                            results.push(websearch::types::SearchResult {
                                                title,
                                                snippet,
                                                url,
                                                ref_index: idx + 1,
                                            });
                                        }
                                    }
                                }
                            }
                        } else if provider == "google" && !api_key.is_empty() && cfg.ai_websearch_cx.is_some() {
                            if let Some(ref cx) = cfg.ai_websearch_cx {
                                let url = format!(
                                    "https://www.googleapis.com/customsearch/v1?q={}&key={}&cx={}&num={}",
                                    urlencoding::encode(query),
                                    urlencoding::encode(&api_key),
                                    urlencoding::encode(cx),
                                    limit
                                );
                                if let Ok(resp) = client.get(&url).send().await {
                                    if let Ok(json) = resp.json::<serde_json::Value>().await {
                                        if let Some(arr) = json.get("items").and_then(|i| i.as_array()) {
                                            for (idx, item) in arr.iter().enumerate() {
                                                let title = item.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
                                                let url = item.get("link").and_then(|l| l.as_str()).unwrap_or("").to_string();
                                                let snippet = item.get("snippet").and_then(|s| s.as_str()).unwrap_or("").to_string();
                                                results.push(websearch::types::SearchResult {
                                                    title,
                                                    snippet,
                                                    url,
                                                    ref_index: idx + 1,
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                        } else if provider == "brave" && !api_key.is_empty() {
                            let url = format!(
                                "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
                                urlencoding::encode(query),
                                limit
                            );
                            if let Ok(resp) = client.get(&url)
                                .header("Accept", "application/json")
                                .header("X-Subscription-Token", &api_key)
                                .send()
                                .await
                            {
                                if let Ok(json) = resp.json::<serde_json::Value>().await {
                                    if let Some(arr) = json.get("web").and_then(|w| w.get("results")).and_then(|r| r.as_array()) {
                                        for (idx, item) in arr.iter().enumerate() {
                                            let title = item.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
                                            let url = item.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
                                            let snippet = item.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string();
                                            results.push(websearch::types::SearchResult {
                                                title,
                                                snippet,
                                                url,
                                                ref_index: idx + 1,
                                            });
                                        }
                                    }
                                }
                            }
                        }

                        if results.is_empty() {
                            let opts = websearch::types::SearchOptions {
                                query: query.to_string(),
                                max_results: Some(limit),
                                ..Default::default()
                            };
                            match websearch::run_search(opts).await {
                                Ok(out) => {
                                    state.messages.push(Message::tool(serde_json::to_string(&out).unwrap_or_default(), tool_call_id));
                                }
                                Err(e) => {
                                    state.messages.push(Message::tool(format!("Error: {}", e), tool_call_id));
                                }
                            }
                        } else {
                            let references = websearch::build_refs(&results);
                            let body = websearch::format_results(&results);
                            let refs_block = websearch::render_references(&references);
                            let full = if refs_block.is_empty() {
                                body
                            } else {
                                format!("{}\n\n{}", body, refs_block)
                            };

                            let output = websearch::types::SearchOutput {
                                query: query.to_string(),
                                token_estimate: websearch::compress::estimate_tokens(&full),
                                result_count: results.len(),
                                references,
                                results,
                            };
                            state.messages.push(Message::tool(serde_json::to_string(&output).unwrap_or_default(), tool_call_id));
                        }
                    }
                    "disktracker_human_feedback" => {
                        let prompt = tc.arguments["prompt"].as_str().unwrap_or("");
                        state.data_used.push(format!("disktracker_human_feedback(prompt={:?})", prompt));

                        if !state.interactive {
                            state.messages.push(Message::tool(
                                "Error: Cannot request human feedback in exploratory (read-only) mode. Please run with the --interactive flag to enable human interaction.".to_string(),
                                tool_call_id
                            ));
                        } else {
                            println!("\n\x1b[32m💬 [Agent Question] {}\x1b[0m", prompt);
                            print!("Your Response: ");
                            use std::io::Write;
                            let _ = std::io::stdout().flush();

                            let mut input = String::new();
                            let _ = std::io::stdin().read_line(&mut input);
                            let user_feedback = input.trim().to_string();

                            state.messages.push(Message::tool(user_feedback, tool_call_id));
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(state)
    });

    // 3. Human Interrupt Node
    graph.add_node("human_interrupt", |mut state: AskState, _config: &Config| async move {
        state.round_count += 1;

        let last_msg = match state.messages.last().cloned() {
            Some(m) => m,
            None => return Ok(state),
        };

        if let Some(tool_calls) = last_msg.tool_calls {
            for tc in tool_calls {
                let tool_name = tc.name.as_str();
                let tool_call_id = tc.id.clone();

                if tool_name == "cli_write_command" {
                    let command = tc.arguments["command"].as_str().unwrap_or("");
                    state.data_used.push(format!("cli_write_command(command={:?})", command));

                    println!("\n\x1b[33m⚠️  [Security] Agent requested execution of a mutating command:\x1b[0m");
                    println!("Command: \x1b[31m{}\x1b[0m", command);
                    print!("Do you authorize this action? [y/N]: ");
                    use std::io::Write;
                    let _ = std::io::stdout().flush();

                    let mut input = String::new();
                    let _ = std::io::stdin().read_line(&mut input);
                    let answer = input.trim().to_lowercase();

                    if answer == "y" || answer == "yes" {
                        match query_daemon_rpc("cli_write_command", serde_json::json!({ "command": command })).await {
                            Ok(res) => {
                                let output = res.get("output").and_then(|o| o.as_str()).unwrap_or("");
                                state.messages.push(Message::tool(output.to_string(), tool_call_id));
                            }
                            Err(e) => {
                                state.messages.push(Message::tool(format!("Error: {}", e), tool_call_id));
                            }
                        }
                    } else {
                        state.messages.push(Message::tool("Action Aborted by User".to_string(), tool_call_id));
                    }
                } else if tool_name == "snapshot_manage" {
                    let action = tc.arguments["action"].as_str().unwrap_or("");
                    let label = tc.arguments["label"].as_str().unwrap_or("");
                    state.data_used.push(format!("snapshot_manage(action={:?}, label={:?})", action, label));

                    println!("\n\x1b[33m⚠️  [Security] Agent requested snapshot management mutation:\x1b[0m");
                    println!("Action: \x1b[31m{}\x1b[0m on snapshot: \x1b[31m{}\x1b[0m", action, label);
                    print!("Do you authorize this action? [y/N]: ");
                    use std::io::Write;
                    let _ = std::io::stdout().flush();

                    let mut input = String::new();
                    let _ = std::io::stdin().read_line(&mut input);
                    let answer = input.trim().to_lowercase();

                    if answer == "y" || answer == "yes" {
                        match query_daemon_rpc("snapshot_manage", serde_json::json!({ "action": action, "label": label })).await {
                            Ok(res) => {
                                let status = res.get("status").and_then(|s| s.as_str()).unwrap_or("ok");
                                state.messages.push(Message::tool(status.to_string(), tool_call_id));
                            }
                            Err(e) => {
                                state.messages.push(Message::tool(format!("Error: {}", e), tool_call_id));
                            }
                        }
                    } else {
                        state.messages.push(Message::tool("Action Aborted by User".to_string(), tool_call_id));
                    }
                } else if tool_name == "disktracker_snapshot_create" {
                    let volumes = tc.arguments.get("volumes").and_then(|v| v.as_array());
                    let label = tc.arguments.get("label").and_then(|l| l.as_str());
                    let vols_display = if let Some(ref v) = volumes {
                        format!("{:?}", v)
                    } else {
                        "all registered volumes".to_string()
                    };

                    state.data_used.push(format!("disktracker_snapshot_create(volumes={:?}, label={:?})", volumes, label));

                    println!("\n\x1b[33m⚠️  [Security] Agent requested snapshot creation:\x1b[0m");
                    println!("Volumes: \x1b[31m{}\x1b[0m, Label: \x1b[31m{:?}\x1b[0m", vols_display, label);
                    print!("Do you authorize this action? [y/N]: ");
                    use std::io::Write;
                    let _ = std::io::stdout().flush();

                    let mut input = String::new();
                    let _ = std::io::stdin().read_line(&mut input);
                    let answer = input.trim().to_lowercase();

                    if answer == "y" || answer == "yes" {
                        let create_params = serde_json::json!({
                            "volumes": volumes,
                            "label": label,
                        });
                        match query_daemon_rpc("snapshot_create", create_params).await {
                            Ok(res) => {
                                state.messages.push(Message::tool(res.to_string(), tool_call_id));
                            }
                            Err(e) => {
                                state.messages.push(Message::tool(format!("Error: {}", e), tool_call_id));
                            }
                        }
                    } else {
                        state.messages.push(Message::tool("Action Aborted by User".to_string(), tool_call_id));
                    }
                } else if tool_name == "disktracker_snapshot_delete" {
                    let label = tc.arguments["label"].as_str().unwrap_or("");
                    state.data_used.push(format!("disktracker_snapshot_delete(label={:?})", label));

                    println!("\n\x1b[33m⚠️  [Security] Agent requested snapshot deletion:\x1b[0m");
                    println!("Label: \x1b[31m{}\x1b[0m", label);
                    print!("Do you authorize this action? [y/N]: ");
                    use std::io::Write;
                    let _ = std::io::stdout().flush();

                    let mut input = String::new();
                    let _ = std::io::stdin().read_line(&mut input);
                    let answer = input.trim().to_lowercase();

                    if answer == "y" || answer == "yes" {
                        match query_daemon_rpc("snapshot_manage", serde_json::json!({ "action": "delete", "label": label })).await {
                            Ok(res) => {
                                let status = res.get("status").and_then(|s| s.as_str()).unwrap_or("ok");
                                state.messages.push(Message::tool(status.to_string(), tool_call_id));
                            }
                            Err(e) => {
                                state.messages.push(Message::tool(format!("Error: {}", e), tool_call_id));
                            }
                        }
                    } else {
                        state.messages.push(Message::tool("Action Aborted by User".to_string(), tool_call_id));
                    }
                }
            }
        }

        Ok(state)
    });

    // 4. Non-interactive Rejection Node
    graph.add_node("tool_reject_non_interactive", |mut state: AskState, _config: &Config| async move {
        state.round_count += 1;
        let last_msg = match state.messages.last().cloned() {
            Some(m) => m,
            None => return Ok(state),
        };

        if let Some(tool_calls) = last_msg.tool_calls {
            for tc in tool_calls {
                let tool_call_id = tc.id.clone();
                state.messages.push(Message::tool(
                    "Error: Cannot execute mutating commands in Exploratory (read-only) mode. User must supply --interactive flag to authorize mutating actions.".to_string(),
                    tool_call_id
                ));
            }
        }

        Ok(state)
    });

    graph.set_entry_point("planner");
    graph.set_finish_point("planner");

    // Connect nodes
    graph.add_edge("tool_exec", "planner");
    graph.add_edge("human_interrupt", "planner");
    graph.add_edge("tool_reject_non_interactive", "planner");

    // Add conditional edges from planner
    graph.add_conditional_edges("planner", |state: &AskState| {
        let last_msg = state.messages.last().cloned();
        let round_count = state.round_count;
        let interactive = state.interactive;

        async move {
            if round_count >= 12 {
                return Ok(rust_langgraph::pregel::BranchResult::end());
            }

            if let Some(msg) = last_msg {
                if let Some(ref calls) = msg.tool_calls {
                    if !calls.is_empty() {
                        let mut has_mutating = false;
                        for tc in calls {
                            if tc.name == "cli_write_command"
                                || tc.name == "snapshot_manage"
                                || tc.name == "disktracker_snapshot_create"
                                || tc.name == "disktracker_snapshot_delete"
                            {
                                has_mutating = true;
                            }
                        }
                        if has_mutating {
                            if interactive {
                                return Ok(rust_langgraph::pregel::BranchResult::single(
                                    "human_interrupt",
                                ));
                            } else {
                                return Ok(rust_langgraph::pregel::BranchResult::single(
                                    "tool_reject_non_interactive",
                                ));
                            }
                        } else {
                            return Ok(rust_langgraph::pregel::BranchResult::single("tool_exec"));
                        }
                    }
                }
            }

            Ok(rust_langgraph::pregel::BranchResult::end())
        }
    });

    graph.compile(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_shell() {
        let shell = detect_shell();
        assert!(!shell.is_empty());
    }

    #[test]
    fn test_tools_definition() {
        let tools = get_tools();
        assert_eq!(tools.len(), 16);
        assert_eq!(tools[0].name, "sqlite_read_query");
        assert_eq!(tools[1].name, "fetch_signature");
        assert_eq!(tools[2].name, "cli_read_command");
        assert_eq!(tools[3].name, "cli_write_command");
        assert_eq!(tools[4].name, "snapshot_manage");
        assert_eq!(tools[5].name, "disktracker_status");
        assert_eq!(tools[6].name, "disktracker_doctor");
        assert_eq!(tools[7].name, "disktracker_search");
        assert_eq!(tools[8].name, "disktracker_history");
        assert_eq!(tools[9].name, "disktracker_top");
        assert_eq!(tools[10].name, "disktracker_snapshot_list");
        assert_eq!(tools[11].name, "disktracker_snapshot_diff");
        assert_eq!(tools[12].name, "disktracker_snapshot_create");
        assert_eq!(tools[13].name, "disktracker_snapshot_delete");
        assert_eq!(tools[14].name, "disktracker_websearch");
        assert_eq!(tools[15].name, "disktracker_human_feedback");
    }

    #[test]
    fn test_graph_compilation() {
        let app = build_agent_graph();
        assert!(app.is_ok());
    }
}
