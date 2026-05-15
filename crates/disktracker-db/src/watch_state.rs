use anyhow::Result;
use rusqlite::{params, Connection};

pub struct WatchState {
    pub watch_root: Vec<u8>,
    pub last_event_time: Option<i64>,
    pub last_reconcile_time: Option<i64>,
    pub last_snapshot_id: Option<i64>,
}

pub fn upsert_watch_state(conn: &Connection, state: &WatchState) -> Result<()> {
    conn.execute(
        "INSERT INTO watch_state (id, watch_root, last_event_time, last_reconcile_time, last_snapshot_id)
         VALUES (1, ?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET
             watch_root = excluded.watch_root,
             last_event_time = excluded.last_event_time,
             last_reconcile_time = excluded.last_reconcile_time,
             last_snapshot_id = excluded.last_snapshot_id",
        params![
            state.watch_root,
            state.last_event_time,
            state.last_reconcile_time,
            state.last_snapshot_id,
        ],
    )?;
    Ok(())
}

pub fn get_watch_state(conn: &Connection) -> Result<Option<WatchState>> {
    let mut stmt = conn.prepare(
        "SELECT watch_root, last_event_time, last_reconcile_time, last_snapshot_id
         FROM watch_state WHERE id = 1",
    )?;
    let mut rows = stmt.query_map([], |row| {
        Ok(WatchState {
            watch_root: row.get(0)?,
            last_event_time: row.get(1)?,
            last_reconcile_time: row.get(2)?,
            last_snapshot_id: row.get(3)?,
        })
    })?;
    Ok(rows.next().transpose()?)
}

pub fn touch_event_time(conn: &Connection, ts: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO watch_state (id, watch_root, last_event_time)
         VALUES (1, '', ?1)
         ON CONFLICT(id) DO UPDATE SET last_event_time = excluded.last_event_time",
        params![ts],
    )?;
    Ok(())
}

pub fn touch_reconcile_time(conn: &Connection, ts: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO watch_state (id, watch_root, last_reconcile_time)
         VALUES (1, '', ?1)
         ON CONFLICT(id) DO UPDATE SET last_reconcile_time = excluded.last_reconcile_time",
        params![ts],
    )?;
    Ok(())
}
