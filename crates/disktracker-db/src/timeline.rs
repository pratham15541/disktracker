use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct TimelineEntry {
    pub snapshot_id: i64,
    pub timestamp: i64,
    pub total_bytes: i64,
}

/// Query the size of `path` across all snapshots, ordered by time.
pub fn query_timeline(conn: &Connection, path: &str) -> Result<Vec<TimelineEntry>> {
    // Match exact path or path with trailing separator for subtree prefix
    let _pattern = format!("{}%", path);
    let mut stmt = conn.prepare(
        r#"
        SELECT s.id, s.started_at, ds.total_bytes
        FROM dir_snapshots ds
        JOIN snapshots s ON s.id = ds.snapshot_id
        WHERE ds.path_utf8 = ?1
           OR ds.path_blob = ?2
        ORDER BY s.started_at ASC
        "#,
    )?;
    let path_blob = path.as_bytes().to_vec();
    let rows = stmt.query_map(params![path, path_blob], |row| {
        Ok(TimelineEntry {
            snapshot_id: row.get(0)?,
            timestamp: row.get(1)?,
            total_bytes: row.get(2)?,
        })
    })?;
    let mut result = Vec::new();
    for r in rows {
        result.push(r?);
    }
    Ok(result)
}

/// Query timeline from dir_deltas (for watch mode where full snapshots may be sparse).
pub fn query_timeline_from_deltas(conn: &Connection, path: &str) -> Result<Vec<TimelineEntry>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT -1 AS snapshot_id, recorded_at, current_bytes
        FROM dir_deltas
        WHERE path_utf8 = ?1 OR path_blob = ?2
        ORDER BY recorded_at ASC
        "#,
    )?;
    let path_blob = path.as_bytes().to_vec();
    let rows = stmt.query_map(params![path, path_blob], |row| {
        Ok(TimelineEntry {
            snapshot_id: row.get(0)?,
            timestamp: row.get(1)?,
            total_bytes: row.get(2)?,
        })
    })?;
    let mut result = Vec::new();
    for r in rows {
        result.push(r?);
    }
    Ok(result)
}
