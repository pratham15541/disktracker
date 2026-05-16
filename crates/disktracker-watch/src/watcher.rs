use crate::incremental::{path_to_bytes, IncrementalEngine};
use anyhow::Result;
use chrono::Utc;
use disktracker_db::{
    delta::bulk_insert_deltas,
    events::insert_fs_events_batch,
    store::{insert_snapshot, rollback_snapshot},
    watch_state::{touch_event_time, touch_reconcile_time, upsert_watch_state, WatchState},
};
use disktracker_events::{FsEvent, FsEventKind};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Configuration for a watch session.
pub struct WatchConfig {
    pub root: PathBuf,
    pub db_path: PathBuf,
    pub debounce_ms: u64,
    pub quiet: bool,
    pub one_filesystem: bool,
    pub skip_names: Vec<Vec<u8>>,
    /// Flush a delta snapshot to DB every N seconds (0 = only on Ctrl+C).
    pub flush_interval_secs: u64,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from("/"),
            db_path: PathBuf::new(),
            debounce_ms: 500,
            quiet: false,
            one_filesystem: false,
            skip_names: vec![],
            flush_interval_secs: 3600, // 1 hour
        }
    }
}

/// Run the watch session. Blocks until Ctrl+C.
pub fn run_watch(config: WatchConfig) -> Result<()> {
    let conn = disktracker_db::open_db(&config.db_path)?;

    if !config.quiet {
        eprintln!("[watch] Initial scan of {} …", config.root.display());
    }

    let (mut engine, initial_result) = IncrementalEngine::init(
        config.root.clone(),
        config.skip_names.clone(),
        config.one_filesystem,
    );

    let started_at = Utc::now().timestamp();
    let root_str = config.root.to_string_lossy().into_owned();

    // Store initial snapshot
    let snap_id = insert_snapshot(
        &conn,
        &root_str,
        started_at,
        Utc::now().timestamp(),
        initial_result.total_files,
        initial_result.total_bytes,
        initial_result.error_count,
    )?;

    if let Err(e) = disktracker_db::store::bulk_insert_dirs(&conn, snap_id, &initial_result.arena) {
        eprintln!(
            "[watch] Failed to store initial snapshot: {} — rolling back",
            e
        );
        rollback_snapshot(&conn, snap_id)?;
        return Err(e);
    }

    // Persist watch state
    upsert_watch_state(
        &conn,
        &WatchState {
            watch_root: path_to_bytes(&config.root),
            last_event_time: None,
            last_reconcile_time: Some(Utc::now().timestamp()),
            last_snapshot_id: Some(snap_id),
        },
    )?;

    if !config.quiet {
        eprintln!(
            "[watch] Initial snapshot #{} stored ({} dirs, {}). Watching…",
            snap_id,
            initial_result.arena.nodes.len(),
            fmt_bytes(initial_result.total_bytes),
        );
    }

    // Set up Ctrl+C handler
    let running = Arc::new(AtomicBool::new(true));
    {
        let r = running.clone();
        ctrlc::set_handler(move || {
            r.store(false, Ordering::SeqCst);
        })
        .ok();
    }

    // Set up notify watcher
    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let _ = tx.send(res);
        })?;
    watcher.watch(&config.root, RecursiveMode::Recursive)?;

    let debounce = Duration::from_millis(config.debounce_ms);
    let flush_interval = Duration::from_secs(config.flush_interval_secs.max(1));
    let mut last_flush = Instant::now();
    let mut current_snap_id = snap_id;
    let mut pending_events: Vec<FsEvent> = Vec::new();

    // ─── Main event loop ──────────────────────────────────────────────────
    while running.load(Ordering::SeqCst) {
        // Block until first event (with a 1-second timeout to check running flag)
        let first = rx.recv_timeout(Duration::from_secs(1));

        let event = match first {
            Ok(Ok(ev)) => ev,
            Ok(Err(e)) => {
                eprintln!("[watch] Watcher error: {}", e);
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {
                // Check if flush needed
                if last_flush.elapsed() >= flush_interval {
                    flush_to_db(
                        &conn,
                        &engine,
                        &mut current_snap_id,
                        &mut pending_events,
                        &root_str,
                        config.quiet,
                    )?;
                    last_flush = Instant::now();
                    touch_reconcile_time(&conn, Utc::now().timestamp())?;
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };

        // Ingest first event
        engine.ingest_event(&event);
        record_notify_event(&event, &mut pending_events);
        touch_event_time(&conn, Utc::now().timestamp()).ok();

        // Drain remaining events within the debounce window
        let deadline = Instant::now() + debounce;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(Ok(ev)) => {
                    engine.ingest_event(&ev);
                    record_notify_event(&ev, &mut pending_events);
                }
                Ok(Err(e)) => eprintln!("[watch] Watcher error: {}", e),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    running.store(false, Ordering::SeqCst);
                    break;
                }
            }
        }

        // Process dirty queue
        match engine.process_dirty_batch(&conn) {
            Ok(deltas) if !deltas.is_empty() => {
                if !config.quiet {
                    for d in &deltas {
                        let path_str = std::str::from_utf8(&d.path).unwrap_or("<non-utf8>");
                        eprintln!(
                            "[watch] {} {:>10}",
                            path_str,
                            fmt_signed_bytes(d.delta_bytes)
                        );
                    }
                }
                // Store raw events
                if !pending_events.is_empty() {
                    insert_fs_events_batch(&conn, &pending_events).ok();
                    pending_events.clear();
                }
                // Store deltas
                bulk_insert_deltas(&conn, current_snap_id, &deltas).ok();
            }
            Ok(_) => {}
            Err(e) => eprintln!("[watch] Rescan error: {}", e),
        }

        // Periodic flush
        if last_flush.elapsed() >= flush_interval {
            flush_to_db(
                &conn,
                &engine,
                &mut current_snap_id,
                &mut pending_events,
                &root_str,
                config.quiet,
            )?;
            last_flush = Instant::now();
        }
    }

    // Final flush on exit
    if !config.quiet {
        eprintln!("[watch] Shutting down — flushing final snapshot…");
    }
    flush_to_db(
        &conn,
        &engine,
        &mut current_snap_id,
        &mut pending_events,
        &root_str,
        config.quiet,
    )?;
    touch_reconcile_time(&conn, Utc::now().timestamp())?;
    Ok(())
}

/// Flush current in-memory state to a new snapshot.
fn flush_to_db(
    conn: &Connection,
    engine: &IncrementalEngine,
    snap_id: &mut i64,
    pending_events: &mut Vec<FsEvent>,
    root_str: &str,
    quiet: bool,
) -> Result<()> {
    // Store any pending raw events
    if !pending_events.is_empty() {
        insert_fs_events_batch(conn, pending_events).ok();
        pending_events.clear();
    }

    let now = Utc::now().timestamp();
    let root_bytes = root_str.as_bytes().to_vec();
    let total_bytes = engine.current_size(&root_bytes).max(0) as u64;

    let new_snap_id = insert_snapshot(conn, root_str, now, now, 0, total_bytes, 0)?;

    // Write current state as a full dir_snapshot
    // (for large states this could be delta-only, but correctness first)
    {
        let tx = conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO dir_snapshots
                 (snapshot_id, path_blob, path_utf8, depth, total_bytes, file_count, mtime)
                 VALUES (?1, ?2, ?3, 0, ?4, 0, ?5)",
            )?;
            for (path, &bytes) in &engine.state {
                let utf8 = std::str::from_utf8(path).ok().map(str::to_owned);
                stmt.execute(rusqlite::params![
                    new_snap_id,
                    path,
                    utf8,
                    bytes.max(0),
                    now,
                ])?;
            }
        }
        tx.commit()?;
    }

    *snap_id = new_snap_id;

    if !quiet {
        eprintln!(
            "[watch] Flushed snapshot #{} ({} dirs, {})",
            new_snap_id,
            engine.state.len(),
            fmt_bytes(total_bytes),
        );
    }
    Ok(())
}

fn record_notify_event(event: &notify::Event, out: &mut Vec<FsEvent>) {
    let kind = match event.kind {
        EventKind::Create(_) => FsEventKind::Create,
        EventKind::Remove(_) => FsEventKind::Delete,
        EventKind::Modify(_) => FsEventKind::Modify,
        EventKind::Access(_) => return, // not interesting
        _ => FsEventKind::Other,
    };
    let ts = Utc::now().timestamp();
    for path in &event.paths {
        let path_bytes = {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                path.as_os_str().as_bytes().to_vec()
            }
            #[cfg(windows)]
            {
                path.to_string_lossy().into_owned().into_bytes()
            }
        };
        out.push(FsEvent {
            timestamp: ts,
            kind,
            path: path_bytes,
            is_dir: path.is_dir(),
        });
    }
}

fn fmt_bytes(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1e6)
    } else if bytes >= 1_000 {
        format!("{:.1} KB", bytes as f64 / 1e3)
    } else {
        format!("{} B", bytes)
    }
}

fn fmt_signed_bytes(bytes: i64) -> String {
    let sign = if bytes >= 0 { "+" } else { "" };
    format!("{}{}", sign, fmt_bytes(bytes.unsigned_abs()))
}
