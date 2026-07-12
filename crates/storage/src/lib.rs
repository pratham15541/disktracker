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

    // Create snapshots table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS snapshots (
            id TEXT PRIMARY KEY,
            label TEXT NOT NULL,
            volume TEXT NOT NULL,
            sequence_number INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            daemon_version TEXT NOT NULL,
            schema_version INTEGER NOT NULL,
            retention_setting TEXT NOT NULL,
            facts_count INTEGER NOT NULL,
            UNIQUE(volume, label)
        )",
        [],
    )?;

    Ok(db_path)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SnapshotEntry {
    pub id: String,
    pub label: String,
    pub volume: String,
    pub sequence_number: i64,
    pub created_at: String,
    pub daemon_version: String,
    pub schema_version: i64,
    pub retention_setting: String,
    pub facts_count: i64,
}

pub fn create_snapshot_db(
    id: &str,
    label: &str,
    volume: &str,
    sequence_number: i64,
    daemon_version: &str,
    schema_version: i64,
    retention_setting: &str,
    facts_count: i64,
) -> std::result::Result<(), Box<dyn Error>> {
    let conn = get_db_connection()?;
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO snapshots (id, label, volume, sequence_number, created_at, daemon_version, schema_version, retention_setting, facts_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![id, label, volume, sequence_number, now, daemon_version, schema_version, retention_setting, facts_count],
    )?;
    Ok(())
}

pub fn get_snapshot_by_label_or_id(
    volume: &str,
    input: &str,
) -> std::result::Result<Option<SnapshotEntry>, Box<dyn Error>> {
    let conn = get_db_connection()?;
    let mut stmt = conn.prepare(
        "SELECT id, label, volume, sequence_number, created_at, daemon_version, schema_version, retention_setting, facts_count
         FROM snapshots WHERE volume = ?1 AND (id = ?2 OR label = ?2)",
    )?;
    let mut rows = stmt.query(rusqlite::params![volume, input])?;
    if let Some(row) = rows.next()? {
        Ok(Some(SnapshotEntry {
            id: row.get(0)?,
            label: row.get(1)?,
            volume: row.get(2)?,
            sequence_number: row.get(3)?,
            created_at: row.get(4)?,
            daemon_version: row.get(5)?,
            schema_version: row.get(6)?,
            retention_setting: row.get(7)?,
            facts_count: row.get(8)?,
        }))
    } else {
        Ok(None)
    }
}

pub fn check_snapshot_label_exists(
    volume: &str,
    label: &str,
) -> std::result::Result<Option<SnapshotEntry>, Box<dyn Error>> {
    let conn = get_db_connection()?;
    let mut stmt = conn.prepare(
        "SELECT id, label, volume, sequence_number, created_at, daemon_version, schema_version, retention_setting, facts_count
         FROM snapshots WHERE volume = ?1 AND label = ?2",
    )?;
    let mut rows = stmt.query(rusqlite::params![volume, label])?;
    if let Some(row) = rows.next()? {
        Ok(Some(SnapshotEntry {
            id: row.get(0)?,
            label: row.get(1)?,
            volume: row.get(2)?,
            sequence_number: row.get(3)?,
            created_at: row.get(4)?,
            daemon_version: row.get(5)?,
            schema_version: row.get(6)?,
            retention_setting: row.get(7)?,
            facts_count: row.get(8)?,
        }))
    } else {
        Ok(None)
    }
}

pub fn list_snapshots_db(
    volume: Option<&str>,
    limit: usize,
    cursor_seq: Option<i64>,
    is_backward: bool,
) -> std::result::Result<Vec<SnapshotEntry>, Box<dyn Error>> {
    let conn = get_db_connection()?;
    let mut query = String::from(
        "SELECT id, label, volume, sequence_number, created_at, daemon_version, schema_version, retention_setting, facts_count
         FROM snapshots"
    );
    let mut params = Vec::new();
    let mut conditions = Vec::new();

    if let Some(v) = volume {
        params.push(v.to_string());
        conditions.push(format!("volume = ?{}", params.len()));
    }

    if let Some(c) = cursor_seq {
        params.push(c.to_string());
        if is_backward {
            conditions.push(format!("sequence_number < ?{}", params.len()));
        } else {
            conditions.push(format!("sequence_number > ?{}", params.len()));
        }
    }

    if !conditions.is_empty() {
        query.push_str(" WHERE ");
        query.push_str(&conditions.join(" AND "));
    }

    if is_backward {
        query.push_str(" ORDER BY sequence_number DESC LIMIT ");
    } else {
        query.push_str(" ORDER BY sequence_number ASC LIMIT ");
    }
    query.push_str(&(limit + 1).to_string());

    let mut stmt = conn.prepare(&query)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(params))?;
    let mut results = Vec::new();
    while let Some(row) = rows.next()? {
        results.push(SnapshotEntry {
            id: row.get(0)?,
            label: row.get(1)?,
            volume: row.get(2)?,
            sequence_number: row.get(3)?,
            created_at: row.get(4)?,
            daemon_version: row.get(5)?,
            schema_version: row.get(6)?,
            retention_setting: row.get(7)?,
            facts_count: row.get(8)?,
        });
    }

    if is_backward {
        results.reverse();
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
