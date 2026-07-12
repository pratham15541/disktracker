use api::{JsonRpcRequest, JsonRpcResponse, JsonRpcError};
use clap::{Parser, Subcommand};
use core_types::{DaemonState, ProgressSnapshot, VolumeProgress};
use std::io::Write;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const PIPE_PATH: &str = r"\\.\pipe\disktracker";

#[derive(Parser)]
#[command(name = "disktracker")]
#[command(about = "DiskTracker — Windows file-system observation daemon", long_about = None)]
struct Cli {
    /// Output raw JSON instead of human-readable text
    #[arg(long, global = true)]
    json: bool,

    /// Show verbose output with detailed columns/times
    #[arg(long, short, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize storage, register/start the daemon, and kick off indexing
    Init,
    /// Query the status of the running daemon
    Status,
    /// Run diagnostic checks (permissions, journal, DB integrity)
    Doctor,
    /// Stop the running daemon, delete named pipe, and clean up database files/folders
    Uninstall,
    /// Manage the Windows Service (register, unregister, start, stop)
    Service {
        #[command(subcommand)]
        subcommand: ServiceCommands,
    },
    /// View or modify daemon configuration
    Config {
        #[command(subcommand)]
        subcommand: ConfigCommands,
    },
    /// Search files/directories indexed in the database
    Search {
        /// The text search query (matches filenames)
        #[arg(index = 1, default_value = "*")]
        query: String,
        /// Filter by relative path prefix under the volume
        #[arg(long)]
        path: Option<String>,
        /// Filter by exact file extension (e.g. txt, pdf)
        #[arg(long)]
        ext: Option<String>,
        /// Filter by volume (e.g. C:, D:)
        #[arg(long)]
        volume: Option<String>,
        /// Filter by minimum size in bytes
        #[arg(long)]
        min_size: Option<u64>,
        /// Filter by maximum size in bytes
        #[arg(long)]
        max_size: Option<u64>,
        /// Filter files modified after a given duration (e.g. 2d, 1h) or UTC datetime (RFC3339)
        #[arg(long)]
        modified_after: Option<String>,
        /// Filter files modified before a given duration (e.g. 2d, 1h) or UTC datetime (RFC3339)
        #[arg(long)]
        modified_before: Option<String>,
        /// Filter hidden files (true or false)
        #[arg(long)]
        hidden: Option<bool>,
        /// Filter system files (true or false)
        #[arg(long)]
        system: Option<bool>,
        /// Max number of search results to return
        #[arg(long, default_value = "100")]
        limit: usize,
        /// Cursor for pagination
        #[arg(long)]
        cursor: Option<String>,
        /// Output the results in JSON format
        #[arg(long)]
        json: bool,
        /// Show verbose output
        #[arg(long)]
        verbose: bool,
    },
    /// View the mutation history of a specific file or directory
    History {
        /// The path of the file or directory to query history for (defaults to current directory)
        #[arg(index = 1)]
        path: Option<String>,
        /// Filter history since a duration (e.g. 2d, 1h) or UTC datetime (RFC3339)
        #[arg(long)]
        since: Option<String>,
        /// Filter history until a duration (e.g. 2d, 1h) or UTC datetime (RFC3339)
        #[arg(long)]
        until: Option<String>,
        /// Filter by mutation kind (created, modified, deleted, renamed)
        #[arg(long)]
        kind: Option<String>,
        /// Collapse consecutive same-kind entries
        #[arg(long)]
        collapse: bool,
        /// Max number of history entries to return
        #[arg(long, default_value = "100")]
        limit: usize,
        /// Cursor for pagination
        #[arg(long)]
        cursor: Option<String>,
        /// Output the results in JSON format
        #[arg(long)]
        json: bool,
        /// Show verbose output
        #[arg(long)]
        verbose: bool,
    },
    /// Internal subcommand to start the IPC daemon server
    #[command(hide = true)]
    Daemon {
        /// Start in Windows Service mode
        #[arg(long)]
        service: bool,
    },
    /// Manage and diff volume snapshots
    Snapshot {
        #[command(subcommand)]
        subcommand: SnapshotCommands,
    },
}

#[derive(Subcommand)]
enum ServiceCommands {
    /// Register DiskTracker as a Windows Service
    Register,
    /// Stop and delete the Windows Service
    Unregister,
    /// Start the registered Windows Service
    Start,
    /// Stop the registered Windows Service
    Stop,
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Get the value of a configuration key. Examples: 'config get fuzzy', 'config get retention'
    Get {
        /// The config key to query (retention, retention-days, fuzzy)
        key: String,
    },
    /// Set the value of a configuration key. Examples: 'config set fuzzy false', 'config set retention 24h', 'config set retention-days 7'
    Set {
        /// The config key to modify (retention, retention-days, fuzzy)
        key: String,
        /// The new value to assign
        value: String,
    },
}

#[derive(Subcommand)]
enum SnapshotCommands {
    /// Create a new snapshot of a volume
    Create {
        /// Label for the snapshot (auto-generated if omitted)
        #[arg(long)]
        label: Option<String>,
        /// The volume to snapshot (defaults to current drive, e.g. C:)
        #[arg(long)]
        volume: Option<String>,
    },
    /// List all snapshots in the database
    List {
        /// Filter snapshots by volume (e.g. C:, D:)
        #[arg(long)]
        volume: Option<String>,
        /// Max number of snapshots to return
        #[arg(long, default_value = "20")]
        limit: usize,
        /// Cursor for pagination
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Diff two snapshots to see net file mutations
    Diff {
        /// First snapshot label or ID
        snapshot_a: String,
        /// Second snapshot label or ID
        snapshot_b: String,
        /// Filter diff results by path prefix
        #[arg(long)]
        path: Option<String>,
        /// Max number of diff results to return
        #[arg(long, default_value = "100")]
        limit: usize,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Daemon { service } => {
            #[cfg(windows)]
            {
                if !platform_windows::is_elevated() {
                    print_error(
                        cli.json,
                        "E_NOT_ELEVATED",
                        "DiskTracker daemon must be run with Administrator privileges.",
                        None,
                    );
                    std::process::exit(1);
                }
            }

            if *service {
                let run_server_fn = |rx| {
                    let rt = tokio::runtime::Builder::new_multi_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    rt.block_on(async {
                        if let Err(e) = api::run_server(PIPE_PATH, rx).await {
                            eprintln!("[Daemon] Service daemon error: {:?}", e);
                        }
                    });
                };
                if let Err(e) = platform_windows::run_as_service(run_server_fn) {
                    eprintln!("[Daemon] Failed to run as service: {:?}", e);
                    std::process::exit(1);
                }
            } else {
                let (tx, rx) = tokio::sync::oneshot::channel();
                tokio::spawn(async move {
                    let _ = tokio::signal::ctrl_c().await;
                    let _ = tx.send(());
                });
                if let Err(e) = api::run_server(PIPE_PATH, rx).await {
                    eprintln!("[Daemon] Daemon error: {:?}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Init => {
            #[cfg(windows)]
            {
                if !platform_windows::is_elevated() {
                    print_error(
                        cli.json,
                        "E_NOT_ELEVATED",
                        "DiskTracker must be run as Administrator/elevated terminal on Windows to watch drives.",
                        None,
                    );
                    std::process::exit(1);
                }
            }

            // 1. Check if daemon is already running
            let already_running = match query_status().await {
                Ok(resp) => {
                    if let Some(result) = resp.result {
                        serde_json::from_value::<ProgressSnapshot>(result).ok()
                    } else {
                        None
                    }
                }
                Err(_) => None,
            };

            if let Some(snap) = already_running {
                if snap.state == DaemonState::Live {
                    println!(
                        "[Cli] Daemon is already running and Live (PID {}).",
                        snap.daemon_pid
                    );
                    return Ok(());
                }
                println!(
                    "[Cli] Daemon is already running (PID {}). Attaching to live progress...",
                    snap.daemon_pid
                );
            } else {
                #[allow(unused_mut)]
                let mut started_as_service = false;
                #[cfg(windows)]
                {
                    println!("[Cli] Registering DiskTracker daemon as a Windows Service...");
                    match platform_windows::register_service() {
                        Ok(_) => {
                            println!("  [OK] Service registered successfully. Starting service...");
                            match platform_windows::start_service() {
                                Ok(_) => {
                                    println!("  [OK] Service started successfully.");
                                    started_as_service = true;
                                }
                                Err(e) => {
                                    eprintln!("  [WARNING] Failed to start service: {:?}. Falling back to background process...", e);
                                }
                            }
                        }
                        Err(e) => {
                            println!("  [INFO] Service registration failed: {:?}. Trying to start existing service...", e);
                            match platform_windows::start_service() {
                                Ok(_) => {
                                    println!("  [OK] Service started successfully.");
                                    started_as_service = true;
                                }
                                Err(start_err) => {
                                    eprintln!("  [WARNING] Failed to start service: {:?}. Falling back to background process...", start_err);
                                }
                            }
                        }
                    }
                }

                if !started_as_service {
                    println!("[Cli] Starting DiskTracker daemon in the background...");
                    let current_exe = std::env::current_exe()?;
                    let mut cmd = std::process::Command::new(current_exe);
                    cmd.arg("daemon");
                    cmd.stdin(std::process::Stdio::null());
                    cmd.stdout(std::process::Stdio::null());
                    cmd.stderr(std::process::Stdio::null());

                    #[cfg(windows)]
                    {
                        use std::os::windows::process::CommandExt;
                        const DETACHED_PROCESS: u32 = 0x00000008;
                        cmd.creation_flags(DETACHED_PROCESS);
                    }

                    #[cfg(unix)]
                    {
                        use std::os::unix::process::CommandExt;
                        cmd.process_group(0);
                    }

                    cmd.spawn()?;
                }

                // Poll IPC until reachable (timeout after 5 seconds)
                let start_wait = std::time::Instant::now();
                let mut connected = false;
                while start_wait.elapsed().as_secs() < 5 {
                    if query_status().await.is_ok() {
                        connected = true;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }

                if !connected {
                    eprintln!(
                        "[Cli] Error: Failed to connect to the background daemon within 5 seconds."
                    );
                    std::process::exit(1);
                }
                println!("[Cli] Daemon started. Attaching to live progress...");
            }

            // 2. Poll progress until Live
            loop {
                match query_status().await {
                    Ok(resp) => {
                        if let Some(result) = resp.result {
                            if let Ok(snap) = serde_json::from_value::<ProgressSnapshot>(result) {
                                let mut volumes_sorted: Vec<(&String, &VolumeProgress)> =
                                    snap.volumes.iter().collect();
                                volumes_sorted.sort_by(|a, b| a.0.cmp(b.0));

                                let mut status_parts = Vec::new();
                                for (vol_name, vol_progress) in volumes_sorted {
                                    let vol_phase = match vol_progress.state {
                                        DaemonState::Starting => "Starting",
                                        DaemonState::BaselineScanning => "Scanning",
                                        DaemonState::Reconciling => "Reconciling",
                                        DaemonState::Live => "Live",
                                    };
                                    status_parts.push(format!(
                                        "{}: [{}] 📁 {} dirs | 📄 {} files",
                                        vol_name,
                                        vol_phase,
                                        vol_progress.scanner.dirs_scanned,
                                        vol_progress.scanner.files_scanned
                                    ));
                                }

                                let aggregate_phase = match snap.state {
                                    DaemonState::Starting => "Starting",
                                    DaemonState::BaselineScanning => "Scanning",
                                    DaemonState::Reconciling => "Reconciling",
                                    DaemonState::Live => "Live",
                                };

                                print!(
                                    "\r[{}] {}                  ",
                                    aggregate_phase,
                                    status_parts.join("  |  ")
                                );
                                use std::io::Write;
                                std::io::stdout().flush()?;

                                if snap.state == DaemonState::Live {
                                    println!("\n[Cli] Live: Baseline scan complete. Daemon (PID {}) is running in the background.", snap.daemon_pid);
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("\n[Cli] Connection to daemon lost: {}", e);
                        std::process::exit(1);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }
        Commands::Status => match query_status().await {
            Ok(resp) => {
                if let Some(err) = resp.error {
                    eprintln!(
                        "[Cli] Error response from daemon (code {}): {}",
                        err.code, err.message
                    );
                    std::process::exit(1);
                }
                if let Some(result) = resp.result {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("[Cli] Empty response from daemon.");
                }
            }
            Err(e) => {
                eprintln!("[Cli] Failed to connect to daemon: {}. Is the daemon running (disktracker init)?", e);
                std::process::exit(1);
            }
        },
        Commands::Doctor => {
            run_doctor().await;
        }
        Commands::Uninstall => {
            if let Err(e) = run_uninstall().await {
                eprintln!("[Cli] Uninstall failed: {:?}", e);
                std::process::exit(1);
            }
        }
        Commands::Service { subcommand } => match subcommand {
            ServiceCommands::Register => {
                #[cfg(windows)]
                {
                    match platform_windows::register_service() {
                        Ok(_) => {
                            println!("[Cli] Windows Service 'DiskTracker' registered successfully.")
                        }
                        Err(e) => {
                            eprintln!("[Cli] Error registering Windows Service: {:?}", e);
                            std::process::exit(1);
                        }
                    }
                }
                #[cfg(not(windows))]
                {
                    println!("[Cli] Service commands are only supported on Windows.");
                }
            }
            ServiceCommands::Unregister => {
                #[cfg(windows)]
                {
                    let _ = platform_windows::stop_service();
                    match platform_windows::unregister_service() {
                        Ok(_) => println!(
                            "[Cli] Windows Service 'DiskTracker' unregistered successfully."
                        ),
                        Err(e) => {
                            eprintln!("[Cli] Error unregistering Windows Service: {:?}", e);
                            std::process::exit(1);
                        }
                    }
                }
                #[cfg(not(windows))]
                {
                    println!("[Cli] Service commands are only supported on Windows.");
                }
            }
            ServiceCommands::Start => {
                #[cfg(windows)]
                {
                    match platform_windows::start_service() {
                        Ok(_) => {
                            println!("[Cli] Windows Service 'DiskTracker' started successfully.")
                        }
                        Err(e) => {
                            eprintln!("[Cli] Error starting Windows Service: {:?}", e);
                            std::process::exit(1);
                        }
                    }
                }
                #[cfg(not(windows))]
                {
                    println!("[Cli] Service commands are only supported on Windows.");
                }
            }
            ServiceCommands::Stop => {
                #[cfg(windows)]
                {
                    match platform_windows::stop_service() {
                        Ok(_) => {
                            println!("[Cli] Windows Service 'DiskTracker' stopped successfully.")
                        }
                        Err(e) => {
                            eprintln!("[Cli] Error stopping Windows Service: {:?}", e);
                            std::process::exit(1);
                        }
                    }
                }
                #[cfg(not(windows))]
                {
                    println!("[Cli] Service commands are only supported on Windows.");
                }
            }
        },
        Commands::Config { subcommand } => match subcommand {
            ConfigCommands::Get { key } => {
                if key != "retention" && key != "retention-days" && key != "fuzzy" {
                    let err_msg = "Invalid config key. Valid keys are: retention, retention-days, fuzzy.\nExamples:\n  disktracker config get fuzzy\n  disktracker config get retention";
                    print_error(
                        cli.json,
                        "E_INVALID_PARAMS",
                        err_msg,
                        Some(serde_json::json!({ "valid_keys": ["retention", "retention-days", "fuzzy"] })),
                    );
                    std::process::exit(1);
                }

                let params = serde_json::json!({ "key": key });
                match query_rpc("config_get", params).await {
                    Ok(resp) => {
                        if let Some(err) = resp.error {
                            print_error(cli.json, "E_INVALID_PARAMS", &err.message, None);
                            std::process::exit(1);
                        }
                        if let Some(result) = resp.result {
                            if cli.json {
                                println!("{}", serde_json::to_string_pretty(&result).unwrap());
                            } else {
                                let val = result
                                    .get("value")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("30d");
                                println!("{} = {}", key, val);
                            }
                        }
                    }
                    Err(e) => {
                        let err_msg =
                            format!("Failed to connect to daemon: {}. Is the daemon running?", e);
                        print_error(cli.json, "E_INVALID_PARAMS", &err_msg, None);
                        std::process::exit(1);
                    }
                }
            }
            ConfigCommands::Set { key, value } => {
                if key != "retention" && key != "retention-days" && key != "fuzzy" {
                    let err_msg = "Invalid config key. Valid keys are: retention, retention-days, fuzzy.\nExamples:\n  disktracker config set fuzzy false\n  disktracker config set retention 24h\n  disktracker config set retention-days 7";
                    print_error(
                        cli.json,
                        "E_INVALID_PARAMS",
                        err_msg,
                        Some(serde_json::json!({ "valid_keys": ["retention", "retention-days", "fuzzy"] })),
                    );
                    std::process::exit(1);
                }

                let params = serde_json::json!({ "key": key, "value": value });
                match query_rpc("config_set", params).await {
                    Ok(resp) => {
                        if let Some(err) = resp.error {
                            print_error(cli.json, "E_INVALID_PARAMS", &err.message, None);
                            std::process::exit(1);
                        }
                        if let Some(result) = resp.result {
                            if cli.json {
                                println!("{}", serde_json::to_string_pretty(&result).unwrap());
                            } else {
                                let val = result
                                    .get("value")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let msg = result
                                    .get("message")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if !msg.is_empty() {
                                    println!("Set {} to {}. {}", key, val, msg);
                                } else {
                                    println!("Set {} to {}.", key, val);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let err_msg =
                            format!("Failed to connect to daemon: {}. Is the daemon running?", e);
                        print_error(cli.json, "E_INVALID_PARAMS", &err_msg, None);
                        std::process::exit(1);
                    }
                }
            }
        },
        Commands::Search {
            query,
            path,
            ext,
            volume,
            min_size,
            max_size,
            modified_after,
            modified_before,
            hidden,
            system,
            limit,
            cursor,
            json,
            verbose: _,
        } => {
            let mod_after_ts = if let Some(ref s) = modified_after {
                match parse_modified_time(s) {
                    Ok(dt) => Some(dt.timestamp()),
                    Err(e) => {
                        eprintln!("Error parsing modified_after: {}", e);
                        std::process::exit(1);
                    }
                }
            } else {
                None
            };

            let mod_before_ts = if let Some(ref s) = modified_before {
                match parse_modified_time(s) {
                    Ok(dt) => Some(dt.timestamp()),
                    Err(e) => {
                        eprintln!("Error parsing modified_before: {}", e);
                        std::process::exit(1);
                    }
                }
            } else {
                None
            };

            let mut search_volume = volume.clone();
            let mut search_path = path.clone();

            if let Some(ref p) = path {
                let p_buf = std::path::Path::new(p);
                let p_str = p_buf.to_string_lossy().to_string();
                let normalized = p_str.replace('\\', "/");
                if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
                    let drive = normalized[0..2].to_uppercase();
                    let remaining = normalized[2..].trim_start_matches('/').to_string();
                    search_volume = Some(drive);
                    search_path = if remaining.is_empty() {
                        None
                    } else {
                        Some(remaining)
                    };
                }
            }

            let params = serde_json::json!({
                "query": query,
                "path": search_path,
                "ext": ext,
                "volume": search_volume,
                "min_size": min_size,
                "max_size": max_size,
                "modified_after": mod_after_ts,
                "modified_before": mod_before_ts,
                "hidden": hidden,
                "system": system,
                "limit": limit,
                "cursor": cursor,
            });

            let mut first_attempt = true;
            loop {
                match query_rpc("search_query", params.clone()).await {
                    Ok(resp) => {
                        if let Some(err) = resp.error {
                            if let Some(ref data) = err.data {
                                if data.get("code").and_then(|v| v.as_str())
                                    == Some("E_SEARCH_INDEX_STALE")
                                {
                                    let initial_progress =
                                        data.get("progress").and_then(|v| v.as_u64()).unwrap_or(0);
                                    if first_attempt {
                                        print!(
                                            "Building search index: {} files indexed...",
                                            initial_progress
                                        );
                                        use std::io::Write;
                                        let _ = std::io::stdout().flush();
                                        first_attempt = false;
                                    }

                                    loop {
                                        let status_resp = query_rpc(
                                            "get_search_rebuild_status",
                                            serde_json::json!({}),
                                        )
                                        .await;
                                        match status_resp {
                                            Ok(status_val) => {
                                                if let Some(res) = status_val.result {
                                                    let in_progress = res
                                                        .get("in_progress")
                                                        .and_then(|v| v.as_bool())
                                                        .unwrap_or(false);
                                                    let progress = res
                                                        .get("progress")
                                                        .and_then(|v| v.as_u64())
                                                        .unwrap_or(0);
                                                    print!("\rBuilding search index: {} files indexed...", progress);
                                                    let _ = std::io::stdout().flush();
                                                    if !in_progress {
                                                        println!();
                                                        break;
                                                    }
                                                }
                                            }
                                            Err(_) => {
                                                tokio::time::sleep(
                                                    std::time::Duration::from_millis(250),
                                                )
                                                .await;
                                            }
                                        }
                                        tokio::time::sleep(std::time::Duration::from_millis(250))
                                            .await;
                                    }
                                    continue;
                                }
                            }
                            eprintln!("Error: {}", err.message);
                            std::process::exit(1);
                        }

                        if let Some(result) = resp.result {
                            if *json {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&result.get("results").unwrap())
                                        .unwrap()
                                );
                            } else {
                                let results = result
                                    .get("results")
                                    .and_then(|v| v.as_array())
                                    .cloned()
                                    .unwrap_or_default();
                                let volumes_incomplete = result
                                    .get("volumes_incomplete")
                                    .and_then(|v| v.as_array())
                                    .map(|arr| {
                                        arr.iter()
                                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                            .collect::<std::collections::HashSet<_>>()
                                    })
                                    .unwrap_or_default();

                                let mut shown_warnings = std::collections::HashSet::new();
                                for item in &results {
                                    if let Some(vol) = item.get("volume").and_then(|v| v.as_str()) {
                                        if volumes_incomplete.contains(vol)
                                            && shown_warnings.insert(vol.to_string())
                                        {
                                            println!("⚠ Volume {} is currently scanning; results may be incomplete.", vol);
                                        }
                                    }
                                }

                                if let Some(last_mod_str) =
                                    result.get("last_db_modified").and_then(|v| v.as_str())
                                {
                                    if let Ok(dt) =
                                        chrono::DateTime::parse_from_rfc3339(last_mod_str)
                                    {
                                        let local_dt = dt.with_timezone(&chrono::Local);
                                        println!(
                                            "Database Last Modified: {}",
                                            local_dt.format("%Y-%m-%d %H:%M:%S")
                                        );
                                    }
                                }

                                if result
                                    .get("index_rebuilding")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false)
                                {
                                    println!("⚠ Search index is currently rebuilding in the background; results may be stale.");
                                }

                                if results.is_empty() {
                                    println!("No results found.");
                                    return Ok(());
                                }

                                struct TreeNode {
                                    name: String,
                                    volume: String,
                                    attributes: String,
                                    size_str: String,
                                    modified_str: String,
                                    is_match: bool,
                                    children: std::collections::BTreeMap<String, TreeNode>,
                                }

                                impl TreeNode {
                                    fn new(name: &str) -> Self {
                                        Self {
                                            name: name.to_string(),
                                            volume: String::new(),
                                            attributes: String::new(),
                                            size_str: String::new(),
                                            modified_str: String::new(),
                                            is_match: false,
                                            children: std::collections::BTreeMap::new(),
                                        }
                                    }
                                }

                                fn print_tree_node(
                                    node: &TreeNode,
                                    prefix: &str,
                                    is_last: bool,
                                    is_root: bool,
                                    max_vol: usize,
                                    max_attrs: usize,
                                    max_size: usize,
                                    max_mod: usize,
                                ) {
                                    let vol_str = if is_root { &node.volume } else { "" };

                                    let display_name = if node.is_match {
                                        node.name.clone()
                                    } else {
                                        format!("{}/", node.name)
                                    };

                                    let tree_line = if is_root {
                                        format!("{}\\", node.name)
                                    } else {
                                        format!(
                                            "{}{}{}",
                                            prefix,
                                            if is_last { "└── " } else { "├── " },
                                            display_name
                                        )
                                    };

                                    // Print tabular metadata + tree path
                                    println!(
                                        "{:<width_vol$} | {:<width_attrs$} | {:>width_size$} | {:<width_mod$} | {}",
                                        vol_str, node.attributes, node.size_str, node.modified_str, tree_line,
                                        width_vol = max_vol,
                                        width_attrs = max_attrs,
                                        width_size = max_size,
                                        width_mod = max_mod
                                    );

                                    let child_count = node.children.len();
                                    let mut i = 0;
                                    for (_, child) in &node.children {
                                        i += 1;
                                        let is_last_child = i == child_count;
                                        let new_prefix = if is_root {
                                            "".to_string()
                                        } else {
                                            format!(
                                                "{}{}",
                                                prefix,
                                                if is_last { "    " } else { "│   " }
                                            )
                                        };
                                        print_tree_node(
                                            child,
                                            &new_prefix,
                                            is_last_child,
                                            false,
                                            max_vol,
                                            max_attrs,
                                            max_size,
                                            max_mod,
                                        );
                                    }
                                }

                                let mut max_vol = 6;
                                let mut max_attrs = 10;
                                let mut max_size = 8;
                                let mut max_mod = 19;

                                let mut roots: std::collections::BTreeMap<String, TreeNode> =
                                    std::collections::BTreeMap::new();

                                for item in &results {
                                    let vol =
                                        item.get("volume").and_then(|v| v.as_str()).unwrap_or("");
                                    let path =
                                        item.get("path").and_then(|v| v.as_str()).unwrap_or("");
                                    let name =
                                        item.get("name").and_then(|v| v.as_str()).unwrap_or("");

                                    let attrs_arr = item
                                        .get("attributes")
                                        .and_then(|v| v.as_array())
                                        .cloned()
                                        .unwrap_or_default();
                                    let mut attr_flags = String::new();
                                    let attr_strs: Vec<&str> =
                                        attrs_arr.iter().filter_map(|v| v.as_str()).collect();
                                    if attr_strs.contains(&"readonly") {
                                        attr_flags.push('R');
                                    }
                                    if attr_strs.contains(&"hidden") {
                                        attr_flags.push('H');
                                    }
                                    if attr_strs.contains(&"system") {
                                        attr_flags.push('S');
                                    }
                                    if attr_strs.contains(&"archive") {
                                        attr_flags.push('A');
                                    }
                                    if attr_strs.contains(&"reparse") {
                                        attr_flags.push('L');
                                    }
                                    if attr_flags.is_empty() {
                                        attr_flags = "-".to_string();
                                    }

                                    let size_val =
                                        item.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let is_dir = item
                                        .get("is_directory")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false);
                                    let size_str = if is_dir {
                                        "<DIR>".to_string()
                                    } else {
                                        format_size(size_val)
                                    };

                                    let mod_time = item
                                        .get("modified_at")
                                        .and_then(|v| v.as_i64())
                                        .unwrap_or(0);
                                    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(
                                        mod_time, 0,
                                    )
                                    .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                                    .unwrap_or_else(|| "-".to_string());

                                    max_vol = max_vol.max(vol.len());
                                    max_attrs = max_attrs.max(attr_flags.len());
                                    max_size = max_size.max(size_str.len());
                                    max_mod = max_mod.max(dt.len());

                                    let root_node =
                                        roots.entry(vol.to_string()).or_insert_with(|| {
                                            let mut r = TreeNode::new(vol);
                                            r.volume = vol.to_string();
                                            r
                                        });

                                    let mut curr = root_node;
                                    if !path.is_empty() {
                                        for segment in path.split('/') {
                                            if !segment.is_empty() {
                                                curr = curr
                                                    .children
                                                    .entry(segment.to_string())
                                                    .or_insert_with(|| TreeNode::new(segment));
                                            }
                                        }
                                    }

                                    let leaf = curr
                                        .children
                                        .entry(name.to_string())
                                        .or_insert_with(|| TreeNode::new(name));
                                    leaf.is_match = true;
                                    leaf.volume = vol.to_string();
                                    leaf.attributes = attr_flags;
                                    leaf.size_str = size_str;
                                    leaf.modified_str = dt;
                                }

                                println!(
                                    "{:<width_vol$} | {:<width_attrs$} | {:>width_size$} | {:<width_mod$} | Path / Tree",
                                    "Volume", "Attributes", "Size", "Modified",
                                    width_vol = max_vol,
                                    width_attrs = max_attrs,
                                    width_size = max_size,
                                    width_mod = max_mod
                                );
                                println!(
                                    "{:-<width_vol$}---{:-<width_attrs$}---{:-<width_size$}---{:-<width_mod$}-------",
                                    "", "", "", "",
                                    width_vol = max_vol,
                                    width_attrs = max_attrs,
                                    width_size = max_size,
                                    width_mod = max_mod
                                );

                                for (_, root_node) in &roots {
                                    print_tree_node(
                                        root_node, "", false, true, max_vol, max_attrs, max_size,
                                        max_mod,
                                    );
                                }
                            }
                        }
                        break;
                    }
                    Err(e) => {
                        eprintln!("Error querying daemon: {}", e);
                        std::process::exit(1);
                    }
                }
            }
        }
        Commands::History {
            path,
            since,
            until,
            kind,
            collapse,
            limit,
            cursor,
            json,
            verbose,
        } => {
            let resolved_path = match path {
                Some(p) => {
                    let p_buf = std::path::Path::new(p);
                    if p_buf.is_absolute() {
                        p.clone()
                    } else {
                        match std::env::current_dir() {
                            Ok(cwd) => cwd.join(p_buf).to_string_lossy().to_string(),
                            Err(_) => p.clone(),
                        }
                    }
                }
                None => match std::env::current_dir() {
                    Ok(pb) => pb.to_string_lossy().to_string(),
                    Err(e) => {
                        eprintln!("Error getting current directory: {}", e);
                        std::process::exit(1);
                    }
                },
            };

            let since_ts = if let Some(ref s) = since {
                match parse_modified_time(s) {
                    Ok(dt) => Some(dt.timestamp()),
                    Err(e) => {
                        eprintln!("Error parsing since: {}", e);
                        std::process::exit(1);
                    }
                }
            } else {
                None
            };

            let until_ts = if let Some(ref s) = until {
                match parse_modified_time(s) {
                    Ok(dt) => Some(dt.timestamp()),
                    Err(e) => {
                        eprintln!("Error parsing until: {}", e);
                        std::process::exit(1);
                    }
                }
            } else {
                None
            };

            let params = serde_json::json!({
                "path": resolved_path,
                "since": since_ts,
                "until": until_ts,
                "kind": kind,
                "collapse": collapse,
                "limit": limit,
                "cursor": cursor,
            });

            match query_rpc("get_history", params).await {
                Ok(resp) => {
                    if let Some(err) = resp.error {
                        let code_owned = err.data.as_ref()
                            .and_then(|d| d.get("code"))
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "E_INVALID_PARAMS".to_string());
                        print_error(*json, &code_owned, &err.message, err.data);
                        std::process::exit(1);
                    }

                    if let Some(result) = resp.result {
                        let results = result
                            .get("results")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default();
                        let truncated = result
                            .get("truncated")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let next_cursor = result
                            .get("next_cursor")
                            .and_then(|v| v.as_str());
                        let prev_cursor = result
                            .get("prev_cursor")
                            .and_then(|v| v.as_str());

                        if *json {
                            println!("{}", serde_json::to_string_pretty(&results).unwrap());
                        } else {
                            if truncated {
                                println!("⚠ Note: History is truncated. Some older entries may have been pruned according to the retention policy.");
                            }

                            if results.is_empty() {
                                println!("No history found for: {}", resolved_path);
                                if let Some(debug) = result.get("debug_info").and_then(|v| v.as_str()) {
                                    println!("Debug Info: {}", debug);
                                }
                                return Ok(());
                            }

                            // Dynamic formatting
                            let mut max_vol = 6;
                            let mut max_kind = 8;
                            let mut max_size = 10;
                            let mut max_date = 15;
                            let mut max_name = 15;

                            let mut local_relative_times = Vec::new();
                            let mut local_absolute_times = Vec::new();
                            let mut size_deltas = Vec::new();

                            for item in &results {
                                let vol = item.get("volume").and_then(|v| v.as_str()).unwrap_or("");
                                let kind_str = item.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                                let size_val = item.get("size_delta").and_then(|v| v.as_i64()).unwrap_or(0);
                                let at_str = item.get("at").and_then(|v| v.as_str()).unwrap_or("");
                                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");

                                let r_time = format_relative_time(at_str);
                                let a_time = format_absolute_time(at_str);
                                let size_str = format_size_delta(size_val);

                                max_vol = max_vol.max(vol.len());
                                max_kind = max_kind.max(kind_str.len());
                                max_size = max_size.max(size_str.len());
                                max_date = max_date.max(if *verbose { a_time.len() } else { r_time.len() });
                                max_name = max_name.max(name.len());

                                local_relative_times.push(r_time);
                                local_absolute_times.push(a_time);
                                size_deltas.push(size_str);
                            }

                            if *verbose {
                                let mut max_seq = 3;
                                for item in &results {
                                    let seq = item.get("sequence").and_then(|v| v.as_i64()).unwrap_or(0).to_string();
                                    max_seq = max_seq.max(seq.len());
                                }
                                println!(
                                    "{:<width_seq$} | {:<width_date$} | {:<width_kind$} | {:>width_size$} | {:<width_name$} | Details",
                                    "Seq", "Date", "Kind", "Size Delta", "Name",
                                    width_seq = max_seq,
                                    width_date = max_date,
                                    width_kind = max_kind,
                                    width_size = max_size,
                                    width_name = max_name
                                );
                                println!(
                                    "{}",
                                    "-".repeat(max_seq + max_date + max_kind + max_size + max_name + 18)
                                );
                                let mut prev_names = std::collections::HashMap::new();
                                for (i, item) in results.iter().enumerate() {
                                    let seq = item.get("sequence").and_then(|v| v.as_i64()).unwrap_or(0);
                                    let date_str = &local_absolute_times[i];
                                    let kind_str = item.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                                    let size_str = &size_deltas[i];
                                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                    let file_id = item.get("file_id").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let parent_id = item.get("parent_file_id").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let source = item.get("source").and_then(|v| v.as_str()).unwrap_or("");

                                    let mut details_parts = Vec::new();
                                    if kind_str.eq_ignore_ascii_case("renamed") {
                                        if let Some(old_name) = prev_names.get(&file_id) {
                                            details_parts.push(format!("Renamed from {}", old_name));
                                        } else {
                                            details_parts.push("Renamed".to_string());
                                        }
                                    }
                                    details_parts.push(format!("Parent: {}", parent_id));
                                    details_parts.push(format!("Source: {}", source));

                                    println!(
                                        "{:<width_seq$} | {:<width_date$} | {:<width_kind$} | {:>width_size$} | {:<width_name$} | {}",
                                        seq, date_str, kind_str, size_str, name, details_parts.join(", "),
                                        width_seq = max_seq,
                                        width_date = max_date,
                                        width_kind = max_kind,
                                        width_size = max_size,
                                        width_name = max_name
                                    );
                                    prev_names.insert(file_id, name.to_string());
                                }
                            } else {
                                println!(
                                    "{:<width_vol$} | {:<width_kind$} | {:>width_size$} | {:<width_date$} | {:<width_name$} | Details",
                                    "Volume", "Kind", "Size Delta", "Date", "Name",
                                    width_vol = max_vol,
                                    width_kind = max_kind,
                                    width_size = max_size,
                                    width_date = max_date,
                                    width_name = max_name
                                );
                                println!(
                                    "{}",
                                    "-".repeat(max_vol + max_kind + max_size + max_date + max_name + 18)
                                );
                                let mut prev_names = std::collections::HashMap::new();
                                for (i, item) in results.iter().enumerate() {
                                    let vol = item.get("volume").and_then(|v| v.as_str()).unwrap_or("");
                                    let kind_str = item.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                                    let size_str = &size_deltas[i];
                                    let date_str = &local_relative_times[i];
                                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                    let file_id = item.get("file_id").and_then(|v| v.as_u64()).unwrap_or(0);

                                    let mut details = String::new();
                                    if kind_str.eq_ignore_ascii_case("renamed") {
                                        if let Some(old_name) = prev_names.get(&file_id) {
                                            details = format!("Renamed from {}", old_name);
                                        } else {
                                            details = "Renamed".to_string();
                                        }
                                    }

                                    println!(
                                        "{:<width_vol$} | {:<width_kind$} | {:>width_size$} | {:<width_date$} | {:<width_name$} | {}",
                                        vol, kind_str, size_str, date_str, name, details,
                                        width_vol = max_vol,
                                        width_kind = max_kind,
                                        width_size = max_size,
                                        width_date = max_date,
                                        width_name = max_name
                                    );
                                    prev_names.insert(file_id, name.to_string());
                                }
                            }

                            if prev_cursor.is_some() || next_cursor.is_some() {
                                println!();
                                if let Some(prev) = prev_cursor {
                                    print!("Previous page cursor: {}    ", prev);
                                }
                                if let Some(next) = next_cursor {
                                    print!("Next page cursor: {}", next);
                                }
                                println!();
                            }

                            if *verbose {
                                if let Some(debug) = result.get("debug_info").and_then(|v| v.as_str()) {
                                    println!("\nDebug Info: {}", debug);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    let err_msg = format!("Failed to query history: {}", e);
                    print_error(*json, "E_INVALID_PARAMS", &err_msg, None);
                    std::process::exit(1);
                }
            }
        }
        Commands::Snapshot { subcommand } => match subcommand {
            SnapshotCommands::Create { label, volume } => {
                let vol_to_use = match volume {
                    Some(v) => {
                        let mut uppercase = v.to_uppercase();
                        if !uppercase.ends_with(':') {
                            uppercase.push(':');
                        }
                        uppercase
                    }
                    None => {
                        match std::env::current_dir() {
                            Ok(cwd) => {
                                let path_str = cwd.to_string_lossy().to_string();
                                if path_str.len() >= 2 && path_str.as_bytes()[1] == b':' {
                                    path_str[0..2].to_uppercase()
                                } else {
                                    "C:".to_string()
                                }
                            }
                            Err(_) => "C:".to_string()
                        }
                    }
                };

                let params = serde_json::json!({
                    "volume": vol_to_use,
                    "label": label,
                });

                match query_rpc("snapshot_create", params).await {
                    Ok(resp) => {
                        if let Some(err) = resp.error {
                            print_cli_rpc_error(cli.json, &err);
                            std::process::exit(1);
                        }
                        if let Some(result) = resp.result {
                            let job_id = result.get("job_id").and_then(|j| j.as_str()).unwrap_or("");
                            if !cli.json {
                                println!("Creating snapshot for volume {} (job: {})...", vol_to_use, job_id);
                            }
                            
                            // Poll job completion
                            loop {
                                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                                let poll_params = serde_json::json!({ "job_id": job_id });
                                match query_rpc("job.completed", poll_params).await {
                                    Ok(poll_resp) => {
                                        if let Some(poll_err) = poll_resp.error {
                                            print_cli_rpc_error(cli.json, &poll_err);
                                            std::process::exit(1);
                                        }
                                        if let Some(job_val) = poll_resp.result {
                                            let completed = job_val.get("completed").and_then(|c| c.as_bool()).unwrap_or(false);
                                            if completed {
                                                if let Some(job_err) = job_val.get("error").and_then(|e| e.as_str()) {
                                                    eprintln!("Error: {}", job_err);
                                                    std::process::exit(1);
                                                }
                                                if cli.json {
                                                    println!("{}", serde_json::to_string_pretty(&job_val).unwrap());
                                                } else {
                                                    if let Some(res_details) = job_val.get("result") {
                                                        let snap_id = res_details.get("id").and_then(|s| s.as_str()).unwrap_or("");
                                                        let final_label = res_details.get("label").and_then(|l| l.as_str()).unwrap_or("");
                                                        let seq = res_details.get("sequence_number").and_then(|s| s.as_i64()).unwrap_or(0);
                                                        println!("Snapshot \"{}\" created successfully (id: {}, sequence: {}).", final_label, snap_id, seq);
                                                    }
                                                }
                                                break;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("Error polling job: {}", e);
                                        std::process::exit(1);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let err_msg = format!("Failed to request snapshot creation: {}", e);
                        print_error(cli.json, "E_INVALID_PARAMS", &err_msg, None);
                        std::process::exit(1);
                    }
                }
            }
            SnapshotCommands::List { volume, limit, cursor } => {
                let params = serde_json::json!({
                    "volume": volume,
                    "limit": limit,
                    "cursor": cursor,
                });

                match query_rpc("snapshot_list", params).await {
                    Ok(resp) => {
                        if let Some(err) = resp.error {
                            print_cli_rpc_error(cli.json, &err);
                            std::process::exit(1);
                        }
                        if let Some(result) = resp.result {
                            if cli.json {
                                println!("{}", serde_json::to_string_pretty(&result.get("results").unwrap()).unwrap());
                            } else {
                                let results = result
                                    .get("results")
                                    .and_then(|v| v.as_array())
                                    .cloned()
                                    .unwrap_or_default();

                                if results.is_empty() {
                                    println!("No snapshots found.");
                                    return Ok(());
                                }

                                let next_cursor = result.get("next_cursor").and_then(|c| c.as_str());
                                let prev_cursor = result.get("prev_cursor").and_then(|c| c.as_str());

                                let mut max_label = 5;
                                let mut max_created = 19;
                                let mut max_vol = 6;
                                let mut max_age = 3;
                                let mut max_id = 11;
                                let mut max_seq = 8;
                                let mut max_daemon = 7;
                                let mut max_schema = 6;
                                let mut max_retention = 9;
                                let mut max_facts = 11;

                                let mut relative_ages = Vec::new();
                                let mut absolute_times = Vec::new();

                                for item in &results {
                                    let label = item.get("label").and_then(|l| l.as_str()).unwrap_or("");
                                    let created = item.get("created_at").and_then(|c| c.as_str()).unwrap_or("");
                                    let vol = item.get("volume").and_then(|v| v.as_str()).unwrap_or("");
                                    let id = item.get("id").and_then(|i| i.as_str()).unwrap_or("");
                                    let seq = item.get("sequence_number").and_then(|s| s.as_i64()).unwrap_or(0).to_string();
                                    let daemon = item.get("daemon_version").and_then(|d| d.as_str()).unwrap_or("");
                                    let schema = item.get("schema_version").and_then(|s| s.as_i64()).unwrap_or(0).to_string();
                                    let retention = item.get("retention_setting").and_then(|r| r.as_str()).unwrap_or("");
                                    let facts = item.get("facts_count").and_then(|f| f.as_i64()).unwrap_or(0).to_string();

                                    let rel_age = format_relative_time(created);
                                    let abs_time = format_absolute_time(created);

                                    relative_ages.push(rel_age.clone());
                                    absolute_times.push(abs_time.clone());

                                    max_label = max_label.max(label.len());
                                    max_created = max_created.max(abs_time.len());
                                    max_vol = max_vol.max(vol.len());
                                    max_age = max_age.max(rel_age.len());
                                    max_id = max_id.max(id.len());
                                    max_seq = max_seq.max(seq.len());
                                    max_daemon = max_daemon.max(daemon.len());
                                    max_schema = max_schema.max(schema.len());
                                    max_retention = max_retention.max(retention.len());
                                    max_facts = max_facts.max(facts.len());
                                }

                                if cli.verbose {
                                    println!(
                                        "{:<width_id$} | {:<width_label$} | {:<width_created$} | {:<width_vol$} | {:<width_age$} | {:<width_seq$} | {:<width_daemon$} | {:<width_schema$} | {:<width_retention$} | {:<width_facts$}",
                                        "Snapshot ID", "Label", "Created", "Volume", "Age", "Seq Ref", "Daemon", "Schema", "Retention", "Facts Count",
                                        width_id = max_id,
                                        width_label = max_label,
                                        width_created = max_created,
                                        width_vol = max_vol,
                                        width_age = max_age,
                                        width_seq = max_seq,
                                        width_daemon = max_daemon,
                                        width_schema = max_schema,
                                        width_retention = max_retention,
                                        width_facts = max_facts
                                    );
                                    println!(
                                        "{}",
                                        "-".repeat(max_id + max_label + max_created + max_vol + max_age + max_seq + max_daemon + max_schema + max_retention + max_facts + 27)
                                    );
                                    for (i, item) in results.iter().enumerate() {
                                        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                        let label = item.get("label").and_then(|v| v.as_str()).unwrap_or("");
                                        let abs_time = &absolute_times[i];
                                        let vol = item.get("volume").and_then(|v| v.as_str()).unwrap_or("");
                                        let rel_age = &relative_ages[i];
                                        let seq = item.get("sequence_number").and_then(|s| s.as_i64()).unwrap_or(0);
                                        let daemon = item.get("daemon_version").and_then(|v| v.as_str()).unwrap_or("");
                                        let schema = item.get("schema_version").and_then(|s| s.as_i64()).unwrap_or(0);
                                        let retention = item.get("retention_setting").and_then(|v| v.as_str()).unwrap_or("");
                                        let facts = item.get("facts_count").and_then(|f| f.as_i64()).unwrap_or(0);

                                        println!(
                                            "{:<width_id$} | {:<width_label$} | {:<width_created$} | {:<width_vol$} | {:<width_age$} | {:<width_seq$} | {:<width_daemon$} | {:<width_schema$} | {:<width_retention$} | {:<width_facts$}",
                                            id, label, abs_time, vol, rel_age, seq, daemon, schema, retention, facts,
                                            width_id = max_id,
                                            width_label = max_label,
                                            width_created = max_created,
                                            width_vol = max_vol,
                                            width_age = max_age,
                                            width_seq = max_seq,
                                            width_daemon = max_daemon,
                                            width_schema = max_schema,
                                            width_retention = max_retention,
                                            width_facts = max_facts
                                        );
                                    }
                                } else {
                                    println!(
                                        "{:<width_label$} | {:<width_created$} | {:<width_vol$} | {:<width_age$}",
                                        "Label", "Created", "Volume", "Age",
                                        width_label = max_label,
                                        width_created = max_created,
                                        width_vol = max_vol,
                                        width_age = max_age
                                    );
                                    println!(
                                        "{}",
                                        "-".repeat(max_label + max_created + max_vol + max_age + 9)
                                    );
                                    for (i, item) in results.iter().enumerate() {
                                        let label = item.get("label").and_then(|v| v.as_str()).unwrap_or("");
                                        let abs_time = &absolute_times[i];
                                        let vol = item.get("volume").and_then(|v| v.as_str()).unwrap_or("");
                                        let rel_age = &relative_ages[i];

                                        println!(
                                            "{:<width_label$} | {:<width_created$} | {:<width_vol$} | {:<width_age$}",
                                            label, abs_time, vol, rel_age,
                                            width_label = max_label,
                                            width_created = max_created,
                                            width_vol = max_vol,
                                            width_age = max_age
                                        );
                                    }
                                }

                                if prev_cursor.is_some() || next_cursor.is_some() {
                                    println!();
                                    if let Some(prev) = prev_cursor {
                                        print!("Previous page cursor: {}    ", prev);
                                    }
                                    if let Some(next) = next_cursor {
                                        print!("Next page cursor: {}", next);
                                    }
                                    println!();
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let err_msg = format!("Failed to query snapshots list: {}", e);
                        print_error(cli.json, "E_INVALID_PARAMS", &err_msg, None);
                        std::process::exit(1);
                    }
                }
            }
            SnapshotCommands::Diff { snapshot_a, snapshot_b, path, limit } => {
                let params = serde_json::json!({
                    "snapshot_a": snapshot_a,
                    "snapshot_b": snapshot_b,
                    "path_filter": path,
                    "limit": limit,
                });

                match query_rpc("snapshot_diff", params).await {
                    Ok(resp) => {
                        if let Some(err) = resp.error {
                            print_cli_rpc_error(cli.json, &err);
                            std::process::exit(1);
                        }
                        if let Some(result) = resp.result {
                            if cli.json {
                                println!("{}", serde_json::to_string_pretty(&result.get("results").unwrap()).unwrap());
                            } else {
                                let results = result
                                    .get("results")
                                    .and_then(|v| v.as_array())
                                    .cloned()
                                    .unwrap_or_default();

                                if results.is_empty() {
                                    println!("No changes found between snapshots.");
                                    return Ok(());
                                }

                                let mut max_kind = 8;
                                let mut max_size = 10;
                                let mut max_path = 4;
                                let mut max_fid = 7;
                                let mut max_pid = 9;

                                let mut size_deltas = Vec::new();
                                for item in &results {
                                    let kind = item.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                                    let size_val = item.get("size_delta").and_then(|s| s.as_i64()).unwrap_or(0);
                                    let path_str = item.get("path").and_then(|p| p.as_str()).unwrap_or("");
                                    let fid = item.get("file_id").and_then(|f| f.as_u64()).unwrap_or(0).to_string();
                                    let pid = item.get("parent_file_id").and_then(|p| p.as_u64()).unwrap_or(0).to_string();

                                    let size_str = format_size_delta(size_val);
                                    size_deltas.push(size_str.clone());

                                    max_kind = max_kind.max(kind.len());
                                    max_size = max_size.max(size_str.len());
                                    
                                    // Handle renaming path presentation length
                                    let mut display_path = path_str.to_string();
                                    if kind.eq_ignore_ascii_case("renamed") {
                                        if let Some(old) = item.get("old_name").and_then(|o| o.as_str()) {
                                            display_path = format!("{} (Renamed from {})", path_str, old);
                                        }
                                    }
                                    max_path = max_path.max(display_path.len());
                                    max_fid = max_fid.max(fid.len());
                                    max_pid = max_pid.max(pid.len());
                                }

                                if cli.verbose {
                                    println!(
                                        "{:<width_fid$} | {:<width_pid$} | {:<width_kind$} | {:>width_size$} | Path / Details",
                                        "File ID", "Parent ID", "Kind", "Size Delta",
                                        width_fid = max_fid,
                                        width_pid = max_pid,
                                        width_kind = max_kind,
                                        width_size = max_size
                                    );
                                    println!(
                                        "{}",
                                        "-".repeat(max_fid + max_pid + max_kind + max_size + max_path + 15)
                                    );
                                    for (i, item) in results.iter().enumerate() {
                                        let fid = item.get("file_id").and_then(|f| f.as_u64()).unwrap_or(0);
                                        let pid = item.get("parent_file_id").and_then(|p| p.as_u64()).unwrap_or(0);
                                        let kind = item.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                                        let size_str = &size_deltas[i];
                                        let path_str = item.get("path").and_then(|p| p.as_str()).unwrap_or("");
                                        let mut display_path = path_str.to_string();
                                        if kind.eq_ignore_ascii_case("renamed") {
                                            if let Some(old) = item.get("old_name").and_then(|o| o.as_str()) {
                                                display_path = format!("{} (Renamed from {})", path_str, old);
                                            }
                                        }

                                        println!(
                                            "{:<width_fid$} | {:<width_pid$} | {:<width_kind$} | {:>width_size$} | {}",
                                            fid, pid, kind, size_str, display_path,
                                            width_fid = max_fid,
                                            width_pid = max_pid,
                                            width_kind = max_kind,
                                            width_size = max_size
                                        );
                                    }
                                } else {
                                    // Tree node structure for colorized diff tree
                                    struct DiffTreeNode {
                                        name: String,
                                        kind: String,
                                        size_delta: i64,
                                        old_name: Option<String>,
                                        is_directory: bool,
                                        children: std::collections::BTreeMap<String, DiffTreeNode>,
                                    }

                                    impl DiffTreeNode {
                                        fn new(name: &str) -> Self {
                                            Self {
                                                name: name.to_string(),
                                                kind: String::new(),
                                                size_delta: 0,
                                                old_name: None,
                                                is_directory: false,
                                                children: std::collections::BTreeMap::new(),
                                            }
                                        }
                                    }

                                    fn print_diff_tree_node(
                                        node: &DiffTreeNode,
                                        prefix: &str,
                                        is_last: bool,
                                        is_root: bool,
                                    ) {
                                        let mut tree_line = String::new();

                                        if is_root {
                                            tree_line.push_str(&format!("{}\\", node.name));
                                        } else {
                                            tree_line.push_str(prefix);
                                            if is_last {
                                                tree_line.push_str("└── ");
                                            } else {
                                                tree_line.push_str("├── ");
                                            }

                                            let formatted_delta = format_size_delta(node.size_delta);
                                            match node.kind.as_str() {
                                                "Created" => {
                                                    tree_line.push_str(&format!(
                                                        "\x1b[32m+ {} ({})\x1b[0m",
                                                        node.name, formatted_delta
                                                    ));
                                                }
                                                "Deleted" => {
                                                    tree_line.push_str(&format!(
                                                        "\x1b[31m- {} ({})\x1b[0m",
                                                        node.name, formatted_delta
                                                    ));
                                                }
                                                "Renamed" => {
                                                    let old = node.old_name.as_deref().unwrap_or("unknown");
                                                    tree_line.push_str(&format!(
                                                        "\x1b[33m~ {} (Renamed from {}, {})\x1b[0m",
                                                        node.name, old, formatted_delta
                                                    ));
                                                }
                                                "Modified" => {
                                                    tree_line.push_str(&format!(
                                                        "\x1b[36m~ {} ({})\x1b[0m",
                                                        node.name, formatted_delta
                                                    ));
                                                }
                                                _ => {
                                                    let display_name = if node.is_directory {
                                                        format!("{}/", node.name)
                                                    } else {
                                                        node.name.clone()
                                                    };
                                                    tree_line.push_str(&display_name);
                                                }
                                            }
                                        }

                                        println!("{}", tree_line);

                                        let child_count = node.children.len();
                                        let mut i = 0;
                                        for (_, child) in &node.children {
                                            i += 1;
                                            let is_last_child = i == child_count;
                                            let new_prefix = if is_root {
                                                "".to_string()
                                            } else {
                                                format!(
                                                    "{}{}",
                                                    prefix,
                                                    if is_last { "    " } else { "│   " }
                                                )
                                            };
                                            print_diff_tree_node(child, &new_prefix, is_last_child, false);
                                        }
                                    }

                                    let mut roots: std::collections::BTreeMap<String, DiffTreeNode> =
                                        std::collections::BTreeMap::new();

                                    for item in &results {
                                        let kind = item.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                                        let size_val = item.get("size_delta").and_then(|s| s.as_i64()).unwrap_or(0);
                                        let path_str = item.get("path").and_then(|p| p.as_str()).unwrap_or("");
                                        let is_dir = item.get("is_directory").and_then(|v| v.as_bool()).unwrap_or(false);
                                        let old_name = item.get("old_name").and_then(|o| o.as_str().map(|s| s.to_string()));

                                        let segments: Vec<&str> = path_str.split('/').filter(|s| !s.is_empty()).collect();
                                        if !segments.is_empty() {
                                            let vol = segments[0];
                                            let mut curr = roots.entry(vol.to_string()).or_insert_with(|| {
                                                let mut r = DiffTreeNode::new(vol);
                                                r.is_directory = true;
                                                r
                                            });

                                            for i in 1..segments.len() {
                                                let segment = segments[i];
                                                let is_leaf = i == segments.len() - 1;
                                                curr = curr
                                                    .children
                                                    .entry(segment.to_string())
                                                    .or_insert_with(|| {
                                                        let mut n = DiffTreeNode::new(segment);
                                                        if !is_leaf {
                                                            n.is_directory = true;
                                                        }
                                                        n
                                                    });
                                            }

                                            // Apply leaf properties
                                            curr.kind = kind.to_string();
                                            curr.size_delta = size_val;
                                            curr.old_name = old_name;
                                            curr.is_directory = is_dir;
                                        }
                                    }

                                    for (_, root_node) in &roots {
                                        print_diff_tree_node(root_node, "", false, true);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let err_msg = format!("Failed to request snapshot diff: {}", e);
                        print_error(cli.json, "E_INVALID_PARAMS", &err_msg, None);
                        std::process::exit(1);
                    }
                }
            }
        }
    }

    Ok(())
}

fn print_cli_rpc_error(json_mode: bool, err: &JsonRpcError) {
    let mut code_str = "E_UNKNOWN";
    if let Some(ref data) = err.data {
        if let Some(c) = data.get("code").and_then(|v| v.as_str()) {
            code_str = c;
        }
    }
    print_error(json_mode, code_str, &err.message, err.data.clone());
}

async fn query_rpc(
    method: &str,
    params: serde_json::Value,
) -> Result<JsonRpcResponse, Box<dyn std::error::Error>> {
    let mut stream = platform_windows::connect_client(PIPE_PATH).await?;

    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: method.to_string(),
        params,
        id: Some(serde_json::Value::Number(1.into())),
    };

    let mut req_bytes = serde_json::to_vec(&request)?;
    req_bytes.push(b'\n');
    stream.write_all(&req_bytes).await?;
    stream.flush().await?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;

    let response: JsonRpcResponse = serde_json::from_str(&line)?;
    Ok(response)
}

async fn query_status() -> Result<JsonRpcResponse, Box<dyn std::error::Error>> {
    query_rpc("status", serde_json::Value::Null).await
}

async fn run_doctor() {
    println!("DiskTracker Diagnostics:");
    let mut all_ok = true;

    // 1. Check Process Elevation / Administrator privileges
    let is_elevated = platform_windows::is_elevated();
    if is_elevated {
        println!("  [OK] Process elevation (Running with administrator/root privileges)");
    } else {
        println!("  [FAIL] Process elevation: Please run DiskTracker from an Administrator/Root terminal.");
        all_ok = false;
    }

    // 2. Check AppData folder write permissions
    let appdata_dir_res = storage::get_db_dir();
    match appdata_dir_res {
        Ok(dir) => {
            let temp_file = dir.join(".doctor_temp");
            match std::fs::write(&temp_file, b"test") {
                Ok(_) => {
                    let _ = std::fs::remove_file(temp_file);
                    println!("  [OK] AppData folder write permissions");
                }
                Err(e) => {
                    println!("  [FAIL] AppData folder write permissions: Failed to write to AppData folder at {:?}: {:?}", dir, e);
                    all_ok = false;
                }
            }
        }
        Err(e) => {
            println!("  [FAIL] AppData folder write permissions: Failed to resolve AppData directory: {:?}", e);
            all_ok = false;
        }
    }

    // 3. Check IPC Socket/Named Pipe path connectivity (Daemon reachable?)
    match platform_windows::connect_client(PIPE_PATH).await {
        Ok(_) => {
            println!("  [OK] IPC Named Pipe path reachable (Daemon is running)");
        }
        Err(e) => {
            println!("  [FAIL] IPC Named Pipe path reachable: Could not connect to daemon named pipe. Is the daemon running? (Error: {:?})", e);
            all_ok = false;
        }
    }

    // 4. Check SQLite database integrity
    match query_rpc("check_db_integrity", serde_json::Value::Null).await {
        Ok(resp) => {
            if let Some(result) = resp.result {
                if let Some(status) = result.get("status").and_then(|s| s.as_str()) {
                    if status == "ok" {
                        println!("  [OK] SQLite database integrity");
                    } else {
                        println!(
                            "  [FAIL] SQLite database integrity check returned: {}",
                            status
                        );
                        all_ok = false;
                    }
                } else {
                    println!("  [FAIL] SQLite database integrity: Invalid response from daemon");
                    all_ok = false;
                }
            } else if let Some(err) = resp.error {
                println!(
                    "  [FAIL] SQLite database integrity: Daemon error: {}",
                    err.message
                );
                all_ok = false;
            }
        }
        Err(e) => {
            println!("  [FAIL] SQLite database integrity: Failed to connect to SQLite database via daemon: {:?}", e);
            all_ok = false;
        }
    }

    // 5. Check USN Journal availability
    #[cfg(windows)]
    {
        let volumes = platform_windows::detect_ntfs_volumes();
        if volumes.is_empty() {
            println!("  [FAIL] USN Journal availability: No logical volumes detected.");
            all_ok = false;
        } else {
            let mut usn_ok = true;
            for vol in &volumes {
                if let Err(e) = platform_windows::check_volume_usn(vol) {
                    println!("  [FAIL] USN Journal availability: USN Journal for drive {} is not available or readable. Error: {:?}", vol, e);
                    usn_ok = false;
                    all_ok = false;
                }
            }
            if usn_ok {
                println!(
                    "  [OK] USN Journal availability (Verified {} volumes: {:?})",
                    volumes.len(),
                    volumes
                );
            }
        }
    }
    #[cfg(not(windows))]
    {
        println!("  [OK] USN Journal availability (Mocked fallback on non-Windows target)");
    }

    // 6. Check Pruning Log Status
    println!("  Checking Pruning Logs...");
    match query_rpc("get_pruning_logs", serde_json::Value::Null).await {
        Ok(resp) => {
            if let Some(result) = resp.result {
                if let Some(arr) = result.as_array() {
                    if arr.is_empty() {
                        println!("  [OK] Pruning Log check (No pruning runs logged yet)");
                    } else {
                        println!("  [OK] Pruning Log check. Recent runs:");
                        for item in arr.iter().take(3) {
                            let volume = item.get("volume").and_then(|v| v.as_str()).unwrap_or("?");
                            let status = item.get("status").and_then(|s| s.as_str()).unwrap_or("?");
                            let run_at = item.get("run_at").and_then(|r| r.as_str()).unwrap_or("?");
                            let details =
                                item.get("details").and_then(|d| d.as_str()).unwrap_or("?");
                            println!(
                                "      - Volume {}: [{}] at {} ({})",
                                volume, status, run_at, details
                            );
                        }
                    }
                } else {
                    println!("  [FAIL] Pruning Log check: Invalid response from daemon");
                    all_ok = false;
                }
            } else if let Some(err) = resp.error {
                println!("  [FAIL] Pruning Log check: Daemon error: {}", err.message);
                all_ok = false;
            }
        }
        Err(e) => {
            println!(
                "  [FAIL] Pruning Log check: Failed to retrieve pruning logs via daemon: {:?}",
                e
            );
            all_ok = false;
        }
    }

    if all_ok {
        println!("\nDiagnostics complete. System status: OK");
    } else {
        println!("\nDiagnostics complete. System status: FAILED (Some checks did not pass)");
        std::process::exit(1);
    }
}

async fn run_uninstall() -> Result<(), Box<dyn std::error::Error>> {
    print!("Are you sure you want to stop the daemon and delete all database files? [y/N]: ");
    use std::io::Write;
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();
    if input != "y" && input != "yes" {
        println!("[Cli] Uninstall aborted.");
        return Ok(());
    }

    println!("[Cli] Starting uninstallation...");

    // 1. Check if daemon is running and stop it
    let status_res = query_status().await;
    match status_res {
        Ok(resp) => {
            if let Some(result) = resp.result {
                if let Ok(snap) = serde_json::from_value::<ProgressSnapshot>(result) {
                    println!("[Cli] Stopping running daemon (PID {})...", snap.daemon_pid);
                    match platform_windows::kill_process_by_pid(snap.daemon_pid) {
                        Ok(_) => {
                            println!("  [OK] Daemon stopped successfully.");
                            // Give the OS some time to release file locks
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        }
                        Err(e) => {
                            eprintln!("  [WARNING] Failed to terminate daemon process: {:?}", e);
                        }
                    }
                }
            }
        }
        Err(_) => {
            println!("  [OK] Daemon is not running.");
        }
    }

    // 2. Stop and delete the Windows Service if registered
    #[cfg(windows)]
    {
        println!("[Cli] Stopping and unregistering DiskTracker Windows Service...");
        let _ = platform_windows::stop_service();
        match platform_windows::unregister_service() {
            Ok(_) => println!("  [OK] Windows Service stopped and unregistered successfully."),
            Err(e) => {
                println!(
                    "  [INFO] No active Windows Service to unregister (or: {:?})",
                    e
                );
            }
        }
    }

    // 3. Delete SQLite database files and AppData folder
    let db_dir = storage::get_db_dir()?;
    if db_dir.exists() {
        println!(
            "[Cli] Deleting database files and directory at {:?}...",
            db_dir
        );
        let mut retries = 5;
        loop {
            match std::fs::remove_dir_all(&db_dir) {
                Ok(_) => {
                    println!("  [OK] Database files and directory deleted.");
                    break;
                }
                Err(e) => {
                    if retries > 0 {
                        println!("  [INFO] Directory locked, retrying deletion in 200ms...");
                        retries -= 1;
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    } else {
                        return Err(Box::new(e));
                    }
                }
            }
        }
    } else {
        println!("  [OK] AppData directory does not exist.");
    }

    // 4. Clean up Unix Fallback Named Pipe
    #[cfg(not(windows))]
    {
        let socket_path = "/tmp/disktracker.sock";
        if std::path::Path::new(socket_path).exists() {
            println!(
                "[Cli] (Unix Fallback) Deleting socket file at {}...",
                socket_path
            );
            let _ = std::fs::remove_file(socket_path);
            println!("  [OK] Socket file deleted.");
        }

        // Clean up mock mount directories if they exist
        let volumes = vec!["C", "D", "E"];
        for vol in volumes {
            let mock_path = format!("/tmp/disktracker_mock_{}", vol);
            if std::path::Path::new(&mock_path).exists() {
                println!(
                    "[Cli] (Unix Fallback) Deleting mock folder at {}...",
                    mock_path
                );
                let _ = std::fs::remove_dir_all(&mock_path);
            }
        }
    }

    println!("[Cli] Uninstall complete!");
    Ok(())
}

fn print_error(json: bool, code: &str, message: &str, details: Option<serde_json::Value>) {
    if json {
        let err_json = serde_json::json!({
            "code": code,
            "message": message,
            "details": details.unwrap_or(serde_json::Value::Null)
        });
        println!("{}", serde_json::to_string_pretty(&err_json).unwrap());
    } else {
        eprintln!("Error: {}", message);
    }
}

fn parse_modified_time(s: &str) -> Result<chrono::DateTime<chrono::Utc>, String> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&chrono::Utc));
    }

    let s = s.trim().to_lowercase();
    let mut num_str = String::new();
    let mut unit_str = String::new();
    for c in s.chars() {
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

    Ok(now - duration)
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    if bytes >= TB {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

fn format_relative_time(at_rfc3339: &str) -> String {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(at_rfc3339) {
        let now = chrono::Utc::now();
        let diff = now.signed_duration_since(dt.with_timezone(&chrono::Utc));
        if diff.num_seconds() < 0 {
            "just now".to_string()
        } else if diff.num_seconds() < 60 {
            "just now".to_string()
        } else if diff.num_minutes() < 60 {
            format!("{}m ago", diff.num_minutes())
        } else if diff.num_hours() < 24 {
            format!("{}h ago", diff.num_hours())
        } else {
            format!("{}d ago", diff.num_days())
        }
    } else {
        at_rfc3339.to_string()
    }
}

fn format_absolute_time(at_rfc3339: &str) -> String {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(at_rfc3339) {
        let local_dt = dt.with_timezone(&chrono::Local);
        local_dt.format("%Y-%m-%d %H:%M:%S").to_string()
    } else {
        at_rfc3339.to_string()
    }
}

fn format_size_delta(bytes: i64) -> String {
    if bytes > 0 {
        format!("+{}", format_size(bytes as u64))
    } else if bytes < 0 {
        format!("-{}", format_size((-bytes) as u64))
    } else {
        "0 B".to_string()
    }
}
