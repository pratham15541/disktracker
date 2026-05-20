use crate::incremental::IncrementalEngine;
use anyhow::Result;
use chrono::Utc;
use disktracker_db::{
    delta::bulk_insert_deltas,
    events::insert_fs_events_batch,
    mutation::insert_mutations_batch,
    store::{insert_snapshot, rollback_snapshot},
    watch_state::{touch_event_time, touch_reconcile_time, upsert_watch_state},
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
    pub roots: Vec<PathBuf>,
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
            roots: vec![PathBuf::from("/")],
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

    let mmap_path = config.db_path.with_extension("mmap");

    let mut hydrated_snap_id = None;
    let (mut engine, initial_scan_results) = if mmap_path.exists() {
        if !config.quiet {
            eprintln!(
                "[watch] Hydrating instantly from memory-mapped index: {} …",
                mmap_path.display()
            );
        }
        match crate::mmap_index::MmapState::load_from_file(&mmap_path) {
            Ok(state) => {
                let engine = IncrementalEngine::init_from_state(
                    config.roots.clone(),
                    config.skip_names.clone(),
                    config.one_filesystem,
                    state,
                );
                // Attempt to load the last snapshot ID from DB
                if let Ok(Some(ws)) = disktracker_db::watch_state::get_watch_state(&conn) {
                    hydrated_snap_id = ws.last_snapshot_id;
                }
                (engine, None)
            }
            Err(e) => {
                eprintln!(
                    "[watch] Failed to load mmap index: {} — falling back to physical scan",
                    e
                );
                let (engine, initial_results) = IncrementalEngine::init_multi(
                    config.roots.clone(),
                    config.skip_names.clone(),
                    config.one_filesystem,
                );
                (engine, Some(initial_results))
            }
        }
    } else {
        let (engine, initial_results) = IncrementalEngine::init_multi(
            config.roots.clone(),
            config.skip_names.clone(),
            config.one_filesystem,
        );
        (engine, Some(initial_results))
    };

    let started_at = Utc::now().timestamp();

    let snap_id = if let Some(sid) = hydrated_snap_id {
        if !config.quiet {
            eprintln!("[watch] Reusing snapshot #{} from watch_state.", sid);
        }
        sid
    } else {
        let results = match initial_scan_results {
            Some(res) => res,
            None => {
                if !config.quiet {
                    eprintln!("[watch] No snapshot ID found in DB, conducting fallback scans…");
                }
                let mut scan_results = Vec::new();
                for root in &config.roots {
                    let config_scan = disktracker_core::scan::ScanConfig {
                        root: root.clone(),
                        max_depth: None,
                        skip_names: config.skip_names.clone(),
                        one_filesystem: config.one_filesystem,
                        cancel_flag: None,
                        ..Default::default()
                    };
                    scan_results.push(disktracker_core::scan::scan(config_scan));
                }
                scan_results
            }
        };

        let mut last_sid = 0;
        for (root, result) in config.roots.iter().zip(results) {
            let root_str = root.to_string_lossy().into_owned();
            let sid = insert_snapshot(
                &conn,
                &root_str,
                started_at,
                Utc::now().timestamp(),
                result.total_files,
                result.total_bytes,
                result.error_count,
            )?;
            last_sid = sid;

            if let Err(e) = disktracker_db::store::bulk_insert_dirs(&conn, sid, &result.arena) {
                eprintln!(
                    "[watch] Failed to store initial snapshot for {}: {} — rolling back",
                    root.display(),
                    e
                );
                rollback_snapshot(&conn, sid)?;
                return Err(e);
            }
        }

        // Try to write it so next boot is instant
        if let Err(e2) = engine.state.write_to_file(&mmap_path) {
            eprintln!("[watch] Failed to write initial mmap index: {}", e2);
        }

        last_sid
    };

    // Persist watch state
    upsert_watch_state(
        &conn,
        &disktracker_db::watch_state::WatchState {
            watch_root: crate::incremental::roots_to_bytes(&config.roots),
            last_event_time: None,
            last_reconcile_time: Some(Utc::now().timestamp()),
            last_snapshot_id: Some(snap_id),
        },
    )?;

    if !config.quiet {
        let root_names: Vec<String> = config
            .roots
            .iter()
            .map(|r| r.to_string_lossy().into_owned())
            .collect();
        eprintln!(
            "[watch] Watching {} (snapshot #{}, {} entries). Watching…",
            root_names.join(", "),
            snap_id,
            engine.state.len(),
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
    for root in &config.roots {
        watcher.watch(root, RecursiveMode::Recursive)?;
    }

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
                        &mut engine,
                        &mut current_snap_id,
                        &mut pending_events,
                        &config.roots,
                        config.quiet,
                        &mmap_path,
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
                    if let Err(e) = insert_mutations_batch(&conn, &pending_events) {
                        eprintln!("[watch] Failed to log mutations: {}", e);
                    }
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
                &mut engine,
                &mut current_snap_id,
                &mut pending_events,
                &config.roots,
                config.quiet,
                &mmap_path,
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
        &mut engine,
        &mut current_snap_id,
        &mut pending_events,
        &config.roots,
        config.quiet,
        &mmap_path,
    )?;
    touch_reconcile_time(&conn, Utc::now().timestamp())?;
    Ok(())
}

/// Flush current in-memory state to a new snapshot.
fn flush_to_db(
    conn: &Connection,
    engine: &mut IncrementalEngine,
    snap_id: &mut i64,
    pending_events: &mut Vec<FsEvent>,
    roots: &[PathBuf],
    quiet: bool,
    mmap_path: &std::path::Path,
) -> Result<()> {
    // Store any pending raw events
    if !pending_events.is_empty() {
        insert_fs_events_batch(conn, pending_events).ok();
        if let Err(e) = insert_mutations_batch(conn, pending_events) {
            eprintln!("[watch] Failed to log mutations: {}", e);
        }
        pending_events.clear();
    }

    let now = Utc::now().timestamp();
    let mut last_new_snap_id = *snap_id;

    for root in roots {
        let root_str = root.to_string_lossy().into_owned();
        let root_bytes = crate::incremental::path_to_bytes(root);
        let total_bytes = engine.current_size(&root_bytes).max(0) as u64;

        let new_snap_id = insert_snapshot(conn, &root_str, now, now, 0, total_bytes, 0)?;
        last_new_snap_id = new_snap_id;

        // Write current state as a full dir_snapshot
        {
            let tx = conn.unchecked_transaction()?;
            {
                let mut stmt = tx.prepare_cached(
                    "INSERT INTO dir_snapshots
                     (snapshot_id, path_blob, path_utf8, depth, total_bytes, file_count, mtime)
                     VALUES (?1, ?2, ?3, 0, ?4, 0, ?5)",
                )?;
                for (path, bytes) in engine.state.get_active_entries() {
                    if path.starts_with(&root_bytes) {
                        let utf8 = std::str::from_utf8(&path).ok().map(str::to_owned);
                        stmt.execute(rusqlite::params![
                            new_snap_id,
                            path,
                            utf8,
                            bytes.max(0),
                            now,
                        ])?;
                    }
                }
            }
            tx.commit()?;
        }
    }

    // Now write & reload the mmap index!
    if let Err(e) = engine.state.write_to_file(mmap_path) {
        eprintln!("[watch] Failed to write mmap index: {}", e);
    } else {
        if let Err(e) = engine.state.reload_from_file(mmap_path) {
            eprintln!("[watch] Failed to reload mmap index: {}", e);
        }
    }

    *snap_id = last_new_snap_id;

    if !quiet {
        eprintln!(
            "[watch] Flushed snapshot #{} ({} dirs)",
            last_new_snap_id,
            engine.state.len(),
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
