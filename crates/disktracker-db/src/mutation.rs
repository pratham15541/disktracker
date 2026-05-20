use anyhow::Result;
use disktracker_events::{FsEvent, FsEventKind};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Type of persistent mutation occurring on the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum MutationType {
    Create = 0,
    Delete = 1,
    Modify = 2,
    Rename = 3,
}

impl MutationType {
    /// Convert raw integer to MutationType.
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Create,
            1 => Self::Delete,
            2 => Self::Modify,
            3 => Self::Rename,
            _ => Self::Modify, // Fallback safety
        }
    }

    /// String representation of mutation type.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Delete => "delete",
            Self::Modify => "modify",
            Self::Rename => "rename",
        }
    }
}

impl From<FsEventKind> for MutationType {
    fn from(kind: FsEventKind) -> Self {
        match kind {
            FsEventKind::Create => Self::Create,
            FsEventKind::Delete => Self::Delete,
            FsEventKind::Modify => Self::Modify,
            FsEventKind::Rename => Self::Rename,
            _ => Self::Modify,
        }
    }
}

/// A persistent log of mutations happening on the filesystem (for lazy reconciliation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationRecord {
    pub id: Option<i64>,
    pub timestamp: i64,
    pub mutation_type: MutationType,
    pub dev: u64,
    pub ino: u64,
    pub path_blob: Vec<u8>,
    pub old_size: Option<u64>,
    pub new_size: Option<u64>,
    pub old_path_blob: Option<Vec<u8>>,
}

/// Insert a new mutation into the mutation log.
pub fn insert_mutation(conn: &Connection, record: &MutationRecord) -> Result<i64> {
    conn.execute(
        "INSERT INTO mutation_log (timestamp, mutation_type, dev, ino, path_blob, old_size, new_size, old_path_blob)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            record.timestamp,
            record.mutation_type as u8 as i64,
            record.dev as i64,
            record.ino as i64,
            record.path_blob,
            record.old_size.map(|s| s as i64),
            record.new_size.map(|s| s as i64),
            record.old_path_blob,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Retrieve the mutation log since a given timestamp, up to a specific limit.
pub fn get_mutations_since(
    conn: &Connection,
    since_ts: i64,
    limit: usize,
) -> Result<Vec<MutationRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, mutation_type, dev, ino, path_blob, old_size, new_size, old_path_blob
         FROM mutation_log
         WHERE timestamp >= ?1
         ORDER BY id ASC
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(params![since_ts, limit as i64], |row| {
        let id: i64 = row.get(0)?;
        let timestamp: i64 = row.get(1)?;
        let mut_type_val: i64 = row.get(2)?;
        let dev: i64 = row.get(3)?;
        let ino: i64 = row.get(4)?;
        let path_blob: Vec<u8> = row.get(5)?;
        let old_size: Option<i64> = row.get(6)?;
        let new_size: Option<i64> = row.get(7)?;
        let old_path_blob: Option<Vec<u8>> = row.get(8)?;

        Ok(MutationRecord {
            id: Some(id),
            timestamp,
            mutation_type: MutationType::from_u8(mut_type_val as u8),
            dev: dev as u64,
            ino: ino as u64,
            path_blob,
            old_size: old_size.map(|s| s as u64),
            new_size: new_size.map(|s| s as u64),
            old_path_blob,
        })
    })?;

    let mut results = Vec::new();
    for r in rows {
        results.push(r?);
    }
    Ok(results)
}

/// Clear all entries from the mutation log.
pub fn clear_mutation_log(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM mutation_log", [])?;
    Ok(())
}

/// Helper function to convert raw bytes to a standard PathBuf.
pub fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).as_ref())
    }
}

/// Helper function to convert a standard Path to raw bytes.
pub fn path_to_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().into_owned().into_bytes()
    }
}

/// Retrieve device, inode, and raw size for a path (Unix-compatible fallback).
fn get_path_metadata(path: &Path) -> (u64, u64, Option<u64>) {
    if let Ok(meta) = std::fs::metadata(path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            (meta.dev(), meta.ino(), Some(meta.len()))
        }
        #[cfg(not(unix))]
        {
            (0, 0, Some(meta.len()))
        }
    } else {
        (0, 0, None)
    }
}

/// Retrieve the recursive total size and total file count of a directory on disk.
pub fn scan_dir_physical_stats(path: &Path) -> (u64, u32) {
    let mut total_size = 0;
    let mut file_count = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    let (sub_size, sub_files) = scan_dir_physical_stats(&entry.path());
                    total_size += sub_size;
                    file_count += sub_files;
                } else {
                    total_size += meta.len();
                    file_count += 1;
                }
            }
        }
    }
    (total_size, file_count)
}

/// Batch-insert watch-driven filesystem events into the persistent mutation log.
pub fn insert_mutations_batch(conn: &Connection, events: &[FsEvent]) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }

    // Retrieve the latest snapshot ID to query past size records
    let latest_snap_id: Option<i64> = conn
        .query_row("SELECT MAX(id) FROM snapshots", [], |row| {
            row.get::<_, Option<i64>>(0)
        })
        .unwrap_or(None);

    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO mutation_log (timestamp, mutation_type, dev, ino, path_blob, old_size, new_size, old_path_blob)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;

        for ev in events {
            let path = bytes_to_path(&ev.path);
            let (dev, ino, new_size) = get_path_metadata(&path);

            let new_size_resolved = if path.exists() {
                if path.is_dir() {
                    Some(scan_dir_physical_stats(&path).0)
                } else {
                    new_size
                }
            } else {
                None
            };

            let old_size = if let Some(snap_id) = latest_snap_id {
                tx.query_row(
                    "SELECT total_bytes FROM dir_snapshots WHERE path_blob = ?1 AND snapshot_id = ?2",
                    params![ev.path, snap_id],
                    |row| row.get::<_, i64>(0),
                )
                .ok()
                .map(|s| s as u64)
            } else {
                None
            };

            let mut_type = MutationType::from(ev.kind);

            stmt.execute(params![
                ev.timestamp,
                mut_type as u8 as i64,
                dev as i64,
                ino as i64,
                ev.path,
                old_size.map(|s| s as i64),
                new_size_resolved.map(|s| s as i64),
                None::<Vec<u8>>,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Performs a lazy, delta-driven reconciliation of all mutations logged since `since_ts` under a specific snapshot ID.
/// Returns the net size differential (drift bytes) successfully resolved.
pub fn lazy_reconcile(conn: &Connection, snapshot_id: i64, since_ts: i64) -> Result<i64> {
    let mutations = get_mutations_since(conn, since_ts, 100000)?;
    if mutations.is_empty() {
        return Ok(0);
    }

    let mut total_drift: i64 = 0;
    let mut reconciled_dirs = std::collections::HashSet::new();

    let tx = conn.unchecked_transaction()?;
    {
        for m in mutations {
            let path = bytes_to_path(&m.path_blob);

            // Resolve to parent directory if it's a file, or directory itself if it's a directory
            let (target_path, target_blob) = if path.exists() {
                if path.is_dir() {
                    (path, m.path_blob)
                } else {
                    let parent = path.parent().map(Path::to_path_buf).unwrap_or(path);
                    let parent_blob = path_to_bytes(&parent);
                    (parent, parent_blob)
                }
            } else {
                let parent_blob = parent_path_bytes(&m.path_blob);
                if !parent_blob.is_empty() {
                    let parent = bytes_to_path(&parent_blob);
                    (parent, parent_blob)
                } else {
                    (path, m.path_blob)
                }
            };

            // Deduplicate to avoid multiple scans of the same directory in a single run
            if !reconciled_dirs.insert(target_blob.clone()) {
                continue;
            }

            // 1. Query the previous values from the database snapshot
            let prev_record: Option<(i64, i64)> = tx
                .query_row(
                    "SELECT total_bytes, file_count FROM dir_snapshots WHERE path_blob = ?1 AND snapshot_id = ?2",
                    params![target_blob, snapshot_id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .ok();

            let (prev_size, prev_files) = prev_record.unwrap_or((0, 0));

            // 2. Determine new values
            let (current_size, current_files) = if target_path.exists() {
                if target_path.is_dir() {
                    let (s, f) = scan_dir_physical_stats(&target_path);
                    (s as i64, f as i64)
                } else {
                    let len = std::fs::metadata(&target_path)
                        .map(|meta| meta.len())
                        .unwrap_or(0);
                    (len as i64, 1)
                }
            } else {
                (0, 0)
            };

            let delta_bytes = current_size - prev_size;
            let delta_files = current_files - prev_files;

            if delta_bytes == 0 && delta_files == 0 {
                continue;
            }

            total_drift += delta_bytes;

            // 3. Update the database record
            let exists: bool = tx
                .query_row(
                    "SELECT 1 FROM dir_snapshots WHERE path_blob = ?1 AND snapshot_id = ?2",
                    params![target_blob, snapshot_id],
                    |_| Ok(true),
                )
                .unwrap_or(false);

            let now = chrono::Utc::now().timestamp();
            if exists {
                if current_size == 0 && current_files == 0 {
                    tx.execute(
                        "DELETE FROM dir_snapshots WHERE path_blob = ?1 AND snapshot_id = ?2",
                        params![target_blob, snapshot_id],
                    )?;
                } else {
                    tx.execute(
                        "UPDATE dir_snapshots SET total_bytes = ?1, file_count = ?2, mtime = ?3 WHERE path_blob = ?4 AND snapshot_id = ?5",
                        params![current_size, current_files, now, target_blob, snapshot_id],
                    )?;
                }
            } else if current_size > 0 {
                let utf8 = std::str::from_utf8(&target_blob).ok().map(|s| s.to_owned());
                #[cfg(windows)]
                let depth = target_blob.iter().filter(|&&b| b == b'\\').count() as i64;
                #[cfg(not(windows))]
                let depth = target_blob.iter().filter(|&&b| b == b'/').count() as i64;

                tx.execute(
                    "INSERT INTO dir_snapshots (snapshot_id, path_blob, path_utf8, depth, total_bytes, file_count, mtime, dev, ino)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        snapshot_id,
                        target_blob,
                        utf8,
                        depth,
                        current_size,
                        current_files,
                        now,
                        0,
                        0,
                    ],
                )?;
            }

            // 4. Propagate net size and file count changes up the parent chain
            propagate_reconcile_delta(&tx, snapshot_id, &target_blob, delta_bytes, delta_files)?;
        }
    }
    tx.commit()?;

    // Update snapshots parent record aggregate size
    conn.execute(
        "UPDATE snapshots SET total_bytes = total_bytes + ?1 WHERE id = ?2",
        params![total_drift, snapshot_id],
    )?;

    Ok(total_drift)
}

fn propagate_reconcile_delta(
    tx: &rusqlite::Transaction,
    snapshot_id: i64,
    path_bytes: &[u8],
    delta_bytes: i64,
    delta_files: i64,
) -> Result<()> {
    let mut current = path_bytes.to_vec();
    loop {
        let parent = parent_path_bytes(&current);
        if parent.is_empty() || parent == current {
            break;
        }

        tx.execute(
            "UPDATE dir_snapshots
             SET total_bytes = total_bytes + ?1,
                 file_count = file_count + ?2
             WHERE path_blob = ?3 AND snapshot_id = ?4",
            params![delta_bytes, delta_files, parent, snapshot_id],
        )?;

        current = parent;
    }
    Ok(())
}

fn parent_path_bytes(path: &[u8]) -> Vec<u8> {
    #[cfg(windows)]
    const SEP: u8 = b'\\';
    #[cfg(not(windows))]
    const SEP: u8 = b'/';

    let trimmed = path.strip_suffix(&[SEP]).unwrap_or(path);
    if let Some(pos) = trimmed.iter().rposition(|&b| b == SEP) {
        if pos == 0 {
            return vec![SEP];
        }
        trimmed[..pos].to_vec()
    } else {
        vec![]
    }
}
