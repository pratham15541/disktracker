use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use rusqlite::Connection;
use chrono::{DateTime, Utc};
use storage::{SnapshotEntry, get_db_connection, check_snapshot_label_exists, get_snapshot_by_label_or_id, list_snapshots_db, create_snapshot_db};
use crate::search;

#[derive(Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub completed: bool,
    pub progress: u32,
    pub result: Option<Value>,
    pub error: Option<String>,
}

pub fn get_jobs() -> &'static Mutex<HashMap<String, Job>> {
    static JOBS: OnceLock<Mutex<HashMap<String, Job>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn format_relative_age(created_at: &str) -> String {
    if let Ok(dt) = DateTime::parse_from_rfc3339(created_at) {
        let now = Utc::now();
        let diff = now.signed_duration_since(dt.with_timezone(&Utc));
        if diff.num_seconds() < 60 {
            "just now".to_string()
        } else if diff.num_minutes() < 60 {
            format!("{} minutes ago", diff.num_minutes())
        } else if diff.num_hours() < 24 {
            format!("{} hours ago", diff.num_hours())
        } else {
            format!("{} days ago", diff.num_days())
        }
    } else {
        "some time ago".to_string()
    }
}

pub fn generate_random_id(prefix: &str) -> String {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}_{:x}", prefix, seed)
}

/// JSON-RPC: snapshot_create
pub fn handle_snapshot_create(params: Value) -> Result<Value, String> {
    let volume = params.get("volume").and_then(|v| v.as_str()).ok_or("Missing volume parameter")?.to_uppercase();
    let label_opt = params.get("label").and_then(|l| l.as_str()).map(|s| s.trim().to_string());

    // Verify volume exists/is registered
    let registered = core_types::get_registered_volumes();
    if !registered.contains(&volume) {
        return Err(format!("Volume {} is not registered.", volume));
    }

    if let Some(ref label) = label_opt {
        if label.is_empty() {
            return Err("Label cannot be empty".to_string());
        }
        // Synchronously check label uniqueness
        if let Ok(Some(existing)) = check_snapshot_label_exists(&volume, label) {
            let age_str = format_relative_age(&existing.created_at);
            return Err(format!(
                "E_INVALID_PARAMS: A snapshot named \"{}\" already exists (created {}, id {}). Choose a different label, or omit --label to get an auto-generated one.",
                label, age_str, existing.id
            ));
        }
    }

    let job_id = generate_random_id("job");
    {
        let mut jobs = get_jobs().lock().unwrap();
        jobs.insert(job_id.clone(), Job {
            id: job_id.clone(),
            completed: false,
            progress: 0,
            result: None,
            error: None,
        });
    }

    // Spawn background task to create the snapshot
    let job_id_clone = job_id.clone();
    let volume_clone = volume.clone();
    let label_clone = label_opt.clone();
    tokio::spawn(async move {
        // Run database queries on a blocking thread
        let res = tokio::task::spawn_blocking(move || -> Result<SnapshotEntry, String> {
            let conn = get_db_connection().map_err(|e| e.to_string())?;
            
            // Get current max sequence number
            let seq: i64 = conn.query_row(
                "SELECT COALESCE(MAX(sequence), 0) FROM mutation_log WHERE volume = ?1",
                rusqlite::params![volume_clone],
                |row| row.get(0)
            ).unwrap_or(0);

            // Get facts count
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM facts WHERE volume = ?1",
                rusqlite::params![volume_clone],
                |row| row.get(0)
            ).unwrap_or(0);

            let snapshot_id = generate_random_id("snap");
            let final_label = label_clone.unwrap_or_else(|| {
                let local_time = chrono::Local::now();
                format!("snap_{}", local_time.format("%Y%m%d_%H%M%S"))
            });

            let daemon_ver = env!("CARGO_PKG_VERSION");
            let schema_ver = 2i64; // current schema version
            let config = config_mgr::load_config();
            let retention = config.retention;

            create_snapshot_db(
                &snapshot_id,
                &final_label,
                &volume_clone,
                seq,
                daemon_ver,
                schema_ver,
                &retention,
                count,
            ).map_err(|e| e.to_string())?;

            Ok(SnapshotEntry {
                id: snapshot_id,
                label: final_label,
                volume: volume_clone,
                sequence_number: seq,
                created_at: Utc::now().to_rfc3339(),
                daemon_version: daemon_ver.to_string(),
                schema_version: schema_ver,
                retention_setting: retention,
                facts_count: count,
            })
        }).await;

        let mut jobs = get_jobs().lock().unwrap();
        if let Some(job) = jobs.get_mut(&job_id_clone) {
            job.completed = true;
            job.progress = 100;
            match res {
                Ok(Ok(snap)) => {
                    job.result = Some(serde_json::to_value(snap).unwrap());
                }
                Ok(Err(e)) => {
                    job.error = Some(e);
                }
                Err(e) => {
                    job.error = Some(format!("Task panic: {}", e));
                }
            }
        }
    });

    Ok(serde_json::json!({ "job_id": job_id }))
}

/// JSON-RPC: job.completed
pub fn handle_job_completed(params: Value) -> Result<Value, String> {
    let job_id = params.get("job_id").and_then(|j| j.as_str()).ok_or("Missing job_id parameter")?;
    let jobs = get_jobs().lock().unwrap();
    if let Some(job) = jobs.get(job_id) {
        Ok(serde_json::to_value(job).unwrap())
    } else {
        Err("E_NOT_FOUND: Job not found".to_string())
    }
}

/// JSON-RPC: snapshot_list
pub fn handle_snapshot_list(params: Value) -> Result<Value, String> {
    let volume = params.get("volume").and_then(|v| v.as_str()).map(|s| s.to_uppercase());
    let limit = params.get("limit").and_then(|l| l.as_u64()).unwrap_or(20) as usize;
    let cursor = params.get("cursor").and_then(|c| c.as_str()).unwrap_or("");

    let mut cursor_seq = None;
    let mut is_backward = false;
    if !cursor.is_empty() {
        let parts: Vec<&str> = cursor.split(':').collect();
        if parts.len() == 2 {
            if let Ok(seq) = parts[1].parse::<i64>() {
                cursor_seq = Some(seq);
                is_backward = parts[0] == "b";
            }
        }
    }

    let mut list = list_snapshots_db(
        volume.as_deref(),
        limit,
        cursor_seq,
        is_backward,
    ).map_err(|e| e.to_string())?;

    let has_more = list.len() > limit;
    if has_more {
        if is_backward {
            list.remove(0);
        } else {
            list.pop();
        }
    }

    let next_cursor = if has_more && !is_backward {
        list.last().map(|s| format!("f:{}", s.sequence_number))
    } else if !cursor.is_empty() && is_backward {
        list.last().map(|s| format!("f:{}", s.sequence_number))
    } else {
        None
    };

    let prev_cursor = if !cursor.is_empty() && !is_backward {
        list.first().map(|s| format!("b:{}", s.sequence_number))
    } else if has_more && is_backward {
        list.first().map(|s| format!("b:{}", s.sequence_number))
    } else {
        None
    };

    Ok(serde_json::json!({
        "results": list,
        "next_cursor": next_cursor,
        "prev_cursor": prev_cursor,
    }))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffFile {
    pub file_id: u64,
    pub parent_file_id: u64,
    pub name: String,
    pub is_directory: bool,
    pub size_delta: i64,
    pub kind: String, // "Created", "Deleted", "Modified", "Renamed"
    pub old_name: Option<String>,
    pub old_parent_file_id: Option<u64>,
    pub path: String,
}

fn resolve_diff_file_path(
    conn: &Connection,
    volume: &str,
    file_id: u64,
    name: &str,
    parent_file_id: u64,
) -> String {
    if let Ok(path) = search::get_fact_path(conn, volume, parent_file_id) {
        if path.is_empty() {
            return format!("{}/{}", volume, name);
        } else {
            return format!("{}/{}/{}", volume, path, name);
        }
    }
    let mut current_parent = parent_file_id;
    let mut parts = vec![name.to_string()];
    let mut visited = std::collections::HashSet::new();
    visited.insert(file_id);

    while current_parent != 0 && visited.insert(current_parent) {
        let parent_info: Result<(String, u64), rusqlite::Error> = conn.query_row(
            "SELECT name, parent_file_id FROM mutation_log WHERE volume = ?1 AND file_id = ?2 ORDER BY sequence DESC LIMIT 1",
            rusqlite::params![volume, current_parent],
            |row| Ok((row.get(0)?, row.get(1)?))
        );
        if let Ok((p_name, p_parent)) = parent_info {
            parts.push(p_name);
            current_parent = p_parent;
        } else {
            if let Ok(p_path) = search::get_fact_path(conn, volume, current_parent) {
                if !p_path.is_empty() {
                    parts.push(p_path);
                }
            }
            break;
        }
    }
    parts.reverse();
    format!("{}/{}", volume, parts.join("/"))
}

/// JSON-RPC: snapshot_diff
pub fn handle_snapshot_diff(params: Value) -> Result<Value, String> {
    let volume = params.get("volume").and_then(|v| v.as_str()).map(|s| s.to_uppercase());
    let snapshot_a_input = params.get("snapshot_a").and_then(|s| s.as_str()).ok_or("Missing snapshot_a parameter")?;
    let snapshot_b_input = params.get("snapshot_b").and_then(|s| s.as_str()).ok_or("Missing snapshot_b parameter")?;
    let path_filter = params.get("path_filter").and_then(|p| p.as_str()).map(|s| s.replace('\\', "/"));

    let conn = get_db_connection().map_err(|e| e.to_string())?;

    // If volume is not provided, we try to resolve from snapshots, but if they belong to different volumes or aren't found, it's an error.
    let vol_to_use = match volume {
        Some(v) => v,
        None => {
            // Find volume of snapshot_a
            let row_a: Result<String, rusqlite::Error> = conn.query_row(
                "SELECT volume FROM snapshots WHERE id = ?1 OR label = ?1 LIMIT 1",
                rusqlite::params![snapshot_a_input],
                |row| row.get(0)
            );
            row_a.map_err(|_| format!("E_NOT_FOUND: Couldn't find snapshot \"{}\". Check the path/ID and try again.", snapshot_a_input))?
        }
    };

    let snap_a = get_snapshot_by_label_or_id(&vol_to_use, snapshot_a_input)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("E_NOT_FOUND: Couldn't find snapshot \"{}\". Check the path/ID and try again.", snapshot_a_input))?;

    let snap_b = get_snapshot_by_label_or_id(&vol_to_use, snapshot_b_input)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("E_NOT_FOUND: Couldn't find snapshot \"{}\". Check the path/ID and try again.", snapshot_b_input))?;

    if snap_a.volume != snap_b.volume {
        return Err("Snapshots are on different volumes.".to_string());
    }

    let (start_snap, end_snap) = if snap_a.sequence_number <= snap_b.sequence_number {
        (snap_a, snap_b)
    } else {
        (snap_b, snap_a)
    };

    let results = calculate_diff(&conn, &start_snap, &end_snap, path_filter)?;

    Ok(serde_json::json!({
        "volume": start_snap.volume,
        "snapshot_a": start_snap.label,
        "snapshot_b": end_snap.label,
        "results": results,
    }))
}

pub fn calculate_diff(
    conn: &Connection,
    start_snap: &SnapshotEntry,
    end_snap: &SnapshotEntry,
    path_filter: Option<String>,
) -> Result<Vec<DiffFile>, String> {
    // Check if start snapshot sequence is older than the oldest mutation log entry
    let min_seq: i64 = conn.query_row(
        "SELECT COALESCE(MIN(sequence), 0) FROM mutation_log WHERE volume = ?1",
        rusqlite::params![start_snap.volume],
        |row| row.get(0)
    ).unwrap_or(0);

    // If start_snap.sequence_number is less than min_seq, it means history is pruned!
    // UNLESS the min_seq is 0 (empty log) or start_snap.sequence_number matches or exceeds it.
    if start_snap.sequence_number < min_seq && min_seq > 1 {
        let config = config_mgr::load_config();
        return Err(format!(
            "E_SNAPSHOT_DATA_EXPIRED: This comparison goes further back than DiskTracker currently keeps ({}). Try a more recent snapshot pair.",
            config.retention
        ));
    }

    // Query all mutations between the two snapshot bookmarks
    let mut stmt = conn.prepare(
        "SELECT sequence, file_id, parent_file_id, name, kind, is_directory, size_delta
         FROM mutation_log
         WHERE volume = ?1 AND sequence > ?2 AND sequence <= ?3
         ORDER BY sequence ASC"
    ).map_err(|e| e.to_string())?;

    let mut rows = stmt.query(rusqlite::params![
        start_snap.volume,
        start_snap.sequence_number,
        end_snap.sequence_number
    ]).map_err(|e| e.to_string())?;

    // In-memory reduction map
    let mut diff_map: HashMap<u64, DiffFile> = HashMap::new();

    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let seq: i64 = row.get(0).map_err(|e| e.to_string())?;
        let file_id: u64 = row.get(1).map_err(|e| e.to_string())?;
        let parent_file_id: u64 = row.get(2).map_err(|e| e.to_string())?;
        let name: String = row.get(3).map_err(|e| e.to_string())?;
        let kind: String = row.get(4).map_err(|e| e.to_string())?;
        let is_dir: bool = row.get::<_, i64>(5).unwrap_or(0) != 0;
        let size_delta: i64 = row.get(6).map_err(|e| e.to_string())?;

        match kind.as_str() {
            "Created" => {
                if let Some(entry) = diff_map.get_mut(&file_id) {
                    if entry.kind == "Deleted" {
                        // Deleted then recreated -> Modified
                        entry.kind = "Modified".to_string();
                        entry.name = name;
                        entry.parent_file_id = parent_file_id;
                        entry.size_delta += size_delta;
                    } else {
                        entry.name = name;
                        entry.parent_file_id = parent_file_id;
                        entry.size_delta += size_delta;
                    }
                } else {
                    diff_map.insert(file_id, DiffFile {
                        file_id,
                        parent_file_id,
                        name,
                        is_directory: is_dir,
                        size_delta,
                        kind: "Created".to_string(),
                        old_name: None,
                        old_parent_file_id: None,
                        path: String::new(),
                    });
                }
            }
            "Modified" => {
                if let Some(entry) = diff_map.get_mut(&file_id) {
                    entry.size_delta += size_delta;
                } else {
                    diff_map.insert(file_id, DiffFile {
                        file_id,
                        parent_file_id,
                        name,
                        is_directory: is_dir,
                        size_delta,
                        kind: "Modified".to_string(),
                        old_name: None,
                        old_parent_file_id: None,
                        path: String::new(),
                    });
                }
            }
            "Renamed" => {
                if let Some(entry) = diff_map.get_mut(&file_id) {
                    entry.name = name;
                    entry.parent_file_id = parent_file_id;
                    if entry.kind != "Created" && entry.kind != "Renamed" {
                        // Find old name before the rename
                        let old_info: Result<(String, u64), rusqlite::Error> = conn.query_row(
                            "SELECT name, parent_file_id FROM mutation_log
                             WHERE volume = ?1 AND file_id = ?2 AND sequence < ?3
                             ORDER BY sequence DESC LIMIT 1",
                            rusqlite::params![start_snap.volume, file_id, seq],
                            |row| Ok((row.get(0)?, row.get(1)?))
                        );
                        let (o_name, o_parent) = old_info.unwrap_or_else(|_| (entry.name.clone(), entry.parent_file_id));
                        entry.kind = "Renamed".to_string();
                        entry.old_name = Some(o_name);
                        entry.old_parent_file_id = Some(o_parent);
                    }
                } else {
                    let old_info: Result<(String, u64), rusqlite::Error> = conn.query_row(
                        "SELECT name, parent_file_id FROM mutation_log
                         WHERE volume = ?1 AND file_id = ?2 AND sequence < ?3
                         ORDER BY sequence DESC LIMIT 1",
                        rusqlite::params![start_snap.volume, file_id, seq],
                        |row| Ok((row.get(0)?, row.get(1)?))
                    );
                    let (o_name, o_parent) = old_info.unwrap_or_else(|_| (name.clone(), parent_file_id));
                    diff_map.insert(file_id, DiffFile {
                        file_id,
                        parent_file_id,
                        name,
                        is_directory: is_dir,
                        size_delta,
                        kind: "Renamed".to_string(),
                        old_name: Some(o_name),
                        old_parent_file_id: Some(o_parent),
                        path: String::new(),
                    });
                }
            }
            "Deleted" => {
                if let Some(entry) = diff_map.get_mut(&file_id) {
                    if entry.kind == "Created" {
                        // Created and deleted in same interval -> Net nothing
                        diff_map.remove(&file_id);
                    } else {
                        entry.kind = "Deleted".to_string();
                        entry.size_delta += size_delta;
                    }
                } else {
                    diff_map.insert(file_id, DiffFile {
                        file_id,
                        parent_file_id,
                        name,
                        is_directory: is_dir,
                        size_delta,
                        kind: "Deleted".to_string(),
                        old_name: None,
                        old_parent_file_id: None,
                        path: String::new(),
                    });
                }
            }
            _ => {}
        }
    }

    // Resolve paths & filter by prefix
    let mut results = Vec::new();
    for (_, mut file) in diff_map {
        let full_path = resolve_diff_file_path(
            &conn,
            &start_snap.volume,
            file.file_id,
            &file.name,
            file.parent_file_id,
        );

        // Path filter check (case-insensitive)
        if let Some(ref filter) = path_filter {
            let filter_lower = filter.to_lowercase();
            let path_lower = full_path.to_lowercase();
            if !path_lower.starts_with(&filter_lower) {
                continue;
            }
        }

        file.path = full_path;
        results.push(file);
    }

    // Sort results by path for clean table presentation
    results.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_mock_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE mutation_log (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                volume TEXT NOT NULL,
                file_id INTEGER NOT NULL,
                parent_file_id INTEGER NOT NULL,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                is_directory INTEGER NOT NULL,
                size_delta INTEGER NOT NULL,
                at TEXT NOT NULL,
                source TEXT NOT NULL
            )",
            [],
        ).unwrap();
        conn
    }

    #[test]
    fn test_diff_reduction_created_modified_deleted() {
        let conn = setup_mock_db();

        // 1. Created and modified file_id 1
        conn.execute(
            "INSERT INTO mutation_log (sequence, volume, file_id, parent_file_id, name, kind, is_directory, size_delta, at, source)
             VALUES (1, 'C:', 1, 0, 'new.txt', 'Created', 0, 100, '2026-07-12T12:05:00Z', 'watcher')",
            []
        ).unwrap();
        conn.execute(
            "INSERT INTO mutation_log (sequence, volume, file_id, parent_file_id, name, kind, is_directory, size_delta, at, source)
             VALUES (2, 'C:', 1, 0, 'new.txt', 'Modified', 0, 50, '2026-07-12T12:10:00Z', 'watcher')",
            []
        ).unwrap();

        // 2. Created then deleted file_id 2
        conn.execute(
            "INSERT INTO mutation_log (sequence, volume, file_id, parent_file_id, name, kind, is_directory, size_delta, at, source)
             VALUES (3, 'C:', 2, 0, 'temp.txt', 'Created', 0, 500, '2026-07-12T12:15:00Z', 'watcher')",
            []
        ).unwrap();
        conn.execute(
            "INSERT INTO mutation_log (sequence, volume, file_id, parent_file_id, name, kind, is_directory, size_delta, at, source)
             VALUES (4, 'C:', 2, 0, 'temp.txt', 'Deleted', 0, -500, '2026-07-12T12:20:00Z', 'watcher')",
            []
        ).unwrap();

        let start_snap = SnapshotEntry {
            id: "s1".to_string(),
            label: "s1".to_string(),
            volume: "C:".to_string(),
            sequence_number: 0,
            created_at: "2026-07-12T12:00:00Z".to_string(),
            daemon_version: "0.1.0".to_string(),
            schema_version: 2,
            retention_setting: "30d".to_string(),
            facts_count: 0,
        };

        let end_snap = SnapshotEntry {
            id: "s2".to_string(),
            label: "s2".to_string(),
            volume: "C:".to_string(),
            sequence_number: 4,
            created_at: "2026-07-12T12:30:00Z".to_string(),
            daemon_version: "0.1.0".to_string(),
            schema_version: 2,
            retention_setting: "30d".to_string(),
            facts_count: 0,
        };

        let diff = calculate_diff(&conn, &start_snap, &end_snap, None).unwrap();
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].file_id, 1);
        assert_eq!(diff[0].kind, "Created");
        assert_eq!(diff[0].size_delta, 150);
        assert_eq!(diff[0].name, "new.txt");
    }

    #[test]
    fn test_diff_reduction_renamed() {
        let conn = setup_mock_db();

        conn.execute(
            "INSERT INTO mutation_log (sequence, volume, file_id, parent_file_id, name, kind, is_directory, size_delta, at, source)
             VALUES (1, 'C:', 10, 0, 'old_name.txt', 'Created', 0, 100, '2026-07-12T11:00:00Z', 'watcher')",
            []
        ).unwrap();
        conn.execute(
            "INSERT INTO mutation_log (sequence, volume, file_id, parent_file_id, name, kind, is_directory, size_delta, at, source)
             VALUES (2, 'C:', 10, 0, 'new_name.txt', 'Renamed', 0, 0, '2026-07-12T12:05:00Z', 'watcher')",
            []
        ).unwrap();

        let start_snap = SnapshotEntry {
            id: "s1".to_string(),
            label: "s1".to_string(),
            volume: "C:".to_string(),
            sequence_number: 1,
            created_at: "2026-07-12T12:00:00Z".to_string(),
            daemon_version: "0.1.0".to_string(),
            schema_version: 2,
            retention_setting: "30d".to_string(),
            facts_count: 0,
        };

        let end_snap = SnapshotEntry {
            id: "s2".to_string(),
            label: "s2".to_string(),
            volume: "C:".to_string(),
            sequence_number: 2,
            created_at: "2026-07-12T12:10:00Z".to_string(),
            daemon_version: "0.1.0".to_string(),
            schema_version: 2,
            retention_setting: "30d".to_string(),
            facts_count: 0,
        };

        let diff = calculate_diff(&conn, &start_snap, &end_snap, None).unwrap();
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].file_id, 10);
        assert_eq!(diff[0].kind, "Renamed");
        assert_eq!(diff[0].name, "new_name.txt");
        assert_eq!(diff[0].old_name, Some("old_name.txt".to_string()));
    }
}
