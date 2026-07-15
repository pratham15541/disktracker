use crate::graph::AskState;
use rusqlite::{params, Connection};
use std::path::PathBuf;

fn get_sessions_db_path() -> Result<PathBuf, String> {
    let mut dir = storage::get_db_dir().map_err(|e| e.to_string())?;
    dir.push("agent_sessions.db");
    Ok(dir)
}

fn get_conn() -> Result<Connection, String> {
    let path = get_sessions_db_path()?;
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS sessions (
            session_id TEXT PRIMARY KEY,
            first_question TEXT NOT NULL,
            created_at TEXT NOT NULL,
            state_json TEXT NOT NULL
        )",
        [],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn)
}

pub fn save_session(
    session_id: &str,
    first_question: &str,
    state: &AskState,
) -> Result<(), String> {
    let conn = get_conn()?;
    let state_json = serde_json::to_string(state).map_err(|e| e.to_string())?;
    let created_at = chrono::Utc::now().to_rfc3339();

    conn.execute(
        "INSERT OR REPLACE INTO sessions (session_id, first_question, created_at, state_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![session_id, first_question, created_at, state_json],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

pub fn load_session(session_id: &str) -> Result<Option<AskState>, String> {
    let conn = get_conn()?;
    let mut stmt = conn
        .prepare("SELECT state_json FROM sessions WHERE session_id = ?1")
        .map_err(|e| e.to_string())?;

    let mut rows = stmt
        .query_map(params![session_id], |row| {
            let json: String = row.get(0)?;
            Ok(json)
        })
        .map_err(|e| e.to_string())?;

    if let Some(row_res) = rows.next() {
        let json_str = row_res.map_err(|e| e.to_string())?;
        let state: AskState = serde_json::from_str(&json_str).map_err(|e| e.to_string())?;
        Ok(Some(state))
    } else {
        Ok(None)
    }
}

pub fn list_sessions() -> Result<Vec<(String, String, String)>, String> {
    let conn = get_conn()?;
    let mut stmt = conn
        .prepare(
            "SELECT session_id, created_at, first_question FROM sessions ORDER BY created_at DESC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let created: String = row.get(1)?;
            let q: String = row.get(2)?;
            Ok((id, created, q))
        })
        .map_err(|e| e.to_string())?;

    let mut list = Vec::new();
    for r in rows {
        list.push(r.map_err(|e| e.to_string())?);
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sqlite_session_store_mock() {
        let temp_dir = std::env::temp_dir();
        let db_path = temp_dir.join("test_agent_sessions.db");
        if db_path.exists() {
            let _ = std::fs::remove_file(&db_path);
        }

        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE IF NOT EXISTS sessions (
                session_id TEXT PRIMARY KEY,
                first_question TEXT NOT NULL,
                created_at TEXT NOT NULL,
                state_json TEXT NOT NULL
            )",
            [],
        )
        .unwrap();

        let state = AskState {
            question: "test question".to_string(),
            messages: vec![],
            round_count: 1,
            interactive: false,
            json: false,
            data_used: vec![],
            final_answer: None,
        };

        let state_json = serde_json::to_string(&state).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO sessions (session_id, first_question, created_at, state_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                "session_123",
                "test question",
                "2026-07-14T12:00:00Z",
                state_json
            ],
        )
        .unwrap();

        let mut stmt = conn
            .prepare("SELECT state_json FROM sessions WHERE session_id = ?1")
            .unwrap();
        let mut rows = stmt
            .query_map(params!["session_123"], |row| {
                let val: String = row.get(0)?;
                Ok(val)
            })
            .unwrap();

        let row = rows.next().unwrap().unwrap();
        let loaded: AskState = serde_json::from_str(&row).unwrap();
        assert_eq!(loaded.question, "test question");
        assert_eq!(loaded.round_count, 1);

        let _ = std::fs::remove_file(db_path);
    }
}
