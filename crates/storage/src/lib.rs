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
            PRIMARY KEY (volume, file_id),
            FOREIGN KEY(volume, parent_file_id) REFERENCES facts(volume, file_id)
        )",
        [],
    )?;

    // Create drain_state table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS drain_state (
            volume TEXT PRIMARY KEY,
            last_sequence INTEGER NOT NULL
        )",
        [],
    )?;

    Ok(db_path)
}
