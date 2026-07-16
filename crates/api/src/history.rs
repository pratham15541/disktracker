use rusqlite::Connection;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub sequence: i64,
    pub volume: String,
    pub file_id: u64,
    pub parent_file_id: u64,
    pub name: String,
    pub kind: String,
    pub is_directory: bool,
    pub size_delta: i64,
    pub at: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryResponse {
    pub results: Vec<HistoryEntry>,
    pub truncated: bool,
    pub cursor: Option<String>,
    pub next_cursor: Option<String>,
    pub prev_cursor: Option<String>,
    pub debug_info: Option<String>,
}

fn parse_path(path: &str) -> Result<(String, String), String> {
    let normalized = path.replace('\\', "/");
    // Extract Windows drive letter, e.g. "C:/path" or "C:path"
    if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        let drive = normalized[0..2].to_uppercase();
        let remaining = normalized[2..].trim_start_matches('/').to_string();
        return Ok((drive, remaining));
    }
    // Extract mock Unix path, e.g. "/tmp/disktracker_mock_C/path"
    if let Some(without_prefix) = normalized.strip_prefix("/tmp/disktracker_mock_") {
        if let Some(slash_idx) = without_prefix.find('/') {
            let drive_letter = &without_prefix[..slash_idx];
            let drive = if drive_letter.ends_with(':') {
                drive_letter.to_uppercase()
            } else {
                format!("{}:", drive_letter.to_uppercase())
            };
            let remaining = without_prefix[slash_idx..]
                .trim_start_matches('/')
                .to_string();
            return Ok((drive, remaining));
        } else {
            let drive_letter = without_prefix;
            let drive = if drive_letter.ends_with(':') {
                drive_letter.to_uppercase()
            } else {
                format!("{}:", drive_letter.to_uppercase())
            };
            return Ok((drive, String::new()));
        }
    }
    Err(format!("Could not extract volume from path: {}", path))
}

fn get_root_file_id(conn: &Connection, volume: &str) -> Result<u64, String> {
    let res: Result<u64, rusqlite::Error> = conn.query_row(
        "SELECT file_id FROM facts WHERE volume = ?1 AND parent_file_id = file_id",
        rusqlite::params![volume],
        |row| row.get(0),
    );
    match res {
        Ok(fid) => Ok(fid),
        Err(_) => {
            let res2: Result<u64, rusqlite::Error> = conn.query_row(
                "SELECT file_id FROM facts WHERE volume = ?1 AND (name = ?1 OR name = ?2)",
                rusqlite::params![volume, volume.trim_end_matches(':')],
                |row| row.get(0),
            );
            res2.map_err(|e| format!("Root file_id not found for volume {}: {}", volume, e))
        }
    }
}

fn verify_parent_chain(
    conn: &Connection,
    volume: &str,
    start_parent_id: u64,
    expected_parents: &[&str],
) -> bool {
    if expected_parents.is_empty() {
        if let Ok(root_id) = get_root_file_id(conn, volume) {
            return start_parent_id == root_id;
        }
        return false;
    }

    let mut current_id = start_parent_id;
    let mut expected_idx = expected_parents.len() - 1;

    loop {
        let name_res: Result<(u64, String), rusqlite::Error> = conn.query_row(
            "SELECT parent_file_id, name FROM facts WHERE volume = ?1 AND file_id = ?2",
            rusqlite::params![volume, current_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).or_else(|_| {
            conn.query_row(
                "SELECT parent_file_id, name FROM mutation_log WHERE volume = ?1 AND file_id = ?2 ORDER BY sequence DESC LIMIT 1",
                rusqlite::params![volume, current_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
        });

        match name_res {
            Ok((parent_id, name)) => {
                if name.eq_ignore_ascii_case(expected_parents[expected_idx]) {
                    if expected_idx == 0 {
                        return true;
                    }
                    expected_idx -= 1;
                    current_id = parent_id;
                } else {
                    return false;
                }
            }
            Err(_) => {
                return false;
            }
        }
    }
}

pub fn resolve_path_to_id(conn: &Connection, path: &str) -> Result<(String, u64), String> {
    let (volume, remaining) = parse_path(path)?;
    if remaining.is_empty() {
        let root_id = get_root_file_id(conn, &volume)?;
        return Ok((volume, root_id));
    }

    let parts: Vec<&str> = remaining.split('/').filter(|s| !s.is_empty()).collect();

    // First, try resolving via current facts table (live files/folders)
    let mut current_id = get_root_file_id(conn, &volume)?;
    let mut facts_resolved = true;
    for part in &parts {
        let next_res: Result<u64, rusqlite::Error> = conn.query_row(
            "SELECT file_id FROM facts WHERE volume = ?1 AND parent_file_id = ?2 AND name = ?3 COLLATE NOCASE",
            rusqlite::params![&volume, current_id, part],
            |row| row.get(0),
        );
        match next_res {
            Ok(fid) => {
                current_id = fid;
            }
            Err(_) => {
                facts_resolved = false;
                break;
            }
        }
    }

    if facts_resolved {
        return Ok((volume, current_id));
    }

    // Fallback: search mutation_log for candidates of the final part
    let last_part = parts.last().ok_or("Empty path")?;
    let mut stmt = conn.prepare(
        "SELECT DISTINCT file_id, parent_file_id FROM mutation_log WHERE volume = ?1 AND name = ?2 COLLATE NOCASE"
    ).map_err(|e| e.to_string())?;

    let candidates = stmt
        .query_map(rusqlite::params![&volume, last_part], |row| {
            Ok((row.get::<_, u64>(0)?, row.get::<_, u64>(1)?))
        })
        .map_err(|e| e.to_string())?;

    for (fid, parent_id) in candidates.flatten() {
        if verify_parent_chain(conn, &volume, parent_id, &parts[0..parts.len() - 1]) {
            return Ok((volume, fid));
        }
    }

    Err(format!("Path not found: {}", path))
}

#[allow(clippy::too_many_arguments)]
pub fn get_history(
    conn: &Connection,
    path: &str,
    since: Option<i64>,
    until: Option<i64>,
    kind: Option<&str>,
    collapse: bool,
    limit: usize,
    cursor: Option<&str>,
) -> Result<HistoryResponse, String> {
    let (volume, file_id) = resolve_path_to_id(conn, path)?;

    let is_dir: bool = conn
        .query_row(
            "SELECT is_directory FROM facts WHERE volume = ?1 AND file_id = ?2",
            rusqlite::params![&volume, file_id],
            |row| row.get(0),
        )
        .unwrap_or(false);

    let is_dir = if is_dir {
        true
    } else {
        conn.query_row(
            "SELECT is_directory FROM mutation_log WHERE volume = ?1 AND file_id = ?2 LIMIT 1",
            rusqlite::params![&volume, file_id],
            |row| Ok(row.get::<_, i32>(0)? != 0),
        )
        .unwrap_or(false)
    };

    // 1. Check truncation based on retention config
    let config = config_mgr::load_config();
    let duration = match config_mgr::parse_duration(&config.retention) {
        Ok(dur) => dur,
        Err(_) => chrono::Duration::days(30),
    };
    let cutoff = chrono::Utc::now() - duration;

    let truncated = if let Some(since_ts) = since {
        since_ts < cutoff.timestamp()
    } else {
        let oldest_at_res: Result<Option<String>, rusqlite::Error> = conn.query_row(
            "SELECT MIN(at) FROM mutation_log WHERE volume = ?1",
            rusqlite::params![&volume],
            |row| row.get(0),
        );
        if let Ok(Some(oldest_at_str)) = oldest_at_res {
            if let Ok(oldest_dt) = chrono::DateTime::parse_from_rfc3339(&oldest_at_str) {
                oldest_dt.timestamp() <= (cutoff + chrono::Duration::minutes(5)).timestamp()
            } else {
                false
            }
        } else {
            false
        }
    };

    // 2. Build history query
    let mut query = String::from(
        "SELECT sequence, volume, file_id, parent_file_id, name, kind, is_directory, size_delta, at, source
         FROM mutation_log
         WHERE volume = ?1"
    );
    if is_dir {
        query.push_str(" AND (file_id = ?2 OR parent_file_id = ?2)");
    } else {
        query.push_str(" AND file_id = ?2");
    }
    let mut params = vec![
        &volume as &dyn rusqlite::ToSql,
        &file_id as &dyn rusqlite::ToSql,
    ];

    // Store string conversions in boxed slots to extend their lifetime for parameters iterator
    let mut dt_strings = Vec::new();
    let kind_string = kind.map(|k| k.to_string());
    let mut is_backward = false;
    let mut cursor_val = None;
    if let Some(c) = cursor {
        if let Some(rest) = c.strip_prefix("f:") {
            cursor_val = rest.parse::<i64>().ok();
        } else if let Some(rest) = c.strip_prefix("b:") {
            is_backward = true;
            cursor_val = rest.parse::<i64>().ok();
        } else {
            cursor_val = c.parse::<i64>().ok();
        }
    }

    if let Some(ref since_val) = since {
        let dt = chrono::DateTime::from_timestamp(*since_val, 0)
            .unwrap_or_default()
            .to_rfc3339();
        dt_strings.push(dt);
    }
    if let Some(ref until_val) = until {
        let dt = chrono::DateTime::from_timestamp(*until_val, 0)
            .unwrap_or_default()
            .to_rfc3339();
        dt_strings.push(dt);
    }

    let mut dt_iter = dt_strings.iter();

    if since.is_some() {
        if let Some(dt) = dt_iter.next() {
            params.push(dt as &dyn rusqlite::ToSql);
            query.push_str(&format!(" AND at >= ?{}", params.len()));
        }
    }

    if until.is_some() {
        if let Some(dt) = dt_iter.next() {
            params.push(dt as &dyn rusqlite::ToSql);
            query.push_str(&format!(" AND at <= ?{}", params.len()));
        }
    }

    if let Some(ref kind_str) = kind_string {
        params.push(kind_str as &dyn rusqlite::ToSql);
        query.push_str(&format!(" AND kind = ?{} COLLATE NOCASE", params.len()));
    }

    if let Some(ref cursor_seq) = cursor_val {
        params.push(cursor_seq as &dyn rusqlite::ToSql);
        if is_backward {
            query.push_str(&format!(" AND sequence < ?{}", params.len()));
        } else {
            query.push_str(&format!(" AND sequence > ?{}", params.len()));
        }
    }

    if is_backward {
        query.push_str(" ORDER BY sequence DESC");
    } else {
        query.push_str(" ORDER BY sequence ASC");
    }

    // Fetch extra rows so consecutive aggregation still fills the requested limit
    let fetch_limit = if collapse { limit * 4 } else { limit * 2 };
    let limit_val = fetch_limit + 1;
    params.push(&limit_val as &dyn rusqlite::ToSql);
    query.push_str(&format!(" LIMIT ?{}", params.len()));

    let mut stmt = conn.prepare(&query).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params), |row| {
            Ok(HistoryEntry {
                sequence: row.get(0)?,
                volume: row.get(1)?,
                file_id: row.get(2)?,
                parent_file_id: row.get(3)?,
                name: row.get(4)?,
                kind: row.get(5)?,
                is_directory: row.get::<_, i32>(6)? != 0,
                size_delta: row.get(7)?,
                at: row.get(8)?,
                source: row.get(9)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut db_entries: Vec<HistoryEntry> = rows.flatten().collect();
    let has_more = db_entries.len() > fetch_limit;
    if has_more {
        db_entries.truncate(fetch_limit);
    }

    if is_backward {
        db_entries.reverse();
    }

    let mut entries = db_entries;

    // Aggregate consecutive identical display rows (Created/Deleted/Renamed of the
    // same file). Modified stays unique. With --collapse, also merge consecutive
    // same-kind same-file rows (including Modified size_deltas).
    {
        let mut collapsed = Vec::with_capacity(entries.len());
        let mut iter = entries.into_iter();
        if let Some(first) = iter.next() {
            let mut current = first;
            for next in iter {
                let same_display = crate::mutation_filter::can_aggregate_history_entries(
                    &current.kind,
                    current.file_id,
                    &current.name,
                    &next.kind,
                    next.file_id,
                    &next.name,
                );
                let collapse_same_file =
                    collapse && current.kind == next.kind && current.file_id == next.file_id;
                if same_display || collapse_same_file {
                    current.size_delta += next.size_delta;
                    current.sequence = next.sequence;
                    current.at = next.at;
                    current.name = next.name;
                    current.parent_file_id = next.parent_file_id;
                    current.source = next.source;
                } else {
                    collapsed.push(current);
                    current = next;
                }
            }
            collapsed.push(current);
        }
        entries = collapsed;
    }

    // Created/Deleted/Renamed (incl. 0B) always shown; Modified only if |delta| >= 2
    entries.retain(|e| crate::mutation_filter::is_displayable_mutation(&e.kind, e.size_delta));

    let collapsed_has_more = entries.len() > limit || has_more;
    if entries.len() > limit {
        entries.truncate(limit);
    }

    let mut next_cursor = None;
    let mut prev_cursor = None;

    if !entries.is_empty() {
        let s_first = entries.first().unwrap().sequence;
        let s_last = entries.last().unwrap().sequence;

        if !is_backward {
            if cursor.is_some() {
                prev_cursor = Some(format!("b:{}", s_first));
            }
            if collapsed_has_more {
                next_cursor = Some(format!("f:{}", s_last));
            }
        } else {
            if collapsed_has_more {
                prev_cursor = Some(format!("b:{}", s_first));
            }
            next_cursor = Some(format!("f:{}", s_last));
        }
    }

    let total_mutations: i64 = conn
        .query_row("SELECT COUNT(*) FROM mutation_log", [], |row| row.get(0))
        .unwrap_or(0);
    let matched_mutations: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM mutation_log WHERE volume = ?1 AND file_id = ?2",
            rusqlite::params![&volume, file_id],
            |row| row.get(0),
        )
        .unwrap_or(0);
    let parent_matched: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM mutation_log WHERE volume = ?1 AND parent_file_id = ?2",
            rusqlite::params![&volume, file_id],
            |row| row.get(0),
        )
        .unwrap_or(0);
    let debug_info = format!(
        "Path: '{}', Resolved: ({}, file_id: {}), is_dir: {}, Total mutations in DB: {}, Matches for file_id: {}, Matches for parent_file_id: {}",
        path, volume, file_id, is_dir, total_mutations, matched_mutations, parent_matched
    );

    Ok(HistoryResponse {
        results: entries,
        truncated,
        cursor: next_cursor.clone(),
        next_cursor,
        prev_cursor,
        debug_info: Some(debug_info),
    })
}
