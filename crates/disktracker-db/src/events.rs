use anyhow::Result;
use disktracker_events::{FsEvent, FsEventKind};
use rusqlite::{params, Connection};

pub fn insert_fs_event(conn: &Connection, event: &FsEvent) -> Result<()> {
    conn.execute(
        "INSERT INTO fs_events (timestamp, event_type, path_blob, is_dir)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            event.timestamp,
            event.kind as u8 as i64,
            event.path,
            event.is_dir as i64,
        ],
    )?;
    Ok(())
}

pub fn insert_fs_events_batch(conn: &Connection, events: &[FsEvent]) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO fs_events (timestamp, event_type, path_blob, is_dir)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for ev in events {
            stmt.execute(params![
                ev.timestamp,
                ev.kind as u8 as i64,
                ev.path,
                ev.is_dir as i64,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub struct FsEventRecord {
    pub id: i64,
    pub timestamp: i64,
    pub kind: FsEventKind,
    pub path: Vec<u8>,
    pub is_dir: bool,
}

pub fn get_recent_events(
    conn: &Connection,
    since_ts: i64,
    limit: usize,
) -> Result<Vec<FsEventRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, event_type, path_blob, is_dir
         FROM fs_events WHERE timestamp >= ?1
         ORDER BY timestamp DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![since_ts, limit as i64], |row| {
        Ok(FsEventRecord {
            id: row.get(0)?,
            timestamp: row.get(1)?,
            kind: FsEventKind::from_u8(row.get::<_, i64>(2)? as u8),
            path: row.get(3)?,
            is_dir: row.get::<_, i64>(4)? != 0,
        })
    })?;
    let mut result = Vec::new();
    for r in rows {
        result.push(r?);
    }
    Ok(result)
}

pub fn get_event_count_since(conn: &Connection, since_ts: i64) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM fs_events WHERE timestamp >= ?1",
        params![since_ts],
        |row| row.get(0),
    )
    .map_err(Into::into)
}
