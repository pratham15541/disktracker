use crate::search;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use storage::{
    check_parent_snapshot_label_exists, create_parent_snapshot_db, create_volume_snapshot_db,
    get_db_connection, get_parent_snapshot_by_label_or_id, list_parent_snapshots_db,
    ParentSnapshotEntry, VolumeSnapshotEntry,
};

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
    let mut volumes = Vec::new();
    if let Some(vols_val) = params.get("volumes").and_then(|v| v.as_array()) {
        for val in vols_val {
            if let Some(s) = val.as_str() {
                volumes.push(s.to_uppercase());
            }
        }
    } else if let Some(vol_str) = params.get("volume").and_then(|v| v.as_str()) {
        volumes.push(vol_str.to_uppercase());
    }

    if volumes.is_empty() {
        return Err("Missing volumes parameter".to_string());
    }

    let registered = core_types::get_registered_volumes();
    for volume in &volumes {
        if !registered.contains(volume) {
            return Err(format!("Volume {} is not registered.", volume));
        }
    }

    let label_opt = params
        .get("label")
        .and_then(|l| l.as_str())
        .map(|s| s.trim().to_string());

    if let Some(ref label) = label_opt {
        if label.is_empty() {
            return Err("Label cannot be empty".to_string());
        }
        if let Ok(Some(existing)) = check_parent_snapshot_label_exists(label) {
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
        jobs.insert(
            job_id.clone(),
            Job {
                id: job_id.clone(),
                completed: false,
                progress: 0,
                result: None,
                error: None,
            },
        );
    }

    let job_id_clone = job_id.clone();
    let volumes_clone = volumes.clone();
    let label_clone = label_opt.clone();
    tokio::spawn(async move {
        let res = tokio::task::spawn_blocking(move || -> Result<ParentSnapshotEntry, String> {
            let conn = get_db_connection().map_err(|e| e.to_string())?;

            let parent_id = generate_random_id("parent");
            let final_label = label_clone.unwrap_or_else(|| {
                let local_time = chrono::Local::now();
                format!("snap_{}", local_time.format("%Y%m%d_%H%M%S"))
            });

            let daemon_ver = env!("CARGO_PKG_VERSION");
            let schema_ver = 2i64;
            let config = config_mgr::load_config();
            let retention = config.retention.clone();

            create_parent_snapshot_db(&parent_id, &final_label, daemon_ver, schema_ver, &retention)
                .map_err(|e| e.to_string())?;

            let mut child_entries = Vec::new();
            for volume in &volumes_clone {
                let seq: i64 = conn
                    .query_row(
                        "SELECT COALESCE(MAX(sequence), 0) FROM mutation_log WHERE volume = ?1",
                        rusqlite::params![volume],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);

                let count: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM facts WHERE volume = ?1",
                        rusqlite::params![volume],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);

                let child_id = generate_random_id("snap");

                create_volume_snapshot_db(&child_id, &parent_id, volume, seq, count)
                    .map_err(|e| e.to_string())?;

                child_entries.push(VolumeSnapshotEntry {
                    id: child_id,
                    parent_id: parent_id.clone(),
                    volume: volume.clone(),
                    sequence_number: seq,
                    facts_count: count,
                });
            }

            Ok(ParentSnapshotEntry {
                id: parent_id,
                label: final_label,
                created_at: Utc::now().to_rfc3339(),
                daemon_version: daemon_ver.to_string(),
                schema_version: schema_ver,
                retention_setting: retention,
                volumes: child_entries,
            })
        })
        .await;

        let mut jobs = get_jobs().lock().unwrap();
        if let Some(job) = jobs.get_mut(&job_id_clone) {
            job.completed = true;
            job.progress = 100;
            match res {
                Ok(Ok(parent_snap)) => {
                    job.result = Some(serde_json::to_value(parent_snap).unwrap());
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

pub fn trigger_auto_snapshot_for_all_volumes() -> Result<(), String> {
    let conn = storage::get_db_connection().map_err(|e| e.to_string())?;
    let volumes = core_types::get_registered_volumes();

    let now_local = chrono::Local::now();
    let label = format!("auto_{}", now_local.format("%Y-%m-%d_%H-%M-%S"));

    let daemon_ver = env!("CARGO_PKG_VERSION");
    let schema_ver = 2i64;
    let config = config_mgr::load_config();
    let retention = config.retention.clone();

    let parent_id = generate_random_id("parent");

    // Insert parent snapshot record
    storage::create_parent_snapshot_db(&parent_id, &label, daemon_ver, schema_ver, &retention)
        .map_err(|e| e.to_string())?;

    for volume in &volumes {
        let seq: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(sequence), 0) FROM mutation_log WHERE volume = ?1",
                rusqlite::params![volume],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM facts WHERE volume = ?1",
                rusqlite::params![volume],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let child_id = generate_random_id("snap");

        storage::create_volume_snapshot_db(&child_id, &parent_id, volume, seq, count)
            .map_err(|e| e.to_string())?;

        println!(
            "[Auto-Snapshot] Created snapshot \"{}\" for volume {} (id: {})",
            label, volume, child_id
        );
    }
    Ok(())
}

pub fn get_last_auto_snapshot_time() -> Option<chrono::DateTime<chrono::Utc>> {
    let conn = storage::get_db_connection().ok()?;
    let time_str: String = conn
        .query_row(
            "SELECT MAX(created_at) FROM parent_snapshots WHERE label LIKE 'auto_%'",
            [],
            |row| row.get(0),
        )
        .ok()?;
    chrono::DateTime::parse_from_rfc3339(&time_str)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .ok()
}

/// JSON-RPC: job.completed
pub fn handle_job_completed(params: Value) -> Result<Value, String> {
    let job_id = params
        .get("job_id")
        .and_then(|j| j.as_str())
        .ok_or("Missing job_id parameter")?;
    let jobs = get_jobs().lock().unwrap();
    if let Some(job) = jobs.get(job_id) {
        Ok(serde_json::to_value(job).unwrap())
    } else {
        Err("E_NOT_FOUND: Job not found".to_string())
    }
}

/// JSON-RPC: snapshot_list
pub fn handle_snapshot_list(params: Value) -> Result<Value, String> {
    let volume = params
        .get("volume")
        .and_then(|v| v.as_str())
        .map(|s| s.to_uppercase());
    let limit = params.get("limit").and_then(|l| l.as_u64()).unwrap_or(20) as usize;

    let list = list_parent_snapshots_db(volume.as_deref(), limit).map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "results": list
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
    let volume_filter = params
        .get("volume")
        .and_then(|v| v.as_str())
        .map(|s| s.to_uppercase());
    let snapshot_a_input = params
        .get("snapshot_a")
        .and_then(|s| s.as_str())
        .ok_or("Missing snapshot_a parameter")?;
    let snapshot_b_input = params
        .get("snapshot_b")
        .and_then(|s| s.as_str())
        .ok_or("Missing snapshot_b parameter")?;
    let path_filter = params
        .get("path_filter")
        .and_then(|p| p.as_str())
        .map(|s| s.replace('\\', "/"));

    let conn = get_db_connection().map_err(|e| e.to_string())?;

    let parent_a = get_parent_snapshot_by_label_or_id(snapshot_a_input)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "E_NOT_FOUND: Couldn't find snapshot \"{}\". Check the path/ID and try again.",
                snapshot_a_input
            )
        })?;

    let parent_b = get_parent_snapshot_by_label_or_id(snapshot_b_input)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "E_NOT_FOUND: Couldn't find snapshot \"{}\". Check the path/ID and try again.",
                snapshot_b_input
            )
        })?;

    let mut volume_pairs = Vec::new();

    if let Some(vol) = volume_filter {
        let child_a = parent_a.volumes.iter().find(|v| v.volume == vol);
        let child_b = parent_b.volumes.iter().find(|v| v.volume == vol);
        if let (Some(ca), Some(cb)) = (child_a, child_b) {
            volume_pairs.push((vol, ca.sequence_number, cb.sequence_number));
        } else {
            return Err(format!(
                "E_NOT_FOUND: Target volume {} is not present in both snapshots.",
                vol
            ));
        }
    } else {
        // Find common volumes
        for ca in &parent_a.volumes {
            if let Some(cb) = parent_b.volumes.iter().find(|v| v.volume == ca.volume) {
                volume_pairs.push((ca.volume.clone(), ca.sequence_number, cb.sequence_number));
            }
        }
    }

    if volume_pairs.is_empty() {
        return Err("No common volumes found to compare between these snapshots.".to_string());
    }

    let mut all_results = Vec::new();
    for (vol, seq_a, seq_b) in volume_pairs {
        let (start_seq, end_seq) = if seq_a <= seq_b {
            (seq_a, seq_b)
        } else {
            (seq_b, seq_a)
        };
        let res = calculate_diff(&conn, &vol, start_seq, end_seq, path_filter.clone())?;
        all_results.extend(res);
    }

    Ok(serde_json::json!({
        "snapshot_a": parent_a.label,
        "snapshot_b": parent_b.label,
        "results": all_results,
    }))
}

pub fn calculate_diff(
    conn: &Connection,
    volume: &str,
    start_seq: i64,
    end_seq: i64,
    path_filter: Option<String>,
) -> Result<Vec<DiffFile>, String> {
    // Check if start snapshot sequence is older than the oldest mutation log entry
    let min_seq: i64 = conn
        .query_row(
            "SELECT COALESCE(MIN(sequence), 0) FROM mutation_log WHERE volume = ?1",
            rusqlite::params![volume],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if start_seq < min_seq && min_seq > 1 {
        let config = config_mgr::load_config();
        return Err(format!(
            "E_SNAPSHOT_DATA_EXPIRED: This comparison goes further back than DiskTracker currently keeps ({}). Try a more recent snapshot pair.",
            config.retention
        ));
    }

    // Query all mutations between the two sequence bookmarks
    let mut stmt = conn
        .prepare(
            "SELECT sequence, file_id, parent_file_id, name, kind, is_directory, size_delta
         FROM mutation_log
         WHERE volume = ?1 AND sequence > ?2 AND sequence <= ?3
         ORDER BY sequence ASC",
        )
        .map_err(|e| e.to_string())?;

    let mut rows = stmt
        .query(rusqlite::params![volume, start_seq, end_seq])
        .map_err(|e| e.to_string())?;

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
                    diff_map.insert(
                        file_id,
                        DiffFile {
                            file_id,
                            parent_file_id,
                            name,
                            is_directory: is_dir,
                            size_delta,
                            kind: "Created".to_string(),
                            old_name: None,
                            old_parent_file_id: None,
                            path: String::new(),
                        },
                    );
                }
            }
            "Modified" => {
                if let Some(entry) = diff_map.get_mut(&file_id) {
                    entry.size_delta += size_delta;
                } else {
                    diff_map.insert(
                        file_id,
                        DiffFile {
                            file_id,
                            parent_file_id,
                            name,
                            is_directory: is_dir,
                            size_delta,
                            kind: "Modified".to_string(),
                            old_name: None,
                            old_parent_file_id: None,
                            path: String::new(),
                        },
                    );
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
                            rusqlite::params![volume, file_id, seq],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        );
                        let (o_name, o_parent) =
                            old_info.unwrap_or_else(|_| (entry.name.clone(), entry.parent_file_id));
                        entry.kind = "Renamed".to_string();
                        entry.old_name = Some(o_name);
                        entry.old_parent_file_id = Some(o_parent);
                    }
                } else {
                    let old_info: Result<(String, u64), rusqlite::Error> = conn.query_row(
                        "SELECT name, parent_file_id FROM mutation_log
                         WHERE volume = ?1 AND file_id = ?2 AND sequence < ?3
                         ORDER BY sequence DESC LIMIT 1",
                        rusqlite::params![volume, file_id, seq],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    );
                    let (o_name, o_parent) =
                        old_info.unwrap_or_else(|_| (name.clone(), parent_file_id));
                    diff_map.insert(
                        file_id,
                        DiffFile {
                            file_id,
                            parent_file_id,
                            name,
                            is_directory: is_dir,
                            size_delta,
                            kind: "Renamed".to_string(),
                            old_name: Some(o_name),
                            old_parent_file_id: Some(o_parent),
                            path: String::new(),
                        },
                    );
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
                    diff_map.insert(
                        file_id,
                        DiffFile {
                            file_id,
                            parent_file_id,
                            name,
                            is_directory: is_dir,
                            size_delta,
                            kind: "Deleted".to_string(),
                            old_name: None,
                            old_parent_file_id: None,
                            path: String::new(),
                        },
                    );
                }
            }
            _ => {}
        }
    }

    // Resolve paths & filter by prefix
    let mut results = Vec::new();
    for (_, mut file) in diff_map {
        // Created/Deleted/Renamed (incl. 0B) always shown; Modified only if |delta| >= 2
        if !crate::mutation_filter::is_displayable_mutation(&file.kind, file.size_delta) {
            continue;
        }
        let full_path =
            resolve_diff_file_path(conn, volume, file.file_id, &file.name, file.parent_file_id);

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
        )
        .unwrap();
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

        let diff = calculate_diff(&conn, "C:", 0, 4, None).unwrap();
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

        let diff = calculate_diff(&conn, "C:", 1, 2, None).unwrap();
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].file_id, 10);
        assert_eq!(diff[0].kind, "Renamed");
        assert_eq!(diff[0].name, "new_name.txt");
        assert_eq!(diff[0].old_name, Some("old_name.txt".to_string()));
    }
}
