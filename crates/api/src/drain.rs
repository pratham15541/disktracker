use rusqlite::Connection;
use std::time::Duration;

struct PendingMutation {
    sequence: i64,
    file_id: u64,
    parent_file_id: u64,
    name: String,
    kind: String,
    is_directory: bool,
    size_delta: i64,
    at: String,
}

/// Runs the background Drain Engine for a given volume.
/// It polls the mutation_log table and replays mutations to update the facts table.
pub async fn run_drain_engine(volume: String, startup_seq: i64) {
    println!("[DrainEngine - {}] Starting background task", volume);
    let tracker = core_types::get_volume_tracker(&volume);
    tracker
        .replaying
        .store(false, std::sync::atomic::Ordering::Relaxed);

    let mut conn = match storage::get_db_connection() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[DrainEngine - {}] Critical: failed to open DB connection: {:?}",
                volume, e
            );
            return;
        }
    };

    // Ensure a default cursor row exists in drain_state for this volume
    let _ = conn.execute(
        "INSERT OR IGNORE INTO drain_state (volume, last_sequence) VALUES (?1, ?2)",
        rusqlite::params![&volume, startup_seq],
    );

    // 1. Wait until baseline crawl completes (state transitions to Reconciling)
    loop {
        let state = { *tracker.state.lock().unwrap() };
        if state == core_types::DaemonState::Reconciling || state == core_types::DaemonState::Live {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    println!("[DrainEngine - {}] Baseline crawl complete. Replaying mutations from startup sequence {}...", volume, startup_seq);

    // 2. Reconciliation Phase: Replay all accumulated mutations until caught up.
    loop {
        let processed = match drain_batch(&mut conn, &volume) {
            Ok(count) => count,
            Err(e) => {
                eprintln!(
                    "[DrainEngine - {}] Error during reconciliation: {:?}",
                    volume, e
                );
                0
            }
        };

        if processed == 0 {
            // Caught up! Transition state to Live
            let mut state_lock = tracker.state.lock().unwrap();
            if *state_lock == core_types::DaemonState::Reconciling {
                *state_lock = core_types::DaemonState::Live;
                println!(
                    "[DrainEngine - {}] Caught up with mutation log. State is Live.",
                    volume
                );
                drop(state_lock);
                // FIX 3: Force a final commit of any staged Tantivy mutations accumulated
                // during the replay. Without this, deletes staged in the last batch are
                // not visible to readers until the first Live-mode poll (up to 500ms away),
                // causing stale search results immediately after going Live.
                let _ = crate::search::commit_search_index();
                println!("[DrainEngine - {}] Final search index commit done.", volume);
            }
            break;
        }
        // Yield briefly and keep draining
        tokio::task::yield_now().await;
    }

    // 3. Continuous Live mode
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = drain_batch(&mut conn, &volume);
    }
}

fn drain_batch(conn: &mut Connection, volume: &str) -> Result<usize, rusqlite::Error> {
    let tracker = core_types::get_volume_tracker(volume);

    // 1. Get last drained sequence cursor
    let mut last_sequence: i64 = conn
        .query_row(
            "SELECT last_sequence FROM drain_state WHERE volume = ?1",
            [volume],
            |row| row.get(0),
        )
        .unwrap_or(0);

    tracker
        .mutations_replayed
        .store(last_sequence as u64, std::sync::atomic::Ordering::Relaxed);

    // 2. Fetch up to 500 pending mutations
    let mutations: Vec<PendingMutation> = {
        let mut stmt = conn.prepare(
            "SELECT sequence, file_id, parent_file_id, name, kind, is_directory, size_delta, at
             FROM mutation_log
             WHERE volume = ?1 AND sequence > ?2
             ORDER BY sequence ASC
             LIMIT 500",
        )?;

        let x: Vec<PendingMutation> = stmt
            .query_map(rusqlite::params![volume, last_sequence], |row| {
                Ok(PendingMutation {
                    sequence: row.get(0)?,
                    file_id: row.get(1)?,
                    parent_file_id: row.get(2)?,
                    name: row.get(3)?,
                    kind: row.get(4)?,
                    is_directory: row.get(5)?,
                    size_delta: row.get(6)?,
                    at: row.get(7)?,
                })
            })?
            .flatten()
            .collect();
        x
    };

    if mutations.is_empty() {
        tracker
            .replaying
            .store(false, std::sync::atomic::Ordering::Relaxed);
        return Ok(0);
    }

    tracker
        .replaying
        .store(true, std::sync::atomic::Ordering::Relaxed);

    // 3. Process mutations in a transaction
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute("PRAGMA defer_foreign_keys = ON", [])?;
    {
        let mut insert_fact_stmt = tx.prepare_cached(
            "INSERT INTO facts (volume, file_id, parent_file_id, name, is_directory, size, created_at, modified_at, attributes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(volume, file_id) DO UPDATE SET
                parent_file_id = excluded.parent_file_id,
                name = excluded.name,
                is_directory = excluded.is_directory,
                size = excluded.size,
                modified_at = excluded.modified_at,
                attributes = excluded.attributes"
        )?;

        let mut delete_fact_stmt =
            tx.prepare_cached("DELETE FROM facts WHERE volume = ?1 AND file_id = ?2")?;

        let mut update_fact_name_stmt = tx.prepare_cached(
            "UPDATE facts SET name = ?3, parent_file_id = ?4 WHERE volume = ?1 AND file_id = ?2",
        )?;

        let mut get_previous_size_stmt =
            tx.prepare_cached("SELECT size FROM facts WHERE volume = ?1 AND file_id = ?2")?;

        let mut update_size_delta_stmt =
            tx.prepare_cached("UPDATE mutation_log SET size_delta = ?1 WHERE sequence = ?2")?;

        for m in &mutations {
            match m.kind.as_str() {
                "Created" | "Modified" => {
                    let previous_size: u64 = if !m.is_directory {
                        get_previous_size_stmt
                            .query_row(rusqlite::params![volume, m.file_id], |row| row.get(0))
                            .unwrap_or(0u64)
                    } else {
                        0
                    };

                    // UPSERT fact
                    let mut size = if m.is_directory {
                        0
                    } else {
                        m.size_delta.max(0) as u64
                    };
                    let mut attributes = 0u32;
                    if let Some((real_size, real_attrs)) =
                        platform_windows::get_file_info_by_id(volume, m.file_id)
                    {
                        if !m.is_directory {
                            size = real_size;
                        }
                        attributes = real_attrs;
                    }

                    let computed_delta = if m.is_directory {
                        0
                    } else {
                        size as i64 - previous_size as i64
                    };

                    let _ = update_size_delta_stmt
                        .execute(rusqlite::params![computed_delta, m.sequence]);

                    if let Err(e) = insert_fact_stmt.execute(rusqlite::params![
                        volume,
                        m.file_id,
                        m.parent_file_id,
                        m.name,
                        if m.is_directory { 1 } else { 0 },
                        size,
                        m.at,
                        m.at,
                        attributes,
                    ]) {
                        eprintln!(
                            "[DrainEngine - {}] DB error replaying fact {}: {:?}",
                            volume, m.file_id, e
                        );
                    }
                }
                "Deleted" => {
                    let previous_size: u64 = if !m.is_directory {
                        get_previous_size_stmt
                            .query_row(rusqlite::params![volume, m.file_id], |row| row.get(0))
                            .unwrap_or(0u64)
                    } else {
                        0
                    };

                    let computed_delta = -(previous_size as i64);
                    let _ = update_size_delta_stmt
                        .execute(rusqlite::params![computed_delta, m.sequence]);

                    if let Err(e) = delete_fact_stmt.execute(rusqlite::params![volume, m.file_id]) {
                        eprintln!(
                            "[DrainEngine - {}] DB error deleting fact {}: {:?}",
                            volume, m.file_id, e
                        );
                    }
                }
                "Renamed" => {
                    // Update name and parent path if changed
                    if let Err(e) = update_fact_name_stmt.execute(rusqlite::params![
                        volume,
                        m.file_id,
                        m.name,
                        m.parent_file_id,
                    ]) {
                        eprintln!(
                            "[DrainEngine - {}] DB error renaming fact {}: {:?}",
                            volume, m.file_id, e
                        );
                    }
                }
                _ => {}
            }
            last_sequence = m.sequence;
        }

        // Update drain_state
        tx.execute(
            "INSERT INTO drain_state (volume, last_sequence)
             VALUES (?1, ?2)
             ON CONFLICT(volume) DO UPDATE SET last_sequence = excluded.last_sequence",
            rusqlite::params![volume, last_sequence],
        )?;
    }
    tx.commit()?;

    // Sync to Tantivy search index after DB transaction succeeds
    for m in &mutations {
        match m.kind.as_str() {
            "Created" | "Modified" | "Renamed" => {
                let _ = crate::search::update_fact_in_index(conn, volume, m.file_id);
            }
            "Deleted" => {
                let _ = crate::search::delete_fact_from_index(volume, m.file_id);
            }
            _ => {}
        }
    }
    let _ = crate::search::commit_search_index();

    tracker
        .mutations_replayed
        .store(last_sequence as u64, std::sync::atomic::Ordering::Relaxed);
    Ok(mutations.len())
}
