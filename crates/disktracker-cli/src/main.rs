use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use disktracker_core::scan::{scan, ScanConfig};
use indicatif::{ProgressBar, ProgressStyle};
use mimalloc::MiMalloc;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod diff;
mod report;
mod store;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

// ─── CLI definition ──────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "disktracker",
    about = "Real-time storage observability engine",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan the filesystem and store a snapshot
    Scan {
        path: Option<PathBuf>,
        #[arg(long)]
        max_depth: Option<u16>,
        #[arg(long = "skip", value_name = "NAME")]
        skip: Vec<String>,
        #[arg(long)]
        one_filesystem: bool,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long)]
        quiet: bool,
        #[arg(long)]
        json: bool,
        /// Emit benchmark results as JSON (suppresses normal output)
        #[arg(long)]
        bench: bool,
        /// Number of scan threads (0 = auto, 1 = single-threaded)
        #[arg(long, default_value = "0")]
        parallelism: u16,
        /// Force a full cold scan, ignoring any cached snapshots
        #[arg(long, conflicts_with = "warm")]
        cold: bool,
        /// Force a warm scan (fails if no previous snapshot exists)
        #[arg(long, conflicts_with = "cold")]
        warm: bool,
        /// Scan all drives/roots on the system
        #[arg(long, conflicts_with = "path")]
        all: bool,
    },

    /// Show what changed between two snapshots
    Diff {
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
        #[arg(long, default_value = "20")]
        top: usize,
        /// Minimum size change to show in bytes [default: 1 MB]
        #[arg(long, default_value = "1048576")]
        min_delta: i64,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },

    /// Human-readable growth report
    Report {
        #[arg(long = "last", default_value = "7d")]
        last: String,
        #[arg(long, default_value = "15")]
        top: usize,
        #[arg(long, default_value = "4")]
        depth: u16,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },

    /// List stored snapshot timestamps
    List {
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },

    // ── MVP2 commands ─────────────────────────────────────────────────────
    /// Watch the filesystem for changes in real time
    Watch {
        /// Root path to watch [default: / on Unix]
        path: Option<PathBuf>,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long)]
        quiet: bool,
        #[arg(long)]
        one_filesystem: bool,
        #[arg(long = "skip", value_name = "NAME")]
        skip: Vec<String>,
        /// Debounce window in milliseconds [default: 500]
        #[arg(long, default_value = "500")]
        debounce_ms: u64,
        /// Flush state to DB every N seconds [default: 3600]
        #[arg(long, default_value = "3600")]
        flush_secs: u64,
        /// Watch all drives/roots on the system
        #[arg(long, conflicts_with = "path")]
        all: bool,
    },

    /// Explain what caused disk growth with human-readable attribution
    Explain {
        /// Time window: e.g. "7d", "2w", "1m" [default: 7d]
        #[arg(long = "last", default_value = "7d")]
        last: String,
        #[arg(long, default_value = "15")]
        top: usize,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },

    /// Show growth history for a specific directory
    Timeline {
        /// Directory path to inspect
        path: String,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },

    /// Validate watcher consistency and repair stale state
    Reconcile {
        #[arg(long)]
        db: Option<PathBuf>,
        /// Perform a fresh full scan to detect and fix drift
        #[arg(long)]
        full: bool,
        #[arg(long)]
        json: bool,
    },

    /// Delete old snapshots to reclaim database space
    Prune {
        /// Keep only the N most recent snapshots [e.g. --keep-last 30]
        #[arg(long, value_name = "N")]
        keep_last: Option<usize>,
        /// Delete snapshots older than this window [e.g. --older-than 90d, 12w, 6m]
        #[arg(long, value_name = "DURATION")]
        older_than: Option<String>,
        /// Preview deletions without making any changes
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn default_db_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".disktracker")
        .join("data.db")
}

fn default_scan_root() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::current_dir()
            .ok()
            .and_then(|p| {
                p.components().next().map(|c| {
                    let mut root = PathBuf::from(c.as_os_str());
                    root.push("\\");
                    root
                })
            })
            .unwrap_or_else(|| PathBuf::from("C:\\"))
    }
    #[cfg(unix)]
    {
        PathBuf::from("/")
    }
}

fn detect_all_drives() -> Vec<PathBuf> {
    if let Ok(mocked) = std::env::var("DISKTRACKER_MOCK_ALL_DRIVES") {
        return mocked.split(',').map(PathBuf::from).collect();
    }
    #[cfg(windows)]
    {
        let mut drives = Vec::new();
        for c in b'A'..=b'Z' {
            let drive_str = format!("{}:\\", c as char);
            let path = PathBuf::from(drive_str);
            if path.exists() {
                drives.push(path);
            }
        }
        if drives.is_empty() {
            drives.push(PathBuf::from("C:\\"));
        }
        drives
    }
    #[cfg(not(windows))]
    {
        vec![PathBuf::from("/")]
    }
}

// ─── MVP1 commands ────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn cmd_scan(
    path: Option<PathBuf>,
    all: bool,
    max_depth: Option<u16>,
    skip: Vec<String>,
    one_filesystem: bool,
    db: Option<PathBuf>,
    quiet: bool,
    json: bool,
    bench: bool,
    parallelism: u16,
    cold: bool,
    warm: bool,
) -> Result<()> {
    let db_path = db.unwrap_or_else(default_db_path);
    let conn = store::open_db(&db_path)?;

    let roots = if all {
        detect_all_drives()
    } else {
        vec![path.unwrap_or_else(default_scan_root)]
    };

    let interrupted = Arc::new(AtomicBool::new(false));
    let ic = interrupted.clone();
    ctrlc::set_handler(move || {
        ic.store(true, Ordering::SeqCst);
    })
    .ok();

    for root in roots {
        if interrupted.load(Ordering::SeqCst) {
            break;
        }

        let skip_bytes: Vec<Vec<u8>> = skip.iter().map(|s| s.as_bytes().to_vec()).collect();
        let scan_root_str = root.to_string_lossy().into_owned();

        // Determine if we should perform a warm scan
        let mut skip_predicate = None;
        let mut warm_scan_attempted = false;
        let skipped_dirs_count = Arc::new(std::sync::atomic::AtomicU32::new(0));

        if !cold {
            let latest_snap_id = store::get_latest_snapshot_for_root(&conn, &scan_root_str)?;
            if let Some(snap_id) = latest_snap_id {
                if !quiet && !json && !bench {
                    eprintln!(
                        "Loading snapshot index for warm scan (snapshot #{})...",
                        snap_id
                    );
                }
                if let Ok(index) = store::load_snapshot_index(&conn, snap_id) {
                    let predicate = index.build_skip_predicate();
                    let skipped_dirs_count_clone = skipped_dirs_count.clone();
                    skip_predicate = Some(Arc::new(
                        move |path_bytes: &[u8],
                              mtime: i64,
                              identity: disktracker_core::identity::FsIdentity| {
                            let res = predicate(path_bytes, mtime, identity);
                            if res.is_some() {
                                skipped_dirs_count_clone
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                            res
                        },
                    )
                        as Arc<
                            dyn Fn(
                                    &[u8],
                                    i64,
                                    disktracker_core::identity::FsIdentity,
                                )
                                    -> Option<disktracker_core::scan::SkipResult>
                                + Send
                                + Sync,
                        >);
                    warm_scan_attempted = true;
                }
            } else if warm {
                anyhow::bail!(
                    "Force-warm scan requested but no previous snapshot found for root '{}'",
                    scan_root_str
                );
            }
        }

        if bench {
            // Bench mode: measure scan, emit JSON, skip DB storage
            use disktracker_core::bench::{BenchHandle, ScanMetrics};
            let handle = BenchHandle::start();
            let result = scan(ScanConfig {
                root: root.clone(),
                max_depth,
                skip_names: skip_bytes,
                one_filesystem,
                cancel_flag: Some(interrupted.clone()),
                parallelism,
                skip_predicate,
            });
            let bench_result = handle.finish(ScanMetrics {
                total_files: result.total_files,
                total_dirs: result.arena.node_count() as u64,
                total_bytes: result.total_bytes,
                error_count: result.error_count,
            });
            println!("{}", serde_json::to_string_pretty(&bench_result)?);
            continue;
        }

        let started_at = chrono::Utc::now().timestamp();

        let pb = if !quiet && !json {
            let pb = ProgressBar::new_spinner();
            pb.set_style(
                ProgressStyle::with_template("{spinner:.cyan} {msg}")
                    .unwrap()
                    .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
            );
            pb.set_message(format!("Scanning {} ...", scan_root_str));
            pb.enable_steady_tick(std::time::Duration::from_millis(80));
            Some(pb)
        } else {
            None
        };

        let result = scan(ScanConfig {
            root: root.clone(),
            max_depth,
            skip_names: skip_bytes,
            one_filesystem,
            cancel_flag: Some(interrupted.clone()),
            parallelism,
            skip_predicate,
        });

        if let Some(ref pb) = pb {
            pb.finish_and_clear();
        }
        if interrupted.load(Ordering::SeqCst) {
            eprintln!("Scan interrupted — no data written.");
            return Ok(());
        }

        let finished_at = chrono::Utc::now().timestamp();
        let dir_count = result.arena.node_count() as u64;

        if warm_scan_attempted && !quiet && !json && !bench {
            let reused = skipped_dirs_count.load(Ordering::SeqCst);
            let total_dirs = dir_count;
            if total_dirs > 0 {
                let hit_rate = (reused as f64) / (total_dirs as f64) * 100.0;
                eprintln!(
                    "Warm scan: reused {}/{} directories ({:.1}% hit rate)",
                    reused, total_dirs, hit_rate
                );
            }
        }

        let snapshot_id = store::insert_snapshot(
            &conn,
            &scan_root_str,
            started_at,
            finished_at,
            result.total_files,
            result.total_bytes,
            result.error_count,
        )?;

        if let Err(e) = store::bulk_insert_dirs(&conn, snapshot_id, &result.arena) {
            eprintln!("DB write failed: {} — rolling back snapshot.", e);
            store::rollback_snapshot(&conn, snapshot_id)?;
            return Err(e);
        }

        let db_path_str = db_path.to_string_lossy().into_owned();
        report::print_scan_summary(
            &report::ScanSummary {
                root: &scan_root_str,
                directories: dir_count,
                files: result.total_files,
                total_bytes: result.total_bytes,
                duration_ms: result.scan_duration_ms,
                snapshot_id,
                db_path: &db_path_str,
                error_count: result.error_count,
            },
            json,
        );
    }
    Ok(())
}

fn cmd_diff(
    from: Option<String>,
    to: Option<String>,
    top: usize,
    min_delta: i64,
    db: Option<PathBuf>,
    json: bool,
) -> Result<()> {
    let conn = store::open_db(&db.unwrap_or_else(default_db_path))?;
    let snap_b = match to {
        Some(ref s) => store::resolve_snapshot_ref(&conn, s)?,
        None => store::get_latest_snapshot_id(&conn)?
            .ok_or_else(|| anyhow::anyhow!("No snapshots found. Run `disktracker scan` first."))?,
    };
    let snap_a = match from {
        Some(ref s) => store::resolve_snapshot_ref(&conn, s)?,
        None => {
            let snaps = store::list_snapshots(&conn)?;
            if snaps.len() < 2 {
                anyhow::bail!("Need at least 2 snapshots. Got {}.", snaps.len());
            }
            snaps[snaps.len() - 2].id
        }
    };
    let result = diff::compute_diff(&conn, snap_a, snap_b, top, min_delta.abs())?;
    report::print_diff(&result, json);
    Ok(())
}

fn cmd_report(last: String, top: usize, depth: u16, db: Option<PathBuf>, json: bool) -> Result<()> {
    let conn = store::open_db(&db.unwrap_or_else(default_db_path))?;
    let snap_b = store::get_latest_snapshot_id(&conn)?
        .ok_or_else(|| anyhow::anyhow!("No snapshots found. Run `disktracker scan` first."))?;
    let snap_a = store::resolve_snapshot_ref(&conn, &last)?;
    let result = diff::compute_diff(&conn, snap_a, snap_b, top, 0)?;
    report::print_report(&result, &last, Some(depth), json);
    Ok(())
}

fn cmd_list(db: Option<PathBuf>, json: bool) -> Result<()> {
    let conn = store::open_db(&db.unwrap_or_else(default_db_path))?;
    let snapshots = store::list_snapshots(&conn)?;
    if snapshots.is_empty() {
        eprintln!("No snapshots found. Run `disktracker scan` first.");
        return Ok(());
    }
    report::print_snapshot_list(&snapshots, json);
    Ok(())
}

// ─── MVP2 commands ────────────────────────────────────────────────────────────

struct CmdWatchArgs {
    path: Option<PathBuf>,
    all: bool,
    db: Option<PathBuf>,
    quiet: bool,
    one_filesystem: bool,
    skip: Vec<String>,
    debounce_ms: u64,
    flush_secs: u64,
}

fn cmd_watch(args: CmdWatchArgs) -> Result<()> {
    let roots = if args.all {
        detect_all_drives()
    } else {
        vec![args.path.unwrap_or_else(default_scan_root)]
    };
    let db_path = args.db.unwrap_or_else(default_db_path);
    let skip_bytes: Vec<Vec<u8>> = args.skip.iter().map(|s| s.as_bytes().to_vec()).collect();

    if !args.quiet {
        let root_names: Vec<String> = roots
            .iter()
            .map(|r| r.to_string_lossy().into_owned())
            .collect();
        eprintln!(
            "[watch] Starting real-time monitoring of {} (Ctrl+C to stop)",
            root_names.join(", ")
        );
    }

    disktracker_watch::run_watch(disktracker_watch::WatchConfig {
        roots,
        db_path,
        debounce_ms: args.debounce_ms,
        quiet: args.quiet,
        one_filesystem: args.one_filesystem,
        skip_names: skip_bytes,
        flush_interval_secs: args.flush_secs,
    })
}

fn cmd_explain(last: String, top: usize, db: Option<PathBuf>, json: bool) -> Result<()> {
    let conn = store::open_db(&db.unwrap_or_else(default_db_path))?;
    let snap_b = store::get_latest_snapshot_id(&conn)?
        .ok_or_else(|| anyhow::anyhow!("No snapshots found. Run `disktracker scan` first."))?;
    let snap_a = store::resolve_snapshot_ref(&conn, &last)
        .with_context(|| format!("Cannot find a snapshot from '{}' ago", last))?;
    let entries = disktracker_db::explain::query_explain(&conn, snap_a, snap_b, top)?;
    report::print_explain(&entries, &last, json);
    Ok(())
}

fn cmd_timeline(path: String, db: Option<PathBuf>, json: bool) -> Result<()> {
    let conn = store::open_db(&db.unwrap_or_else(default_db_path))?;
    // Try full snapshots first, fall back to delta records
    let mut entries = disktracker_db::timeline::query_timeline(&conn, &path)?;
    if entries.is_empty() {
        entries = disktracker_db::timeline::query_timeline_from_deltas(&conn, &path)?;
    }
    report::print_timeline(&entries, &path, json);
    Ok(())
}

fn cmd_reconcile(db: Option<PathBuf>, full: bool, json: bool) -> Result<()> {
    let db_path = db.unwrap_or_else(default_db_path);
    let conn = store::open_db(&db_path)?;

    // Read current watch state
    let state = disktracker_db::watch_state::get_watch_state(&conn)?;
    let snapshots = store::list_snapshots(&conn)?;
    let snapshot_count = snapshots.len();

    let (watch_roots, watch_root_str, last_event, last_reconcile) = state
        .as_ref()
        .map(|s| {
            let roots = disktracker_watch::incremental::bytes_to_roots(&s.watch_root);
            let root_names: Vec<String> = roots
                .iter()
                .map(|r| r.to_string_lossy().into_owned())
                .collect();
            let root_str = root_names.join(", ");
            (
                roots,
                Some(root_str),
                s.last_event_time,
                s.last_reconcile_time,
            )
        })
        .unwrap_or((vec![], None, None, None));

    let mut new_snap_id: Option<i64> = None;
    let mut drift_bytes: i64 = 0;

    if full {
        // Determine root to scan
        let roots = if !watch_roots.is_empty() {
            watch_roots
        } else if let Some(last_snap) = snapshots.last() {
            vec![PathBuf::from(&last_snap.scan_root)]
        } else {
            vec![default_scan_root()]
        };

        if !json {
            let root_names: Vec<String> = roots
                .iter()
                .map(|r| r.to_string_lossy().into_owned())
                .collect();
            eprintln!("Reconciling: scanning {} ...", root_names.join(", "));
        }

        let mut total_drift_bytes: i64 = 0;
        let mut last_snap_id: Option<i64> = None;

        for root in roots {
            let root_str = root.to_string_lossy().into_owned();
            let prev_snap_id = store::get_latest_snapshot_for_root(&conn, &root_str)?;
            let prev_bytes = if let Some(pid) = prev_snap_id {
                store::get_snapshot(&conn, pid)?
                    .map(|s| s.total_bytes)
                    .unwrap_or(0)
            } else {
                0
            };

            let result = scan(ScanConfig {
                root: root.clone(),
                max_depth: None,
                skip_names: vec![],
                one_filesystem: false,
                cancel_flag: None,
                ..Default::default()
            });

            let now = chrono::Utc::now().timestamp();
            let snap_id = store::insert_snapshot(
                &conn,
                &root_str,
                now,
                now,
                result.total_files,
                result.total_bytes,
                result.error_count,
            )?;
            store::bulk_insert_dirs(&conn, snap_id, &result.arena)?;
            last_snap_id = Some(snap_id);

            let drift = result.total_bytes as i64 - prev_bytes;
            total_drift_bytes += drift;
        }

        drift_bytes = total_drift_bytes;
        new_snap_id = last_snap_id;

        disktracker_db::watch_state::touch_reconcile_time(&conn, chrono::Utc::now().timestamp())?;
    } else {
        // Lazy reconciliation (default)
        if let Some(ref s) = state {
            if let (Some(reconcile_time), Some(snapshot_id)) =
                (s.last_reconcile_time, s.last_snapshot_id)
            {
                if !json {
                    eprintln!(
                        "Reconciling: performing lazy reconciliation since timestamp {} for snapshot #{}...",
                        reconcile_time, snapshot_id
                    );
                }
                let now = chrono::Utc::now().timestamp();
                drift_bytes =
                    disktracker_db::mutation::lazy_reconcile(&conn, snapshot_id, reconcile_time)?;
                disktracker_db::watch_state::touch_reconcile_time(&conn, now)?;
                new_snap_id = Some(snapshot_id);
            }
        }
    }

    report::print_reconcile(
        watch_root_str.as_deref(),
        last_event,
        last_reconcile,
        snapshot_count,
        new_snap_id,
        drift_bytes,
        json,
    );
    Ok(())
}

fn cmd_prune(
    keep_last: Option<usize>,
    older_than: Option<&str>,
    dry_run: bool,
    db: Option<PathBuf>,
    json: bool,
) -> Result<()> {
    if keep_last.is_none() && older_than.is_none() {
        anyhow::bail!(
            "Specify at least one pruning rule:\n  \
             --keep-last N       keep the N most recent snapshots\n  \
             --older-than DURATION  delete snapshots older than e.g. 90d, 12w, 6m"
        );
    }

    let conn = store::open_db(&db.unwrap_or_else(default_db_path))?;
    let candidates = disktracker_db::prune::resolve_prune_candidates(&conn, keep_last, older_than)?;

    if dry_run {
        let preview = disktracker_db::prune::preview_prune(&conn, &candidates)?;
        let result = disktracker_db::prune::PruneResult {
            deleted_snapshots: preview.snapshot_ids,
            deleted_snapshot_count: preview.snapshot_count,
            deleted_dir_rows: preview.dir_row_count,
            deleted_delta_rows: preview.delta_row_count,
            deleted_event_rows: preview.event_row_count,
            freed_bytes_approx: preview.freed_bytes_approx,
            dry_run: true,
        };
        report::print_prune(&result, json);
    } else {
        let result = disktracker_db::prune::execute_prune(&conn, &candidates)?;
        report::print_prune(&result, json);
    }
    Ok(())
}

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Scan {
            path,
            max_depth,
            skip,
            one_filesystem,
            db,
            quiet,
            json,
            bench,
            parallelism,
            cold,
            warm,
            all,
        } => cmd_scan(
            path,
            all,
            max_depth,
            skip,
            one_filesystem,
            db,
            quiet,
            json,
            bench,
            parallelism,
            cold,
            warm,
        ),
        Commands::Diff {
            from,
            to,
            top,
            min_delta,
            db,
            json,
        } => cmd_diff(from, to, top, min_delta, db, json),
        Commands::Report {
            last,
            top,
            depth,
            db,
            json,
        } => cmd_report(last, top, depth, db, json),
        Commands::List { db, json } => cmd_list(db, json),
        Commands::Watch {
            path,
            db,
            quiet,
            one_filesystem,
            skip,
            debounce_ms,
            flush_secs,
            all,
        } => cmd_watch(CmdWatchArgs {
            path,
            all,
            db,
            quiet,
            one_filesystem,
            skip,
            debounce_ms,
            flush_secs,
        }),
        Commands::Explain {
            last,
            top,
            db,
            json,
        } => cmd_explain(last, top, db, json),
        Commands::Timeline { path, db, json } => cmd_timeline(path, db, json),
        Commands::Reconcile { db, full, json } => cmd_reconcile(db, full, json),
        Commands::Prune {
            keep_last,
            older_than,
            dry_run,
            db,
            json,
        } => cmd_prune(keep_last, older_than.as_deref(), dry_run, db, json),
    };
    if let Err(e) = result {
        eprintln!("Error: {:#}", e);
        std::process::exit(1);
    }
}
