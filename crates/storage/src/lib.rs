use rusqlite::Connection;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

/// Resolves and creates the database storage directory.
/// - Windows: `%LOCALAPPDATA%\disktracker`
/// - Unix/Linux/WSL: `~/.local/share/disktracker`
pub fn get_db_dir() -> std::io::Result<PathBuf> {
    let mut path = if cfg!(windows) {
        if let Ok(local_appdata) = std::env::var("LOCALAPPDATA") {
            PathBuf::from(local_appdata)
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "LOCALAPPDATA environment variable not set",
            ));
        }
    } else {
        if let Ok(home) = std::env::var("HOME") {
            let mut p = PathBuf::from(home);
            p.push(".local");
            p.push("share");
            p
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "HOME environment variable not set",
            ));
        }
    };
    path.push("disktracker");
    fs::create_dir_all(&path)?;
    Ok(path)
}

/// Establish an active connection to the SQLite database and enable WAL mode.
pub fn get_db_connection() -> std::result::Result<Connection, Box<dyn Error>> {
    let mut db_path = get_db_dir()?;
    db_path.push("disktracker.db");

    let conn = Connection::open(&db_path)?;

    // Enable WAL mode, optimize synchronous writes, set cache size and busy timeout
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "cache_size", &"-64000".to_string())?;
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;

    Ok(conn)
}

/// Establish a read-only connection to the SQLite database.
pub fn get_readonly_db_connection() -> std::result::Result<Connection, Box<dyn Error>> {
    let mut db_path = get_db_dir()?;
    db_path.push("disktracker.db");

    let conn = Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Ok(conn)
}

/// Executes a query on the read-only connection and returns results as an array of JSON objects.
pub fn execute_readonly_query(
    query: &str,
) -> std::result::Result<serde_json::Value, Box<dyn Error>> {
    let conn = get_readonly_db_connection()?;
    let mut stmt = conn.prepare(query)?;
    let col_count = stmt.column_count();
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();

    let mut rows = stmt.query([])?;
    let mut results = Vec::new();

    while let Some(row) = rows.next()? {
        let mut row_map = serde_json::Map::new();
        for i in 0..col_count {
            let val = row.get_ref(i)?;
            let json_val = match val {
                rusqlite::types::ValueRef::Null => serde_json::Value::Null,
                rusqlite::types::ValueRef::Integer(n) => {
                    serde_json::Value::Number(serde_json::Number::from(n))
                }
                rusqlite::types::ValueRef::Real(r) => {
                    if let Some(num) = serde_json::Number::from_f64(r) {
                        serde_json::Value::Number(num)
                    } else {
                        serde_json::Value::Null
                    }
                }
                rusqlite::types::ValueRef::Text(t) => {
                    let s = std::str::from_utf8(t).unwrap_or("");
                    serde_json::Value::String(s.to_string())
                }
                rusqlite::types::ValueRef::Blob(b) => {
                    let s = String::from_utf8_lossy(b).into_owned();
                    serde_json::Value::String(s)
                }
            };
            row_map.insert(col_names[i].clone(), json_val);
        }
        results.push(serde_json::Value::Object(row_map));
    }

    Ok(serde_json::Value::Array(results))
}

/// Initializes/opens the database and creates facts and mutation_log tables.
pub fn init_db() -> std::result::Result<PathBuf, Box<dyn Error>> {
    let mut db_path = get_db_dir()?;
    db_path.push("disktracker.db");

    let conn = Connection::open(&db_path)?;

    // Enable WAL mode, optimize synchronous writes, set cache size and busy timeout
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "cache_size", &"-64000".to_string())?;
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;

    // Create mutation_log table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS mutation_log (
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
    )?;

    // Create facts table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS facts (
            volume TEXT NOT NULL,
            file_id INTEGER NOT NULL,
            parent_file_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            is_directory INTEGER NOT NULL,
            size INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            modified_at TEXT NOT NULL,
            attributes INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (volume, file_id),
            FOREIGN KEY(volume, parent_file_id) REFERENCES facts(volume, file_id)
        )",
        [],
    )?;

    // Create index on parent_file_id to speed up foreign key checks during deletes
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_facts_parent ON facts (volume, parent_file_id)",
        [],
    )?;

    // Migration: Add attributes column if it doesn't exist (upgrading from Epoch 1)
    {
        let mut stmt = conn.prepare("PRAGMA table_info(facts)")?;
        let mut has_attributes = false;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            if name == "attributes" {
                has_attributes = true;
                break;
            }
        }
        if !has_attributes {
            conn.execute(
                "ALTER TABLE facts ADD COLUMN attributes INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
    }

    // Create drain_state table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS drain_state (
            volume TEXT PRIMARY KEY,
            last_sequence INTEGER NOT NULL
        )",
        [],
    )?;

    // Create pruning_log table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS pruning_log (
            volume TEXT NOT NULL,
            run_at TEXT NOT NULL,
            status TEXT NOT NULL,
            details TEXT NOT NULL
        )",
        [],
    )?;

    // Drop old flat snapshots table if it exists
    let _ = conn.execute("DROP TABLE IF EXISTS snapshots", []);

    // Create parent_snapshots table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS parent_snapshots (
            id TEXT PRIMARY KEY,
            label TEXT UNIQUE NOT NULL,
            created_at TEXT NOT NULL,
            daemon_version TEXT NOT NULL,
            schema_version INTEGER NOT NULL,
            retention_setting TEXT NOT NULL
        )",
        [],
    )?;

    // Create volume_snapshots table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS volume_snapshots (
            id TEXT PRIMARY KEY,
            parent_id TEXT NOT NULL,
            volume TEXT NOT NULL,
            sequence_number INTEGER NOT NULL,
            facts_count INTEGER NOT NULL,
            FOREIGN KEY (parent_id) REFERENCES parent_snapshots(id) ON DELETE CASCADE,
            UNIQUE(parent_id, volume)
        )",
        [],
    )?;

    // Create app_install_footprints table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS app_install_footprints (
            app_name TEXT NOT NULL,
            file_path TEXT NOT NULL,
            install_time TEXT NOT NULL,
            PRIMARY KEY (app_name, file_path)
        )",
        [],
    )?;

    // Create app_runtime_artifacts table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS app_runtime_artifacts (
            process_name TEXT NOT NULL,
            target_path TEXT NOT NULL,
            access_time TEXT NOT NULL,
            PRIMARY KEY (process_name, target_path)
        )",
        [],
    )?;

    Ok(db_path)
}

pub fn insert_app_install_footprint(
    app_name: &str,
    file_path: &str,
) -> std::result::Result<(), Box<dyn Error>> {
    let conn = get_db_connection()?;
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR REPLACE INTO app_install_footprints (app_name, file_path, install_time)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![app_name, file_path, now],
    )?;
    Ok(())
}

pub fn insert_app_runtime_artifact(
    process_name: &str,
    target_path: &str,
) -> std::result::Result<(), Box<dyn Error>> {
    let conn = get_db_connection()?;
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR REPLACE INTO app_runtime_artifacts (process_name, target_path, access_time)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![process_name, target_path, now],
    )?;
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParentSnapshotEntry {
    pub id: String,
    pub label: String,
    pub created_at: String,
    pub daemon_version: String,
    pub schema_version: i64,
    pub retention_setting: String,
    pub volumes: Vec<VolumeSnapshotEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VolumeSnapshotEntry {
    pub id: String,
    pub parent_id: String,
    pub volume: String,
    pub sequence_number: i64,
    pub facts_count: i64,
}

pub fn create_parent_snapshot_db(
    id: &str,
    label: &str,
    daemon_version: &str,
    schema_version: i64,
    retention_setting: &str,
) -> std::result::Result<(), Box<dyn Error>> {
    let conn = get_db_connection()?;
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO parent_snapshots (id, label, created_at, daemon_version, schema_version, retention_setting)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![id, label, now, daemon_version, schema_version, retention_setting],
    )?;
    Ok(())
}

pub fn create_volume_snapshot_db(
    id: &str,
    parent_id: &str,
    volume: &str,
    sequence_number: i64,
    facts_count: i64,
) -> std::result::Result<(), Box<dyn Error>> {
    let conn = get_db_connection()?;
    conn.execute(
        "INSERT INTO volume_snapshots (id, parent_id, volume, sequence_number, facts_count)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![id, parent_id, volume, sequence_number, facts_count],
    )?;
    Ok(())
}

pub fn get_parent_snapshot_by_label_or_id(
    input: &str,
) -> std::result::Result<Option<ParentSnapshotEntry>, Box<dyn Error>> {
    let conn = get_db_connection()?;
    let mut stmt = conn.prepare(
        "SELECT id, label, created_at, daemon_version, schema_version, retention_setting
         FROM parent_snapshots WHERE id = ?1 OR label = ?1",
    )?;
    let mut rows = stmt.query(rusqlite::params![input])?;
    if let Some(row) = rows.next()? {
        let parent_id: String = row.get(0)?;
        let label: String = row.get(1)?;
        let created_at: String = row.get(2)?;
        let daemon_version: String = row.get(3)?;
        let schema_version: i64 = row.get(4)?;
        let retention_setting: String = row.get(5)?;

        let mut sub_stmt = conn.prepare(
            "SELECT id, volume, sequence_number, facts_count FROM volume_snapshots WHERE parent_id = ?1",
        )?;
        let mut sub_rows = sub_stmt.query(rusqlite::params![parent_id])?;
        let mut volumes = Vec::new();
        while let Some(sub_row) = sub_rows.next()? {
            volumes.push(VolumeSnapshotEntry {
                id: sub_row.get(0)?,
                parent_id: parent_id.clone(),
                volume: sub_row.get(1)?,
                sequence_number: sub_row.get(2)?,
                facts_count: sub_row.get(3)?,
            });
        }

        Ok(Some(ParentSnapshotEntry {
            id: parent_id,
            label,
            created_at,
            daemon_version,
            schema_version,
            retention_setting,
            volumes,
        }))
    } else {
        Ok(None)
    }
}

pub fn check_parent_snapshot_label_exists(
    label: &str,
) -> std::result::Result<Option<ParentSnapshotEntry>, Box<dyn Error>> {
    let conn = get_db_connection()?;
    let mut stmt = conn.prepare(
        "SELECT id, label, created_at, daemon_version, schema_version, retention_setting
         FROM parent_snapshots WHERE label = ?1",
    )?;
    let mut rows = stmt.query(rusqlite::params![label])?;
    if let Some(row) = rows.next()? {
        let parent_id: String = row.get(0)?;
        let label: String = row.get(1)?;
        let created_at: String = row.get(2)?;
        let daemon_version: String = row.get(3)?;
        let schema_version: i64 = row.get(4)?;
        let retention_setting: String = row.get(5)?;

        let mut sub_stmt = conn.prepare(
            "SELECT id, volume, sequence_number, facts_count FROM volume_snapshots WHERE parent_id = ?1",
        )?;
        let mut sub_rows = sub_stmt.query(rusqlite::params![parent_id])?;
        let mut volumes = Vec::new();
        while let Some(sub_row) = sub_rows.next()? {
            volumes.push(VolumeSnapshotEntry {
                id: sub_row.get(0)?,
                parent_id: parent_id.clone(),
                volume: sub_row.get(1)?,
                sequence_number: sub_row.get(2)?,
                facts_count: sub_row.get(3)?,
            });
        }

        Ok(Some(ParentSnapshotEntry {
            id: parent_id,
            label,
            created_at,
            daemon_version,
            schema_version,
            retention_setting,
            volumes,
        }))
    } else {
        Ok(None)
    }
}

pub fn list_parent_snapshots_db(
    volume_filter: Option<&str>,
    limit: usize,
) -> std::result::Result<Vec<ParentSnapshotEntry>, Box<dyn Error>> {
    let conn = get_db_connection()?;
    let mut query = String::from(
        "SELECT id, label, created_at, daemon_version, schema_version, retention_setting
         FROM parent_snapshots",
    );
    let mut params = Vec::new();
    if let Some(vol) = volume_filter {
        params.push(vol.to_string());
        query.push_str(" WHERE id IN (SELECT parent_id FROM volume_snapshots WHERE volume = ?1)");
    }

    query.push_str(" ORDER BY created_at DESC LIMIT ");
    query.push_str(&limit.to_string());

    let mut stmt = conn.prepare(&query)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(params))?;
    let mut results = Vec::new();
    while let Some(row) = rows.next()? {
        let parent_id: String = row.get(0)?;
        let label: String = row.get(1)?;
        let created_at: String = row.get(2)?;
        let daemon_version: String = row.get(3)?;
        let schema_version: i64 = row.get(4)?;
        let retention_setting: String = row.get(5)?;

        let mut sub_stmt = conn.prepare(
            "SELECT id, volume, sequence_number, facts_count FROM volume_snapshots WHERE parent_id = ?1",
        )?;
        let mut sub_rows = sub_stmt.query(rusqlite::params![parent_id])?;
        let mut volumes = Vec::new();
        while let Some(sub_row) = sub_rows.next()? {
            let child_vol: String = sub_row.get(1)?;
            if let Some(vf) = volume_filter {
                if child_vol != vf {
                    continue;
                }
            }
            volumes.push(VolumeSnapshotEntry {
                id: sub_row.get(0)?,
                parent_id: parent_id.clone(),
                volume: child_vol,
                sequence_number: sub_row.get(2)?,
                facts_count: sub_row.get(3)?,
            });
        }

        results.push(ParentSnapshotEntry {
            id: parent_id,
            label,
            created_at,
            daemon_version,
            schema_version,
            retention_setting,
            volumes,
        });
    }

    Ok(results)
}

#[derive(Debug, Clone)]
pub struct PruningLogEntry {
    pub volume: String,
    pub run_at: String,
    pub status: String,
    pub details: String,
}

pub fn log_pruning_run(
    volume: &str,
    status: &str,
    details: &str,
) -> std::result::Result<(), Box<dyn Error>> {
    let conn = get_db_connection()?;
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO pruning_log (volume, run_at, status, details) VALUES (?1, ?2, ?3, ?4)",
        [volume, &now, status, details],
    )?;
    Ok(())
}

pub fn get_latest_pruning_runs() -> std::result::Result<Vec<PruningLogEntry>, Box<dyn Error>> {
    let conn = get_db_connection()?;
    let mut stmt = conn
        .prepare("SELECT volume, run_at, status, details FROM pruning_log ORDER BY run_at DESC")?;
    let rows = stmt.query_map([], |row| {
        Ok(PruningLogEntry {
            volume: row.get(0)?,
            run_at: row.get(1)?,
            status: row.get(2)?,
            details: row.get(3)?,
        })
    })?;
    let mut results = Vec::new();
    for r in rows {
        results.push(r?);
    }
    Ok(results)
}

pub fn delete_snapshot_db(label_or_id: &str) -> std::result::Result<(), Box<dyn Error>> {
    let conn = get_db_connection()?;
    conn.execute("PRAGMA foreign_keys = ON", [])?;
    let rows_affected = conn.execute(
        "DELETE FROM parent_snapshots WHERE id = ?1 OR label = ?1",
        [label_or_id],
    )?;
    if rows_affected == 0 {
        return Err("Snapshot not found".into());
    }
    Ok(())
}

pub fn load_whitelist() -> std::result::Result<serde_json::Value, Box<dyn Error>> {
    let mut path = get_db_dir()?;
    path.push("whitelist.json");
    if !path.exists() {
        let defaults = serde_json::json!({
            "read": ["dir", "ls", "df", "du", "cat", "ps", "free", "get-childitem", "get-item", "get-itemproperty", "get-process", "get-service", "type"],
            "write": ["rm", "del", "remove-item", "rmdir", "erase"]
        });
        fs::write(&path, serde_json::to_string_pretty(&defaults)?)?;
        return Ok(defaults);
    }
    let data = fs::read_to_string(&path)?;
    let val: serde_json::Value = serde_json::from_str(&data)?;
    Ok(val)
}

pub fn save_whitelist(whitelist: &serde_json::Value) -> std::result::Result<(), Box<dyn Error>> {
    let mut path = get_db_dir()?;
    path.push("whitelist.json");
    fs::write(&path, serde_json::to_string_pretty(whitelist)?)?;
    Ok(())
}

pub fn load_signatures() -> std::result::Result<serde_json::Value, Box<dyn Error>> {
    let mut path = get_db_dir()?;
    path.push("signatures.json");
    if !path.exists() {
        let defaults = serde_json::json!({
            "glcache": "GLCache is an NVIDIA shader cache; safe to delete.",
            "deriveddatacache": "DerivedDataCache is an Unreal Engine shader/asset cache; safe to delete.",
            "appdata\\local\\temp": "Temporary folder containing transient session files; safe to delete.",
            "tmp": "Temporary folder containing transient session files; safe to delete.",
            "steam": "Steam library or configuration files. Deleting this may uninstall games or disrupt client login.",
            "windows": "Critical operating system files. NEVER delete or mutate files in this directory.",
            "system32": "Critical operating system files. NEVER delete or mutate files in this directory."
        });
        fs::write(&path, serde_json::to_string_pretty(&defaults)?)?;
        return Ok(defaults);
    }
    let data = fs::read_to_string(&path)?;
    let val: serde_json::Value = serde_json::from_str(&data)?;
    Ok(val)
}

pub fn save_signatures(sigs: &serde_json::Value) -> std::result::Result<(), Box<dyn Error>> {
    let mut path = get_db_dir()?;
    path.push("signatures.json");
    fs::write(&path, serde_json::to_string_pretty(sigs)?)?;
    Ok(())
}

pub fn find_footprint_association(
    path_str: &str,
) -> std::result::Result<Option<(String, String)>, Box<dyn Error>> {
    let conn = get_db_connection()?;

    // Check install footprints
    let mut stmt = conn.prepare("SELECT app_name FROM app_install_footprints WHERE file_path = ?1 OR ?1 LIKE file_path || '%' LIMIT 1")?;
    let mut rows = stmt.query([path_str])?;
    if let Some(row) = rows.next()? {
        let app_name: String = row.get(0)?;
        return Ok(Some((app_name, "install".to_string())));
    }

    // Check runtime footprints
    let mut stmt2 = conn.prepare("SELECT process_name FROM app_runtime_artifacts WHERE file_path = ?1 OR ?1 LIKE file_path || '%' LIMIT 1")?;
    let mut rows2 = stmt2.query([path_str])?;
    if let Some(row) = rows2.next()? {
        let proc_name: String = row.get(0)?;
        return Ok(Some((proc_name, "runtime".to_string())));
    }

    Ok(None)
}
