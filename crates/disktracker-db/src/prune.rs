use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, Connection};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct PrunePreview {
    pub snapshot_ids: Vec<i64>,
    pub snapshot_count: usize,
    pub dir_row_count: i64,
    pub delta_row_count: i64,
    pub event_row_count: i64,
    pub freed_bytes_approx: i64,
}

#[derive(Debug, Serialize)]
pub struct PruneResult {
    pub deleted_snapshots: Vec<i64>,
    pub deleted_snapshot_count: usize,
    pub deleted_dir_rows: i64,
    pub deleted_delta_rows: i64,
    pub deleted_event_rows: i64,
    pub freed_bytes_approx: i64,
    pub dry_run: bool,
}

/// Resolve which snapshot IDs should be pruned.
/// At least one snapshot (the most recent) is always preserved.
pub fn resolve_prune_candidates(
    conn: &Connection,
    keep_last: Option<usize>,
    older_than: Option<&str>,
) -> Result<Vec<i64>> {
    // Fetch all snapshots ordered newest first
    let mut stmt = conn.prepare("SELECT id, started_at FROM snapshots ORDER BY id DESC")?;
    let rows: Vec<(i64, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    if rows.is_empty() {
        return Ok(vec![]);
    }

    // Always keep at least one (the newest)
    let min_keep = keep_last.unwrap_or(1).max(1);
    let keep_ids: std::collections::HashSet<i64> =
        rows.iter().take(min_keep).map(|(id, _)| *id).collect();

    // Apply --older-than filter
    let cutoff_ts: Option<i64> = older_than.and_then(parse_duration_to_cutoff);
    let candidates: Vec<i64> = rows
        .iter()
        .filter(|(id, ts)| {
            if keep_ids.contains(id) {
                return false;
            }
            // If older_than is set, only delete if the snapshot is old enough
            if let Some(cutoff) = cutoff_ts {
                return *ts < cutoff;
            }
            // If keep_last is set, everything outside keep_ids is a candidate
            true
        })
        .map(|(id, _)| *id)
        .collect();

    Ok(candidates)
}

fn parse_duration_to_cutoff(s: &str) -> Option<i64> {
    let now = Utc::now().timestamp();
    if let Some(rest) = s.strip_suffix('d') {
        return Some(now - rest.parse::<i64>().ok()? * 86_400);
    }
    if let Some(rest) = s.strip_suffix('w') {
        return Some(now - rest.parse::<i64>().ok()? * 7 * 86_400);
    }
    if let Some(rest) = s.strip_suffix('m') {
        return Some(now - rest.parse::<i64>().ok()? * 30 * 86_400);
    }
    None
}

/// Preview what would be deleted without touching the DB.
pub fn preview_prune(conn: &Connection, ids: &[i64]) -> Result<PrunePreview> {
    if ids.is_empty() {
        return Ok(PrunePreview {
            snapshot_ids: vec![],
            snapshot_count: 0,
            dir_row_count: 0,
            delta_row_count: 0,
            event_row_count: 0,
            freed_bytes_approx: 0,
        });
    }

    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");

    let dir_row_count: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM dir_snapshots WHERE snapshot_id IN ({})",
            placeholders
        ),
        rusqlite::params_from_iter(ids.iter()),
        |r| r.get(0),
    )?;

    let freed_bytes_approx: i64 = conn.query_row(
        &format!(
            "SELECT COALESCE(SUM(total_bytes), 0) FROM snapshots WHERE id IN ({})",
            placeholders
        ),
        rusqlite::params_from_iter(ids.iter()),
        |r| r.get(0),
    )?;

    let delta_row_count: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM dir_deltas WHERE snapshot_id IN ({})",
            placeholders
        ),
        rusqlite::params_from_iter(ids.iter()),
        |r| r.get(0),
    )?;

    // fs_events aren't snapshot-scoped, so we estimate by time range
    let event_row_count: i64 = if !ids.is_empty() {
        // Get min/max started_at for the candidate snapshots
        let (min_ts, max_ts): (i64, i64) = conn.query_row(
            &format!(
                "SELECT MIN(started_at), MAX(finished_at) FROM snapshots WHERE id IN ({})",
                placeholders
            ),
            rusqlite::params_from_iter(ids.iter()),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        conn.query_row(
            "SELECT COUNT(*) FROM fs_events WHERE timestamp BETWEEN ?1 AND ?2",
            params![min_ts, max_ts],
            |r| r.get(0),
        )?
    } else {
        0
    };

    Ok(PrunePreview {
        snapshot_ids: ids.to_vec(),
        snapshot_count: ids.len(),
        dir_row_count,
        delta_row_count,
        event_row_count,
        freed_bytes_approx,
    })
}

/// Execute the prune — permanently delete the listed snapshot IDs and all
/// associated rows. Returns counts of deleted rows.
pub fn execute_prune(conn: &Connection, ids: &[i64]) -> Result<PruneResult> {
    if ids.is_empty() {
        return Ok(PruneResult {
            deleted_snapshots: vec![],
            deleted_snapshot_count: 0,
            deleted_dir_rows: 0,
            deleted_delta_rows: 0,
            deleted_event_rows: 0,
            freed_bytes_approx: 0,
            dry_run: false,
        });
    }

    let preview = preview_prune(conn, ids)?;

    // Build a comma-separated literal list of i64 IDs (safe — no user input)
    let id_list = ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");

    let tx = conn.unchecked_transaction()?;

    // diff_cache ON DELETE CASCADE fires, but be explicit in case FK pragmas are off
    tx.execute_batch(&format!(
        "DELETE FROM diff_cache    WHERE snapshot_a IN ({id}) OR snapshot_b IN ({id});
         DELETE FROM dir_deltas    WHERE snapshot_id IN ({id});
         DELETE FROM dir_snapshots WHERE snapshot_id IN ({id});
         DELETE FROM snapshots     WHERE id          IN ({id});",
        id = id_list
    ))?;

    tx.commit()?;

    // Reclaim freed pages without a full VACUUM (WAL-safe)
    conn.execute_batch("PRAGMA incremental_vacuum;")?;

    Ok(PruneResult {
        deleted_snapshots: ids.to_vec(),
        deleted_snapshot_count: ids.len(),
        deleted_dir_rows: preview.dir_row_count,
        deleted_delta_rows: preview.delta_row_count,
        deleted_event_rows: 0, // fs_events not pruned by snapshot
        freed_bytes_approx: preview.freed_bytes_approx,
        dry_run: false,
    })
}
