use crate::schema::{SCHEMA, WAL_PRAGMAS};
use anyhow::{Context, Result};
use disktracker_core::arena::PathlessArena;
use disktracker_core::identity::FsIdentity;
use disktracker_core::warm::{CachedDir, SnapshotIndex};
use rusqlite::{params, Connection};
use std::path::Path;

pub fn open_db(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Cannot create DB directory {:?}", parent))?;
    }
    let conn =
        Connection::open(db_path).with_context(|| format!("Cannot open database {:?}", db_path))?;
    conn.execute_batch(WAL_PRAGMAS)?;
    // Dynamically migrate existing databases by adding columns if they don't exist.
    // SQLite's ALTER TABLE ADD COLUMN is fast and idempotent if we catch and ignore errors.
    let _ = conn.execute(
        "ALTER TABLE dir_snapshots ADD COLUMN dev INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE dir_snapshots ADD COLUMN ino INTEGER NOT NULL DEFAULT 0",
        [],
    );
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

#[derive(Debug)]
pub struct SnapshotRecord {
    pub id: i64,
    pub scan_root: String,
    pub started_at: i64,
    pub finished_at: i64,
    pub total_files: i64,
    pub total_bytes: i64,
    pub error_count: i64,
    pub host: String,
}

pub fn insert_snapshot(
    conn: &Connection,
    scan_root: &str,
    started_at: i64,
    finished_at: i64,
    total_files: u64,
    total_bytes: u64,
    error_count: u32,
) -> Result<i64> {
    let host = hostname::get()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    conn.execute(
        "INSERT INTO snapshots (scan_root, started_at, finished_at, total_files, total_bytes, error_count, host)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![scan_root, started_at, finished_at, total_files as i64, total_bytes as i64, error_count as i64, host],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn bulk_insert_dirs(conn: &Connection, snapshot_id: i64, arena: &PathlessArena) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO dir_snapshots
             (snapshot_id, path_blob, path_utf8, depth, total_bytes, file_count, mtime, dev, ino)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        for idx in 0..arena.node_count() {
            let hot = &arena.hot[idx];
            let cold = &arena.cold[idx];
            let raw_path = arena.materialize_path(idx as u32);
            let utf8_path = std::str::from_utf8(&raw_path).ok().map(str::to_owned);
            stmt.execute(params![
                snapshot_id,
                raw_path,
                utf8_path,
                hot.depth as i64,
                hot.total_bytes as i64,
                hot.file_count as i64,
                cold.mtime,
                cold.identity.dev as i64,
                cold.identity.ino as i64,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn list_snapshots(conn: &Connection) -> Result<Vec<SnapshotRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, scan_root, started_at, finished_at, total_files, total_bytes, error_count, host
         FROM snapshots ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(SnapshotRecord {
            id: row.get(0)?,
            scan_root: row.get(1)?,
            started_at: row.get(2)?,
            finished_at: row.get(3)?,
            total_files: row.get(4)?,
            total_bytes: row.get(5)?,
            error_count: row.get(6)?,
            host: row.get(7)?,
        })
    })?;
    let mut result = Vec::new();
    for r in rows {
        result.push(r?);
    }
    Ok(result)
}

#[allow(dead_code)]
pub fn get_snapshot(conn: &Connection, id: i64) -> Result<Option<SnapshotRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, scan_root, started_at, finished_at, total_files, total_bytes, error_count, host
         FROM snapshots WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(params![id], |row| {
        Ok(SnapshotRecord {
            id: row.get(0)?,
            scan_root: row.get(1)?,
            started_at: row.get(2)?,
            finished_at: row.get(3)?,
            total_files: row.get(4)?,
            total_bytes: row.get(5)?,
            error_count: row.get(6)?,
            host: row.get(7)?,
        })
    })?;
    Ok(rows.next().transpose()?)
}

pub fn get_latest_snapshot_id(conn: &Connection) -> Result<Option<i64>> {
    let mut stmt = conn.prepare("SELECT MAX(id) FROM snapshots")?;
    let id: Option<i64> = stmt.query_row([], |row| row.get(0))?;
    Ok(id)
}

pub fn get_latest_snapshot_for_root(conn: &Connection, root: &str) -> Result<Option<i64>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM snapshots
         WHERE scan_root = ?1
         ORDER BY id DESC LIMIT 1",
    )?;
    let id: Option<i64> = stmt.query_row(params![root], |row| row.get(0)).ok();
    Ok(id)
}

pub fn resolve_snapshot_ref(conn: &Connection, s: &str) -> Result<i64> {
    if let Ok(id) = s.parse::<i64>() {
        return Ok(id);
    }
    if let Some(ts) = parse_relative_duration(s) {
        let mut stmt = conn.prepare(
            "SELECT id FROM snapshots WHERE started_at >= ?1 ORDER BY started_at ASC LIMIT 1",
        )?;
        let id: Option<i64> = stmt.query_row(params![ts], |row| row.get(0)).ok();
        return id.ok_or_else(|| anyhow::anyhow!("No snapshot found for duration '{}'", s));
    }
    if let Ok(dt) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let ts = dt.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        let mut stmt = conn.prepare(
            "SELECT id FROM snapshots WHERE started_at >= ?1 ORDER BY started_at ASC LIMIT 1",
        )?;
        let id: Option<i64> = stmt.query_row(params![ts], |row| row.get(0)).ok();
        return id.ok_or_else(|| anyhow::anyhow!("No snapshot found for date '{}'", s));
    }
    anyhow::bail!("Cannot parse snapshot reference '{}'", s)
}

fn parse_relative_duration(s: &str) -> Option<i64> {
    let now = chrono::Utc::now().timestamp();
    if let Some(rest) = s.strip_suffix('d') {
        return Some(now - rest.parse::<i64>().ok()? * 86400);
    }
    if let Some(rest) = s.strip_suffix('w') {
        return Some(now - rest.parse::<i64>().ok()? * 7 * 86400);
    }
    if let Some(rest) = s.strip_suffix('m') {
        return Some(now - rest.parse::<i64>().ok()? * 30 * 86400);
    }
    None
}

pub fn clear_diff_cache_for(conn: &Connection, snapshot_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM diff_cache WHERE snapshot_a = ?1 OR snapshot_b = ?1",
        params![snapshot_id],
    )?;
    Ok(())
}

pub fn rollback_snapshot(conn: &Connection, snapshot_id: i64) -> Result<()> {
    clear_diff_cache_for(conn, snapshot_id)?;
    conn.execute(
        "DELETE FROM dir_snapshots WHERE snapshot_id = ?1",
        params![snapshot_id],
    )?;
    conn.execute("DELETE FROM snapshots WHERE id = ?1", params![snapshot_id])?;
    Ok(())
}

pub fn load_snapshot_index(conn: &Connection, snapshot_id: i64) -> Result<SnapshotIndex> {
    let mut stmt = conn.prepare(
        "SELECT path_blob, total_bytes, file_count, mtime, dev, ino
         FROM dir_snapshots
         WHERE snapshot_id = ?1",
    )?;
    let mut index = SnapshotIndex::new();
    let rows = stmt.query_map(params![snapshot_id], |row| {
        let path_blob: Vec<u8> = row.get(0)?;
        let total_bytes: i64 = row.get(1)?;
        let file_count: i64 = row.get(2)?;
        let mtime: i64 = row.get(3)?;
        let dev: i64 = row.get(4)?;
        let ino: i64 = row.get(5)?;
        Ok((
            path_blob,
            CachedDir {
                total_bytes: total_bytes as u64,
                file_count: file_count as u32,
                mtime,
            },
            FsIdentity {
                dev: dev as u64,
                ino: ino as u64,
            },
        ))
    })?;

    for row_res in rows {
        let (path_blob, cached, identity) = row_res?;
        if identity.is_known() {
            index.insert_identity(identity, cached.clone());
        }
        index.insert_path(path_blob, cached);
    }

    Ok(index)
}
