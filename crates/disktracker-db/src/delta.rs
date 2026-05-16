use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct DeltaRow {
    pub path: Vec<u8>,
    pub path_utf8: Option<String>,
    pub previous_bytes: Option<i64>,
    pub current_bytes: i64,
    pub delta_bytes: i64,
}

impl DeltaRow {
    pub fn new(path: Vec<u8>, previous: Option<i64>, current: i64) -> Self {
        let path_utf8 = std::str::from_utf8(&path).ok().map(str::to_owned);
        let delta = current - previous.unwrap_or(0);
        Self {
            path,
            path_utf8,
            previous_bytes: previous,
            current_bytes: current,
            delta_bytes: delta,
        }
    }
}

pub fn bulk_insert_deltas(conn: &Connection, snapshot_id: i64, deltas: &[DeltaRow]) -> Result<()> {
    if deltas.is_empty() {
        return Ok(());
    }
    let now = chrono::Utc::now().timestamp();
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO dir_deltas
             (snapshot_id, path_blob, path_utf8, previous_bytes, current_bytes, delta_bytes, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for d in deltas {
            stmt.execute(params![
                snapshot_id,
                d.path,
                d.path_utf8,
                d.previous_bytes,
                d.current_bytes,
                d.delta_bytes,
                now,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Get all deltas for a snapshot, sorted by |delta| descending.
pub fn get_deltas_for_snapshot(conn: &Connection, snapshot_id: i64) -> Result<Vec<DeltaRow>> {
    let mut stmt = conn.prepare(
        "SELECT path_blob, path_utf8, previous_bytes, current_bytes, delta_bytes
         FROM dir_deltas WHERE snapshot_id = ?1
         ORDER BY ABS(delta_bytes) DESC",
    )?;
    let rows = stmt.query_map(params![snapshot_id], |row| {
        Ok(DeltaRow {
            path: row.get(0)?,
            path_utf8: row.get(1)?,
            previous_bytes: row.get(2)?,
            current_bytes: row.get(3)?,
            delta_bytes: row.get(4)?,
        })
    })?;
    let mut result = Vec::new();
    for r in rows {
        result.push(r?);
    }
    Ok(result)
}
