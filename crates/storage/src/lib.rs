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
    conn.pragma_update(None, "foreign_keys", "ON")?;
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
    conn.pragma_update(None, "foreign_keys", "ON")?;
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
            conn.execute("ALTER TABLE facts ADD COLUMN attributes INTEGER NOT NULL DEFAULT 0", [])?;
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

    Ok(db_path)
}

#[derive(Debug, Clone)]
pub struct PruningLogEntry {
    pub volume: String,
    pub run_at: String,
    pub status: String,
    pub details: String,
}

pub fn log_pruning_run(volume: &str, status: &str, details: &str) -> std::result::Result<(), Box<dyn Error>> {
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
    let mut stmt = conn.prepare(
        "SELECT volume, run_at, status, details FROM pruning_log ORDER BY run_at DESC"
    )?;
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

