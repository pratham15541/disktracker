mod drain;

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
}

/// Start the JSON-RPC daemon server over named pipe/IPC.
pub async fn run_server(pipe_path: &str) -> io::Result<()> {
    let db_path =
        storage::init_db().map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    println!("[Daemon] SQLite Database initialized at {:?}", db_path);

    let conn = storage::get_db_connection()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    // Detect NTFS volumes and spawn Scanner, Watcher, and Drain Engine
    let volumes = platform_windows::detect_ntfs_volumes();
    println!("[Daemon] Detected volumes to monitor: {:?}", volumes);

    for vol in volumes {
        let vol_clone_watcher = vol.clone();
        let vol_clone_scanner = vol.clone();
        let vol_clone_drain = vol.clone();

        // Query the max mutation sequence currently in database for this volume
        let startup_seq: i64 = conn.query_row(
            "SELECT MAX(sequence) FROM mutation_log WHERE volume = ?1",
            [&vol],
            |row| row.get::<_, Option<i64>>(0)
        ).unwrap_or(None).unwrap_or(0);

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

    let mut listener = create_listener(pipe_path).await?;
    let started_at = chrono::Utc::now();
    loop {
        match listener.accept().await {
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

    let dirs_scanned = tracker.dirs_scanned.load(std::sync::atomic::Ordering::Relaxed);
    let files_scanned = tracker.files_scanned.load(std::sync::atomic::Ordering::Relaxed);
    let current_path = tracker.current_path.lock().unwrap().clone();
    let usn_start = tracker.usn_start.load(std::sync::atomic::Ordering::Relaxed);
    let events_buffered = tracker.events_buffered.load(std::sync::atomic::Ordering::Relaxed);
    let replaying = tracker.replaying.load(std::sync::atomic::Ordering::Relaxed);
    let mutations_replayed = tracker.mutations_replayed.load(std::sync::atomic::Ordering::Relaxed);
    let mutations_total = if tracker.has_mutations_total.load(std::sync::atomic::Ordering::Relaxed) {
        Some(tracker.mutations_total.load(std::sync::atomic::Ordering::Relaxed))
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

            let snapshot = ProgressSnapshot {
                daemon_pid: std::process::id(),
                db_path,
                started_at,
                state: aggregate_state,
                volumes,
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
        _ => JsonRpcResponse::error(req.id, -32601, format!("Method not found: {}", req.method)),
    }
}
