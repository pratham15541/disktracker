mod drain;
pub mod search;
pub mod history;
pub mod snapshots;
use tantivy::schema::Value;

use core_types::{
    DaemonState, DrainProgress, ProgressSnapshot, ScannerProgress, VolumeProgress, WatcherProgress,
};
use platform_traits::IpcListener;
use platform_windows::create_listener;
use serde::{Deserialize, Serialize};
use std::io;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Serialize, Deserialize, Debug)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
    pub id: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    pub id: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcResponse {
    pub fn success(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: Some(result),
            error: None,
            id,
        }
    }

    pub fn error(id: Option<serde_json::Value>, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(JsonRpcError {
                code,
                message,
                data: None,
            }),
            id,
        }
    }

    pub fn error_with_data(
        id: Option<serde_json::Value>,
        code: i32,
        message: String,
        data: Option<serde_json::Value>,
    ) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(JsonRpcError {
                code,
                message,
                data,
            }),
            id,
        }
    }
}

async fn run_pruning_cycle() {
    let config = config_mgr::load_config();
    let duration = match config_mgr::parse_duration(&config.retention) {
        Ok(dur) => dur,
        Err(e) => {
            eprintln!(
                "[Pruning] Failed to parse retention duration: {}. Defaulting to 30 days.",
                e
            );
            chrono::Duration::days(30)
        }
    };
    let cutoff = chrono::Utc::now() - duration;
    let cutoff_str = cutoff.to_rfc3339();

    let volumes = core_types::get_registered_volumes();
    for vol in volumes {
        let tracker = core_types::get_volume_tracker(&vol);
        let state = { *tracker.state.lock().unwrap() };
        let replaying = tracker.replaying.load(std::sync::atomic::Ordering::Relaxed);

        if state == DaemonState::Starting
            || state == DaemonState::BaselineScanning
            || state == DaemonState::Reconciling
            || replaying
        {
            println!(
                "[Pruning] Skip pruning for volume {} because it is mid-replay/scan",
                vol
            );
            let _ = storage::log_pruning_run(
                &vol,
                "SKIPPED",
                "Volume is mid-replay or baseline scanning",
            );
            continue;
        }

        println!(
            "[Pruning] Pruning volume {} with cutoff {}",
            vol, cutoff_str
        );
        match storage::get_db_connection() {
            Ok(conn) => {
                match conn.execute(
                    "DELETE FROM mutation_log WHERE volume = ?1 AND at < ?2",
                    [&vol, &cutoff_str],
                ) {
                    Ok(deleted_count) => {
                        let msg =
                            format!("Deleted {} rows older than {}", deleted_count, cutoff_str);
                        println!("[Pruning] Pruning successful for volume {}: {}", vol, msg);
                        let _ = storage::log_pruning_run(&vol, "SUCCESS", &msg);
                    }
                    Err(e) => {
                        let msg = format!("Database error during pruning: {:?}", e);
                        eprintln!("[Pruning] Pruning failed for volume {}: {}", vol, msg);
                        let _ = storage::log_pruning_run(&vol, "FAILED", &msg);
                    }
                }
            }
            Err(e) => {
                let msg = format!("Failed to get db connection: {:?}", e);
                eprintln!("[Pruning] Pruning failed for volume {}: {}", vol, msg);
                let _ = storage::log_pruning_run(&vol, "FAILED", &msg);
            }
        }
    }
}

/// Start the JSON-RPC daemon server over named pipe/IPC.
pub async fn run_server(
    pipe_path: &str,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) -> io::Result<()> {
    let db_path =
        storage::init_db().map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    println!("[Daemon] SQLite Database initialized at {:?}", db_path);

    let conn = storage::get_db_connection()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    // Detect NTFS volumes and spawn Scanner, Watcher, and Drain Engine
    let volumes = platform_windows::detect_ntfs_volumes();
    println!("[Daemon] Detected volumes to monitor: {:?}", volumes);

    // Initialize search index and trigger rebuild once all volumes are Live
    search::check_and_trigger_rebuild(volumes.clone());

    for vol in volumes {
        let vol_clone_watcher = vol.clone();
        let vol_clone_scanner = vol.clone();
        let vol_clone_drain = vol.clone();

        // Query the max mutation sequence currently in database for this volume
        let startup_seq: i64 = conn
            .query_row(
                "SELECT MAX(sequence) FROM mutation_log WHERE volume = ?1",
                [&vol],
                |row| row.get::<_, Option<i64>>(0),
            )
            .unwrap_or(None)
            .unwrap_or(0);

        // Initialize state
        let tracker = core_types::get_volume_tracker(&vol);
        *tracker.state.lock().unwrap() = DaemonState::BaselineScanning;

        let usn_start = platform_windows::get_usn_cursor(&vol).unwrap_or(0);

        // Spawn Watcher task
        tokio::spawn(async move {
            if let Err(e) = platform_windows::watch_usn_journal(&vol_clone_watcher, usn_start).await
            {
                eprintln!(
                    "[Daemon] Error in USN Watcher for {}: {:?}",
                    vol_clone_watcher, e
                );
            }
        });

        // Spawn Drain Engine task
        tokio::spawn(async move {
            drain::run_drain_engine(vol_clone_drain, startup_seq).await;
        });

        // Spawn Scanner thread
        std::thread::spawn(move || {
            if let Err(e) = scanner::scan_volume(&vol_clone_scanner) {
                eprintln!(
                    "[Daemon] Error in Scanner for {}: {:?}",
                    vol_clone_scanner, e
                );
            }
            // Transition state to Reconciling
            let tracker = core_types::get_volume_tracker(&vol_clone_scanner);
            *tracker.state.lock().unwrap() = DaemonState::Reconciling;
        });
    }

    // Spawn pruning background task
    let (pruning_shutdown_tx, mut pruning_shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let mut last_test_run = std::time::Instant::now();

        loop {
            let test_interval_secs = std::env::var("DISKTRACKER_TEST_PRUNE_INTERVAL_SECS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok());

            tokio::select! {
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(10)) => {
                    if let Some(secs) = test_interval_secs {
                        if last_test_run.elapsed().as_secs() >= secs {
                            last_test_run = std::time::Instant::now();
                            run_pruning_cycle().await;
                        }
                    } else {
                        let should_run = match get_last_successful_pruning_time() {
                            Some(last_run) => {
                                let elapsed = chrono::Utc::now().signed_duration_since(last_run);
                                elapsed >= chrono::Duration::hours(24)
                            }
                            None => true, // Never run successfully, run now
                        };
                        if should_run {
                            run_pruning_cycle().await;
                        }
                    }
                }
                _ = &mut pruning_shutdown_rx => {
                    break;
                }
            }
        }
    });

    // Spawn auto-snapshot background task
    let (_auto_snap_shutdown_tx, mut auto_snap_shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
                    let config = config_mgr::load_config();
                    if config.auto_snapshot {
                        let dur = match config_mgr::parse_any_duration(&config.auto_snapshot_interval) {
                            Ok(d) => d,
                            Err(e) => {
                                eprintln!("[Auto-Snapshot] Invalid interval '{}': {}. Defaulting to 24h.", config.auto_snapshot_interval, e);
                                chrono::Duration::hours(24)
                            }
                        };

                        let should_run = match snapshots::get_last_auto_snapshot_time() {
                            Some(last_time) => {
                                let elapsed = chrono::Utc::now().signed_duration_since(last_time);
                                elapsed >= dur
                            }
                            None => true, // Never run successfully, run now immediately
                        };

                        if should_run {
                            if let Err(e) = snapshots::trigger_auto_snapshot_for_all_volumes() {
                                eprintln!("[Auto-Snapshot] Error during auto-snapshot: {}", e);
                            }
                        }
                    }
                }
                _ = &mut auto_snap_shutdown_rx => {
                    break;
                }
            }
        }
    });

    let mut listener = create_listener(pipe_path).await?;
    let started_at = chrono::Utc::now();
    loop {
        tokio::select! {
            accept_res = listener.accept() => {
                match accept_res {
                    Ok(stream) => {
                        let db_path_clone = db_path.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, started_at, db_path_clone).await {
                                eprintln!("[Daemon] Error handling connection: {:?}", e);
                            }
                        });
                    }
                    Err(e) => {
                        eprintln!("[Daemon] Error accepting connection: {:?}", e);
                    }
                }
            }
            _ = &mut shutdown_rx => {
                println!("[Daemon] Shutdown signal received. Stopping server.");
                let _ = pruning_shutdown_tx.send(());
                break;
            }
        }
    }
    Ok(())
}

async fn handle_connection<S>(
    stream: S,
    started_at: chrono::DateTime<chrono::Utc>,
    db_path: std::path::PathBuf,
) -> io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = buf_reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            break; // connection closed
        }

        let request: Result<JsonRpcRequest, _> = serde_json::from_str(&line);
        let response = match request {
            Ok(req) => {
                if req.jsonrpc != "2.0" {
                    JsonRpcResponse::error(
                        req.id,
                        -32600,
                        "Invalid Request: jsonrpc version must be 2.0".to_string(),
                    )
                } else {
                    handle_request(req, started_at, db_path.clone()).await
                }
            }
            Err(_) => JsonRpcResponse::error(None, -32700, "Parse error: invalid JSON".to_string()),
        };

        let mut resp_bytes = serde_json::to_vec(&response)?;
        resp_bytes.push(b'\n');
        writer.write_all(&resp_bytes).await?;
        writer.flush().await?;
    }

    Ok(())
}

fn get_volume_progress(volume: &str, state: DaemonState) -> VolumeProgress {
    let tracker = core_types::get_volume_tracker(volume);

    {
        let mut tracker_state = tracker.state.lock().unwrap();
        *tracker_state = state;
    }

    let dirs_scanned = tracker
        .dirs_scanned
        .load(std::sync::atomic::Ordering::Relaxed);
    let files_scanned = tracker
        .files_scanned
        .load(std::sync::atomic::Ordering::Relaxed);
    let current_path = tracker.current_path.lock().unwrap().clone();
    let usn_start = tracker.usn_start.load(std::sync::atomic::Ordering::Relaxed);
    let events_buffered = tracker
        .events_buffered
        .load(std::sync::atomic::Ordering::Relaxed);
    let replaying = tracker.replaying.load(std::sync::atomic::Ordering::Relaxed);
    let mutations_replayed = tracker
        .mutations_replayed
        .load(std::sync::atomic::Ordering::Relaxed);
    let mutations_total = if tracker
        .has_mutations_total
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        Some(
            tracker
                .mutations_total
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    } else {
        None
    };

    VolumeProgress {
        state,
        scanner: ScannerProgress {
            dirs_scanned,
            files_scanned,
            current_path,
        },
        watcher: WatcherProgress {
            usn_start,
            events_buffered,
        },
        drain: DrainProgress {
            replaying,
            mutations_replayed,
            mutations_total,
        },
    }
}

async fn handle_request(
    req: JsonRpcRequest,
    started_at: chrono::DateTime<chrono::Utc>,
    db_path: std::path::PathBuf,
) -> JsonRpcResponse {
    match req.method.as_str() {
        "status" => {
            let registered_volumes = core_types::get_registered_volumes();

            let mut volumes = std::collections::HashMap::new();
            let mut all_live = true;

            for vol in registered_volumes {
                let tracker = core_types::get_volume_tracker(&vol);
                let state = { *tracker.state.lock().unwrap() };
                if state != DaemonState::Live {
                    all_live = false;
                }
                let progress = get_volume_progress(&vol, state);
                volumes.insert(vol, progress);
            }

            let aggregate_state = if volumes.is_empty() {
                DaemonState::Live
            } else if all_live {
                DaemonState::Live
            } else {
                DaemonState::BaselineScanning
            };

            let last_db_modified = std::fs::metadata(&db_path)
                .and_then(|m| m.modified())
                .map(|t| chrono::DateTime::<chrono::Utc>::from(t))
                .ok();

            let snapshot = ProgressSnapshot {
                daemon_pid: std::process::id(),
                db_path,
                started_at,
                state: aggregate_state,
                volumes,
                last_db_modified,
            };

            match serde_json::to_value(&snapshot) {
                Ok(val) => JsonRpcResponse::success(req.id, val),
                Err(e) => JsonRpcResponse::error(
                    req.id,
                    -32603,
                    format!("Internal error serializing status: {}", e),
                ),
            }
        }
        "config_get" => {
            let key = req.params.get("key").and_then(|k| k.as_str()).unwrap_or("");
            if key != "retention" && key != "retention-days" && key != "fuzzy" && key != "auto-snapshot" && key != "auto-snapshot-interval" {
                return JsonRpcResponse::error(
                    req.id,
                    -32602,
                    "Invalid config key. Valid keys are: retention, retention-days, fuzzy, auto-snapshot, auto-snapshot-interval".to_string(),
                );
            }
            let config = config_mgr::load_config();
            let value = match key {
                "fuzzy" => config.fuzzy.to_string(),
                "auto-snapshot" => config.auto_snapshot.to_string(),
                "auto-snapshot-interval" => config.auto_snapshot_interval.clone(),
                _ => config.retention.clone()
            };
            let res = serde_json::json!({
                "key": key,
                "value": value
            });
            JsonRpcResponse::success(req.id, res)
        }
        "config_set" => {
            let key = req.params.get("key").and_then(|k| k.as_str()).unwrap_or("");
            let val_str = req
                .params
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if key != "retention" && key != "retention-days" && key != "fuzzy" && key != "auto-snapshot" && key != "auto-snapshot-interval" {
                return JsonRpcResponse::error(
                    req.id,
                    -32602,
                    "Invalid config key. Valid keys are: retention, retention-days, fuzzy, auto-snapshot, auto-snapshot-interval".to_string(),
                );
            }

            if key == "fuzzy" {
                let parsed_bool = match val_str.trim().to_lowercase().as_str() {
                    "true" | "on" | "1" | "yes" => true,
                    "false" | "off" | "0" | "no" => false,
                    _ => {
                        return JsonRpcResponse::error(
                            req.id,
                            -32602,
                            "Invalid boolean value. Acceptable values: true, false, on, off".to_string(),
                        );
                    }
                };

                let mut config = config_mgr::load_config();
                config.fuzzy = parsed_bool;
                if let Err(e) = config_mgr::save_config(&config) {
                    return JsonRpcResponse::error(
                        req.id,
                        -32603,
                        format!("Failed to save config: {}", e),
                    );
                }
                let res = serde_json::json!({
                    "key": key,
                    "value": parsed_bool.to_string(),
                    "message": "Fuzzy search setting updated."
                });
                return JsonRpcResponse::success(req.id, res);
            }

            if key == "auto-snapshot" {
                let parsed_bool = match val_str.trim().to_lowercase().as_str() {
                    "true" | "on" | "1" | "yes" => true,
                    "false" | "off" | "0" | "no" => false,
                    _ => {
                        return JsonRpcResponse::error(
                            req.id,
                            -32602,
                            "Invalid boolean value. Acceptable values: true, false, on, off".to_string(),
                        );
                    }
                };

                let mut config = config_mgr::load_config();
                config.auto_snapshot = parsed_bool;
                if let Err(e) = config_mgr::save_config(&config) {
                    return JsonRpcResponse::error(
                        req.id,
                        -32603,
                        format!("Failed to save config: {}", e),
                    );
                }
                let res = serde_json::json!({
                    "key": key,
                    "value": parsed_bool.to_string(),
                    "message": "Auto-snapshot setting updated."
                });
                return JsonRpcResponse::success(req.id, res);
            }

            if key == "auto-snapshot-interval" {
                match config_mgr::parse_any_duration(val_str) {
                    Ok(_) => {
                        let mut config = config_mgr::load_config();
                        config.auto_snapshot_interval = val_str.to_string();
                        if let Err(e) = config_mgr::save_config(&config) {
                            return JsonRpcResponse::error(
                                req.id,
                                -32603,
                                format!("Failed to save config: {}", e),
                            );
                        }
                        let res = serde_json::json!({
                            "key": key,
                            "value": val_str,
                            "message": "Auto-snapshot interval updated."
                        });
                        return JsonRpcResponse::success(req.id, res);
                    }
                    Err(e) => return JsonRpcResponse::error(req.id, -32602, e),
                }
            }

            match config_mgr::parse_duration(val_str) {
                Ok(_) => {
                    let mut config = config_mgr::load_config();
                    config.retention = val_str.to_string();
                    if let Err(e) = config_mgr::save_config(&config) {
                        return JsonRpcResponse::error(
                            req.id,
                            -32603,
                            format!("Failed to save config: {}", e),
                        );
                    }
                    tokio::spawn(run_pruning_cycle());
                    let res = serde_json::json!({
                        "key": key,
                        "value": val_str,
                        "message": "Retention setting updated. Pruning cycle triggered immediately."
                    });
                    JsonRpcResponse::success(req.id, res)
                }
                Err(e) => JsonRpcResponse::error(req.id, -32602, e),
            }
        }
        "get_pruning_logs" => match storage::get_latest_pruning_runs() {
            Ok(runs) => {
                let serialized_runs = runs
                    .iter()
                    .map(|run| {
                        serde_json::json!({
                            "volume": run.volume,
                            "run_at": run.run_at,
                            "status": run.status,
                            "details": run.details
                        })
                    })
                    .collect::<Vec<_>>();
                JsonRpcResponse::success(req.id, serde_json::json!(serialized_runs))
            }
            Err(e) => JsonRpcResponse::error(
                req.id,
                -32603,
                format!("Failed to query pruning logs: {}", e),
            ),
        },
        "check_db_integrity" => match storage::get_db_connection() {
            Ok(conn) => {
                let integrity: Result<String, rusqlite::Error> =
                    conn.query_row("PRAGMA integrity_check", [], |row| row.get(0));
                match integrity {
                    Ok(status) => {
                        let res = serde_json::json!({ "status": status });
                        JsonRpcResponse::success(req.id, res)
                    }
                    Err(e) => JsonRpcResponse::error(
                        req.id,
                        -32603,
                        format!("Failed to check integrity: {}", e),
                    ),
                }
            }
            Err(e) => JsonRpcResponse::error(
                req.id,
                -32603,
                format!("Failed to connect to database: {}", e),
            ),
        },
        "get_history" => {
            let path = match req.params.get("path").and_then(|p| p.as_str()) {
                Some(p) => p,
                None => {
                    return JsonRpcResponse::error_with_data(
                        req.id,
                        -32602,
                        "Path is required".to_string(),
                        Some(serde_json::json!({
                            "code": "E_INVALID_PARAMS"
                        }))
                    );
                }
            };
            let since = req.params.get("since").and_then(|t| t.as_i64());
            let until = req.params.get("until").and_then(|t| t.as_i64());
            let kind = req.params.get("kind").and_then(|k| k.as_str());
            let collapse = req.params.get("collapse").and_then(|c| c.as_bool()).unwrap_or(false);
            let limit = req.params.get("limit").and_then(|l| l.as_u64()).unwrap_or(100) as usize;
            let cursor = req.params.get("cursor").and_then(|c| c.as_str());

            match storage::get_db_connection() {
                Ok(conn) => {
                    match history::get_history(&conn, path, since, until, kind, collapse, limit, cursor) {
                        Ok(resp) => {
                            JsonRpcResponse::success(req.id, serde_json::json!(resp))
                        }
                        Err(e) => {
                            if e.contains("not found") || e.contains("component") {
                                JsonRpcResponse::error_with_data(
                                    req.id,
                                    -32002,
                                    format!("Couldn't find \"{}\". Check the path and try again.", path),
                                    Some(serde_json::json!({
                                        "code": "E_NOT_FOUND",
                                        "input": path
                                    }))
                                )
                            } else {
                                JsonRpcResponse::error(req.id, -32603, e)
                            }
                        }
                    }
                }
                Err(e) => JsonRpcResponse::error(
                    req.id,
                    -32603,
                    format!("Failed to connect to database: {}", e),
                ),
            }
        },
        "search_query" => {
            if search::REBUILD_IN_PROGRESS.load(std::sync::atomic::Ordering::SeqCst) {
                let progress =
                    search::REBUILD_PROGRESS_COUNT.load(std::sync::atomic::Ordering::SeqCst);
                return JsonRpcResponse::error_with_data(
                    req.id,
                    -32001,
                    "Still finishing an index update — try again in a moment.".to_string(),
                    Some(serde_json::json!({
                        "code": "E_SEARCH_INDEX_STALE",
                        "progress": progress
                    })),
                );
            }

            let query_str = req
                .params
                .get("query")
                .and_then(|q| q.as_str())
                .unwrap_or("");
            let path_filter = req.params.get("path").and_then(|p| p.as_str());
            let ext_filter = req.params.get("ext").and_then(|e| e.as_str());
            let volume_filter = req.params.get("volume").and_then(|v| v.as_str());
            let min_size = req.params.get("min_size").and_then(|s| s.as_u64());
            let max_size = req.params.get("max_size").and_then(|s| s.as_u64());
            let modified_after = req.params.get("modified_after").and_then(|t| t.as_i64());
            let modified_before = req.params.get("modified_before").and_then(|t| t.as_i64());
            let hidden_filter = req.params.get("hidden").and_then(|h| h.as_bool());
            let system_filter = req.params.get("system").and_then(|s| s.as_bool());
            let limit = req
                .params
                .get("limit")
                .and_then(|l| l.as_u64())
                .unwrap_or(100) as usize;

            match search::execute_search(
                query_str,
                path_filter,
                ext_filter,
                volume_filter,
                min_size,
                max_size,
                modified_after,
                modified_before,
                hidden_filter,
                system_filter,
                limit,
            ) {
                Ok(raw_docs) => {
                    // Reconcile: remove any docs no longer present in the facts table.
                    // This catches deletions that the drain engine hasn't flushed to the
                    // search index yet, and auto-cleans stale index entries on the fly.
                    let docs = match storage::get_db_connection() {
                        Ok(conn) => search::reconcile_search_results(raw_docs, &conn),
                        Err(_) => raw_docs,
                    };

                    let si = match search::init_search_index() {
                        Ok(s) => s,
                        Err(e) => return JsonRpcResponse::error(req.id, -32603, e),
                    };

                    let mut results = Vec::new();
                    let mut volumes_incomplete = std::collections::HashSet::new();

                    let registered_volumes = core_types::get_registered_volumes();
                    for vol in registered_volumes {
                        let tracker = core_types::get_volume_tracker(&vol);
                        let state = { *tracker.state.lock().unwrap() };
                        if state != DaemonState::Live {
                            volumes_incomplete.insert(vol);
                        }
                    }

                    for (doc, doc_score) in docs {
                        let doc_name = doc
                            .get_first(si.name)
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let doc_path = doc
                            .get_first(si.path)
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let doc_ext = doc
                            .get_first(si.ext)
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let doc_volume = doc
                            .get_first(si.volume)
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let doc_size = doc.get_first(si.size).and_then(|v| v.as_u64()).unwrap_or(0);
                        let doc_modified = doc
                            .get_first(si.modified_at)
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        let doc_is_dir = doc
                            .get_first(si.is_directory)
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0)
                            != 0;
                        let doc_file_id = doc
                            .get_first(si.file_id)
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);

                        let doc_attrs: Vec<String> = doc
                            .get_all(si.attributes)
                            .map(|v| v.as_str().unwrap_or("").to_string())
                            .collect();

                        results.push(serde_json::json!({
                            "name": doc_name,
                            "path": doc_path,
                            "ext": doc_ext,
                            "volume": doc_volume,
                            "size": doc_size,
                            "modified_at": doc_modified,
                            "is_directory": doc_is_dir,
                            "file_id": doc_file_id,
                            "attributes": doc_attrs,
                            "score": doc_score,
                        }));
                    }

                    let last_db_modified = std::fs::metadata(&db_path)
                        .and_then(|m| m.modified())
                        .map(|t| chrono::DateTime::<chrono::Utc>::from(t))
                        .ok();

                    let res = serde_json::json!({
                        "results": results,
                        "volumes_incomplete": volumes_incomplete.into_iter().collect::<Vec<_>>(),
                        "last_db_modified": last_db_modified,
                        "index_rebuilding": search::REBUILD_IN_PROGRESS.load(std::sync::atomic::Ordering::SeqCst),
                    });
                    JsonRpcResponse::success(req.id, res)
                }
                Err(e) => JsonRpcResponse::error(req.id, -32603, e),
            }
        }
        "snapshot_create" => {
            map_rpc_result(req.id, snapshots::handle_snapshot_create(req.params))
        }
        "job.completed" => {
            map_rpc_result(req.id, snapshots::handle_job_completed(req.params))
        }
        "snapshot_list" => {
            map_rpc_result(req.id, snapshots::handle_snapshot_list(req.params))
        }
        "snapshot_diff" => {
            map_rpc_result(req.id, snapshots::handle_snapshot_diff(req.params))
        }
        "get_search_rebuild_status" => {
            let in_progress = search::REBUILD_IN_PROGRESS.load(std::sync::atomic::Ordering::SeqCst);
            let progress = search::REBUILD_PROGRESS_COUNT.load(std::sync::atomic::Ordering::SeqCst);
            JsonRpcResponse::success(
                req.id,
                serde_json::json!({
                    "in_progress": in_progress,
                    "progress": progress
                }),
            )
        }
        "uninstall_cleanup" => {
            let delete_snapshot = req.params.get("delete_snapshot").and_then(|v| v.as_bool()).unwrap_or(false);

            if let Ok(db_dir) = storage::get_db_dir() {
                let mut config_path = db_dir.clone();
                config_path.push("config.toml");
                if config_path.exists() {
                    let _ = std::fs::remove_file(&config_path);
                }
            }

            match storage::get_db_connection() {
                Ok(conn) => {
                    let _ = conn.execute("DELETE FROM facts", []);
                    let _ = conn.execute("DELETE FROM mutation_log", []);
                    let _ = conn.execute("DELETE FROM drain_state", []);
                    let _ = conn.execute("DELETE FROM pruning_log", []);
                    if delete_snapshot {
                        let _ = conn.execute("DELETE FROM volume_snapshots", []);
                        let _ = conn.execute("DELETE FROM parent_snapshots", []);
                    }
                    let _ = conn.execute("VACUUM", []);
                }
                Err(e) => {
                    return JsonRpcResponse::error(
                        req.id,
                        -32603,
                        format!("Failed to connect to database for cleanup: {}", e),
                    );
                }
            }

            JsonRpcResponse::success(req.id, serde_json::json!({ "status": "ok" }))
        }
        _ => JsonRpcResponse::error(req.id, -32601, format!("Method not found: {}", req.method)),
    }
}

fn get_last_successful_pruning_time() -> Option<chrono::DateTime<chrono::Utc>> {
    if let Ok(runs) = storage::get_latest_pruning_runs() {
        for run in runs {
            if run.status == "SUCCESS" {
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&run.run_at) {
                    return Some(dt.with_timezone(&chrono::Utc));
                }
            }
        }
    }
    None
}

fn map_rpc_result(id: Option<serde_json::Value>, res: Result<serde_json::Value, String>) -> JsonRpcResponse {
    match res {
        Ok(val) => JsonRpcResponse::success(id, val),
        Err(err) => {
            if err.starts_with("E_INVALID_PARAMS: ") {
                let msg = err.trim_start_matches("E_INVALID_PARAMS: ");
                JsonRpcResponse::error_with_data(
                    id,
                    -32602,
                    msg.to_string(),
                    Some(serde_json::json!({ "code": "E_INVALID_PARAMS" }))
                )
            } else if err.starts_with("E_NOT_FOUND: ") {
                let msg = err.trim_start_matches("E_NOT_FOUND: ");
                JsonRpcResponse::error_with_data(
                    id,
                    -32002,
                    msg.to_string(),
                    Some(serde_json::json!({ "code": "E_NOT_FOUND" }))
                )
            } else if err.starts_with("E_SNAPSHOT_DATA_EXPIRED: ") {
                let msg = err.trim_start_matches("E_SNAPSHOT_DATA_EXPIRED: ");
                JsonRpcResponse::error_with_data(
                    id,
                    -32003,
                    msg.to_string(),
                    Some(serde_json::json!({
                        "code": "E_SNAPSHOT_DATA_EXPIRED",
                        "retention": config_mgr::load_config().retention
                    }))
                )
            } else {
                JsonRpcResponse::error(id, -32603, err)
            }
        }
    }
}
