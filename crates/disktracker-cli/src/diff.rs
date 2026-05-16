use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct DiffEntry {
    pub path: String,
    pub bytes_a: Option<i64>,
    pub bytes_b: Option<i64>,
    pub delta_bytes: i64,
}

pub struct DiffResult {
    pub snapshot_a: i64,
    pub snapshot_b: i64,
    pub started_a: i64,
    pub started_b: i64,
    pub entries: Vec<DiffEntry>,
    pub net_change: i64,
}

/// Compute or retrieve a cached diff between two snapshots.
pub fn compute_diff(
    conn: &Connection,
    snapshot_a: i64,
    snapshot_b: i64,
    top: usize,
    min_delta_bytes: i64,
) -> Result<DiffResult> {
    let (started_a, started_b) = get_snapshot_times(conn, snapshot_a, snapshot_b)?;

    let cached = load_from_cache(conn, snapshot_a, snapshot_b, top, min_delta_bytes)?;
    if !cached.is_empty() {
        let net_change = cached.iter().map(|e| e.delta_bytes).sum();
        return Ok(DiffResult {
            snapshot_a,
            snapshot_b,
            started_a,
            started_b,
            entries: cached,
            net_change,
        });
    }

    populate_diff_cache(conn, snapshot_a, snapshot_b)?;

    let entries = load_from_cache(conn, snapshot_a, snapshot_b, top, min_delta_bytes)?;
    let net_change = entries.iter().map(|e| e.delta_bytes).sum();
    Ok(DiffResult {
        snapshot_a,
        snapshot_b,
        started_a,
        started_b,
        entries,
        net_change,
    })
}

fn get_snapshot_times(conn: &Connection, a: i64, b: i64) -> Result<(i64, i64)> {
    let ts_a: i64 = conn.query_row(
        "SELECT started_at FROM snapshots WHERE id = ?1",
        params![a],
        |row| row.get(0),
    )?;
    let ts_b: i64 = conn.query_row(
        "SELECT started_at FROM snapshots WHERE id = ?1",
        params![b],
        |row| row.get(0),
    )?;
    Ok((ts_a, ts_b))
}

fn populate_diff_cache(conn: &Connection, snapshot_a: i64, snapshot_b: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM diff_cache WHERE snapshot_a = ?1 AND snapshot_b = ?2",
        params![snapshot_a, snapshot_b],
    )?;
    conn.execute_batch(&format!(
        r#"
        INSERT INTO diff_cache (snapshot_a, snapshot_b, path_blob, bytes_a, bytes_b, delta_bytes)
        SELECT {sa},{sb}, path_blob, bytes_a, bytes_b, delta_bytes FROM (
            SELECT path_blob,
                SUM(CASE WHEN which='A' THEN total_bytes ELSE NULL END) AS bytes_a,
                SUM(CASE WHEN which='B' THEN total_bytes ELSE NULL END) AS bytes_b,
                COALESCE(SUM(CASE WHEN which='B' THEN total_bytes ELSE 0 END),0)
                - COALESCE(SUM(CASE WHEN which='A' THEN total_bytes ELSE 0 END),0) AS delta_bytes
            FROM (
                SELECT path_blob, total_bytes, 'A' AS which FROM dir_snapshots WHERE snapshot_id={sa}
                UNION ALL
                SELECT path_blob, total_bytes, 'B' AS which FROM dir_snapshots WHERE snapshot_id={sb}
            ) combined
            GROUP BY path_blob
        ) computed WHERE delta_bytes != 0;
        "#,
        sa = snapshot_a, sb = snapshot_b
    ))?;
    Ok(())
}

fn load_from_cache(
    conn: &Connection,
    snapshot_a: i64,
    snapshot_b: i64,
    top: usize,
    min_delta_bytes: i64,
) -> Result<Vec<DiffEntry>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT COALESCE(ds.path_utf8, CAST(dc.path_blob AS TEXT)),
               dc.bytes_a, dc.bytes_b, dc.delta_bytes
        FROM diff_cache dc
        LEFT JOIN dir_snapshots ds
            ON ds.path_blob = dc.path_blob AND ds.snapshot_id = ?2
        WHERE dc.snapshot_a = ?1 AND dc.snapshot_b = ?2
          AND ABS(dc.delta_bytes) >= ?3
        ORDER BY dc.delta_bytes DESC
        LIMIT ?4
        "#,
    )?;
    let rows = stmt.query_map(
        params![snapshot_a, snapshot_b, min_delta_bytes, (top as i64) * 2],
        |row| {
            Ok(DiffEntry {
                path: row
                    .get::<_, Option<String>>(0)?
                    .unwrap_or_else(|| "<non-utf8>".to_owned()),
                bytes_a: row.get(1)?,
                bytes_b: row.get(2)?,
                delta_bytes: row.get(3)?,
            })
        },
    )?;
    let mut entries: Vec<DiffEntry> = rows.filter_map(|r| r.ok()).collect();
    entries.sort_by_key(|e| -e.delta_bytes.abs());
    entries.truncate(top);
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{bulk_insert_dirs, insert_snapshot, open_db};
    use disktracker_core::arena::{DirNode, PathlessArena};

    fn make_arena_v1() -> PathlessArena {
        let mut arena = PathlessArena::with_capacity(8, 256);
        let r = arena.intern(b"/");
        let root_idx = arena.push(DirNode {
            parent: None,
            name: r,
            total_bytes: 1000,
            file_count: 5,
            mtime: 0,
            depth: 0,
        });
        let s = arena.intern(b"var");
        arena.push(DirNode {
            parent: PathlessArena::encode_parent(root_idx),
            name: s,
            total_bytes: 200,
            file_count: 2,
            mtime: 0,
            depth: 1,
        });
        arena
    }

    fn make_arena_v2() -> PathlessArena {
        let mut arena = PathlessArena::with_capacity(8, 256);
        let r = arena.intern(b"/");
        let root_idx = arena.push(DirNode {
            parent: None,
            name: r,
            total_bytes: 2000,
            file_count: 8,
            mtime: 0,
            depth: 0,
        });
        let s = arena.intern(b"var");
        arena.push(DirNode {
            parent: PathlessArena::encode_parent(root_idx),
            name: s,
            total_bytes: 1200,
            file_count: 6,
            mtime: 0,
            depth: 1,
        });
        arena
    }

    #[test]
    fn test_diff_logic() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db_path = dir.path().join("test.db");
        let conn = open_db(&db_path)?;
        let now = chrono::Utc::now().timestamp();
        let snap_a = insert_snapshot(&conn, "/", now, now + 1, 5, 1000, 0)?;
        bulk_insert_dirs(&conn, snap_a, &make_arena_v1())?;
        let snap_b = insert_snapshot(&conn, "/", now + 86400, now + 86401, 8, 2000, 0)?;
        bulk_insert_dirs(&conn, snap_b, &make_arena_v2())?;
        let result = compute_diff(&conn, snap_a, snap_b, 20, 0)?;
        assert!(!result.entries.is_empty());
        let var_entry = result.entries.iter().find(|e| e.path.contains("var"));
        assert!(var_entry.is_some());
        assert_eq!(var_entry.unwrap().delta_bytes, 1000);
        Ok(())
    }
}
