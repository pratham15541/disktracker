use api::{JsonRpcRequest, JsonRpcResponse};
use clap::{Parser, Subcommand};
use core_types::{DaemonState, ProgressSnapshot, VolumeProgress};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const PIPE_PATH: &str = r"\\.\pipe\disktracker";

#[derive(Parser)]
#[command(name = "disktracker")]
#[command(about = "DiskTracker — Windows file-system observation daemon", long_about = None)]
struct Cli {
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
    /// Internal subcommand to start the IPC daemon server
    #[command(hide = true)]
    Daemon,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Daemon => {
            if let Err(e) = api::run_server(PIPE_PATH).await {
                eprintln!("[Daemon] Daemon error: {:?}", e);
                std::process::exit(1);
            }
        }
        Commands::Init => {
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

                cmd.spawn()?;

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
    }

    Ok(())
}

async fn query_status() -> Result<JsonRpcResponse, Box<dyn std::error::Error>> {
    let mut stream = platform_windows::connect_client(PIPE_PATH).await?;

    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "status".to_string(),
        params: serde_json::Value::Null,
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
    match storage::get_db_connection() {
        Ok(conn) => {
            let integrity: Result<String, rusqlite::Error> =
                conn.query_row("PRAGMA integrity_check", [], |row| row.get(0));
            match integrity {
                Ok(status) if status == "ok" => {
                    println!("  [OK] SQLite database integrity");
                }
                Ok(status) => {
                    println!(
                        "  [FAIL] SQLite database integrity check returned: {}",
                        status
                    );
                    all_ok = false;
                }
                Err(e) => {
                    println!("  [FAIL] SQLite database integrity: Failed to run PRAGMA integrity_check: {:?}", e);
                    all_ok = false;
                }
            }
        }
        Err(e) => {
            println!(
                "  [FAIL] SQLite database integrity: Failed to connect to SQLite database: {:?}",
                e
            );
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
                        Ok(_) => println!("  [OK] Daemon stopped successfully."),
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

    // 2. Delete SQLite database files and AppData folder
    let db_dir = storage::get_db_dir()?;
    if db_dir.exists() {
        println!(
            "[Cli] Deleting database files and directory at {:?}...",
            db_dir
        );
        std::fs::remove_dir_all(&db_dir)?;
        println!("  [OK] Database files and directory deleted.");
    } else {
        println!("  [OK] AppData directory does not exist.");
    }

    // 3. Clean up Unix Fallback Named Pipe
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
