use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use storage;
use crate::history;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopItem {
    pub name: String,
    pub volume: String,
    pub path: String, // relative path excluding volume
    pub is_directory: bool,
    pub size: u64,           // Mode A
    pub size_delta: i64,     // Mode B/C growth
    pub churn: u64,          // Mode B/C churn
    pub item_count: u64,     // rollup count
    pub file_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopResponse {
    pub results: Vec<TopItem>,
    pub volumes_incomplete: Vec<String>,
    pub next_cursor: Option<String>,
    pub window_start: Option<String>, // Bound description (e.g. sequence, snapshot label, date)
    pub window_end: Option<String>,
}

// Simple Base64 encoder/decoder to avoid adding new dependencies
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    let mut i = 0;
    while i < data.len() {
        let b0 = data[i];
        let b1 = if i + 1 < data.len() { data[i + 1] } else { 0 };
        let b2 = if i + 2 < data.len() { data[i + 2] } else { 0 };

        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);

        let c0 = ALPHABET[((n >> 18) & 63) as usize] as char;
        let c1 = ALPHABET[((n >> 12) & 63) as usize] as char;
        let c2 = ALPHABET[((n >> 6) & 63) as usize] as char;
        let c3 = ALPHABET[(n & 63) as usize] as char;

        result.push(c0);
        result.push(c1);
        if i + 1 < data.len() {
            result.push(c2);
        } else {
            result.push('=');
        }
        if i + 2 < data.len() {
            result.push(c3);
        } else {
            result.push('=');
        }
        i += 3;
    }
    result
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (idx, &byte) in ALPHABET.iter().enumerate() {
        lookup[byte as usize] = idx as u8;
    }

    let bytes = s.as_bytes();
    if bytes.len() % 4 != 0 {
        return None;
    }

    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b0 = lookup[bytes[i] as usize];
        let b1 = lookup[bytes[i + 1] as usize];
        let b2 = if bytes[i + 2] == b'=' { 0 } else { lookup[bytes[i + 2] as usize] };
        let b3 = if bytes[i + 3] == b'=' { 0 } else { lookup[bytes[i + 3] as usize] };

        if b0 == 255 || b1 == 255 || (bytes[i + 2] != b'=' && b2 == 255) || (bytes[i + 3] != b'=' && b3 == 255) {
            return None;
        }

        let n = ((b0 as u32) << 18) | ((b1 as u32) << 12) | ((b2 as u32) << 6) | (b3 as u32);
        result.push((n >> 16) as u8);
        if bytes[i + 2] != b'=' {
            result.push((n >> 8) as u8);
        }
        if bytes[i + 3] != b'=' {
            result.push(n as u8);
        }
        i += 4;
    }
    Some(result)
}

fn is_descendant_u64(
    parent_map: &HashMap<u64, u64>,
    file_id: u64,
    ancestor_id: u64,
) -> bool {
    if file_id == ancestor_id {
        return true;
    }
    let mut current_id = file_id;
    let mut visited = HashSet::new();
    visited.insert(current_id);
    
    while current_id != 0 {
        if let Some(&p) = parent_map.get(&current_id) {
            if p == ancestor_id {
                return true;
            }
            if p == current_id || p == 0 || !visited.insert(p) {
                break;
            }
            current_id = p;
        } else {
            break;
        }
    }
    false
}

fn get_fact_path_local_u64(
    parent_map: &HashMap<u64, u64>,
    name_map: &HashMap<u64, String>,
    volume: &str,
    file_id: u64,
) -> String {
    let mut current_id = file_id;
    let mut parts = Vec::new();
    let mut visited = HashSet::new();

    loop {
        if !visited.insert(current_id) {
            break;
        }

        if let Some(&parent_id) = parent_map.get(&current_id) {
            if let Some(name) = name_map.get(&current_id) {
                if name.is_empty() || name == volume {
                    break;
                }
                if current_id != file_id {
                    parts.push(name.clone());
                }
                if parent_id == current_id || parent_id == 0 {
                    break;
                }
                current_id = parent_id;
            } else {
                break;
            }
        } else {
            break;
        }
    }

    parts.reverse();
    parts.join("/")
}

pub fn handle_get_top(params: Value) -> Result<Value, String> {
    let path_filter = params.get("path").and_then(|p| p.as_str());
    let volume_filter = params.get("volume").and_then(|v| v.as_str()).map(|s| s.to_uppercase());
    let folders = params.get("folders").and_then(|f| f.as_bool()).unwrap_or(false);
    let files = params.get("files").and_then(|f| f.as_bool()).unwrap_or(false);
    let limit = params.get("limit").and_then(|l| l.as_u64()).unwrap_or(20) as usize;
    let since = params.get("since").and_then(|s| s.as_i64());
    let between_a = params.get("between_a").and_then(|s| s.as_str());
    let between_b = params.get("between_b").and_then(|s| s.as_str());
    let growth = params.get("growth").and_then(|g| g.as_bool()).unwrap_or(false);
    let churn = params.get("churn").and_then(|c| c.as_bool()).unwrap_or(false);
    let cursor = params.get("cursor").and_then(|c| c.as_str());

    let conn = storage::get_db_connection().map_err(|e| e.to_string())?;

    let mut volumes_incomplete = Vec::new();
    let registered_volumes = core_types::get_registered_volumes();
    for vol in registered_volumes {
        let tracker = core_types::get_volume_tracker(&vol);
        let state = { *tracker.state.lock().unwrap() };
        if state != core_types::DaemonState::Live {
            volumes_incomplete.push(vol);
        }
    }

    let mut filter_volume = volume_filter;
    let mut filter_path_fid = None;

    if let Some(path) = path_filter {
        let (vol, fid) = history::resolve_path_to_id(&conn, path)?;
        if let Some(ref f_vol) = filter_volume {
            if f_vol != &vol {
                return Err("E_INVALID_PARAMS: Path filter and volume filter do not match.".to_string());
            }
        }
        filter_volume = Some(vol);
        filter_path_fid = Some(fid);
    }

    let rollup_folders = if folders {
        true
    } else if files {
        false
    } else {
        since.is_some() || between_a.is_some()
    };

    let is_interval_mode = since.is_some() || between_a.is_some();

    if since.is_some() && (between_a.is_some() || between_b.is_some()) {
        return Err("E_INVALID_PARAMS: Parameters '--since' and '--between' are mutually exclusive.".to_string());
    }
    if growth && churn {
        return Err("E_INVALID_PARAMS: Parameters '--growth' and '--churn' are mutually exclusive.".to_string());
    }
    if folders && files {
        return Err("E_INVALID_PARAMS: Parameters '--folders' and '--files' are mutually exclusive.".to_string());
    }

    let mut all_temp_items = Vec::new();
    let mut window_start = None;
    let mut window_end = None;
    let mut volume_parent_maps = HashMap::new();
    let mut volume_name_maps = HashMap::new();

    let target_vols = if let Some(ref vol) = filter_volume {
        vec![vol.clone()]
    } else {
        core_types::get_registered_volumes()
    };

    if !is_interval_mode {
        // Mode A: Current Size
        for vol in &target_vols {
            let mut parent_map = HashMap::new();
            let mut name_map = HashMap::new();
            let mut file_sizes = Vec::new();
            let mut dir_ids = HashSet::new();

            if rollup_folders {
                let mut stmt_dirs = conn.prepare(
                    "SELECT file_id, parent_file_id, name FROM facts WHERE volume = ?1 AND is_directory = 1"
                ).map_err(|e| e.to_string())?;

                let dir_rows = stmt_dirs.query_map(rusqlite::params![vol], |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                }).map_err(|e| e.to_string())?;

                for r in dir_rows {
                    let (fid, parent_fid, name) = r.map_err(|e| e.to_string())?;
                    parent_map.insert(fid, parent_fid);
                    name_map.insert(fid, name);
                    dir_ids.insert(fid);
                }

                let mut stmt_files = conn.prepare(
                    "SELECT file_id, parent_file_id, size FROM facts WHERE volume = ?1 AND is_directory = 0"
                ).map_err(|e| e.to_string())?;

                let file_rows = stmt_files.query_map(rusqlite::params![vol], |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, u64>(2)?,
                    ))
                }).map_err(|e| e.to_string())?;

                for r in file_rows {
                    let (fid, parent_fid, size) = r.map_err(|e| e.to_string())?;
                    parent_map.insert(fid, parent_fid);
                    file_sizes.push((fid, size));
                }
            } else {
                let mut stmt_dirs = conn.prepare(
                    "SELECT file_id, parent_file_id FROM facts WHERE volume = ?1 AND is_directory = 1"
                ).map_err(|e| e.to_string())?;

                let dir_rows = stmt_dirs.query_map(rusqlite::params![vol], |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, u64>(1)?,
                    ))
                }).map_err(|e| e.to_string())?;

                for r in dir_rows {
                    let (fid, parent_fid) = r.map_err(|e| e.to_string())?;
                    parent_map.insert(fid, parent_fid);
                }

                let mut stmt_files = conn.prepare(
                    "SELECT file_id, parent_file_id, name, size FROM facts WHERE volume = ?1 AND is_directory = 0"
                ).map_err(|e| e.to_string())?;

                let file_rows = stmt_files.query_map(rusqlite::params![vol], |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, u64>(3)?,
                    ))
                }).map_err(|e| e.to_string())?;

                for r in file_rows {
                    let (fid, parent_fid, name, size) = r.map_err(|e| e.to_string())?;
                    parent_map.insert(fid, parent_fid);
                    name_map.insert(fid, name);
                    file_sizes.push((fid, size));
                }
            }

            if rollup_folders {
                let mut folder_sizes = HashMap::new();
                let mut folder_item_counts = HashMap::new();

                for &(file_id, size) in &file_sizes {
                    let mut current_parent = parent_map.get(&file_id).copied().unwrap_or(0);
                    let mut visited = HashSet::new();
                    visited.insert(file_id);

                    while current_parent != 0 && current_parent != file_id && visited.insert(current_parent) {
                        if parent_map.contains_key(&current_parent) {
                            *folder_sizes.entry(current_parent).or_insert(0) += size;
                            *folder_item_counts.entry(current_parent).or_insert(0) += 1;
                            current_parent = parent_map.get(&current_parent).copied().unwrap_or(0);
                        } else {
                            break;
                        }
                    }
                }

                for &fid in &dir_ids {
                    if let Some(ancestor_id) = filter_path_fid {
                        if !is_descendant_u64(&parent_map, fid, ancestor_id) {
                            continue;
                        }
                    }

                    let size = folder_sizes.get(&fid).copied().unwrap_or(0);
                    let item_count = folder_item_counts.get(&fid).copied().unwrap_or(0);
                    let name = name_map.get(&fid).cloned().unwrap_or_default();

                    all_temp_items.push(TopItem {
                        name,
                        volume: vol.clone(),
                        path: String::new(),
                        is_directory: true,
                        size,
                        size_delta: 0,
                        churn: 0,
                        item_count,
                        file_id: fid,
                    });
                }
            } else {
                // Files mode
                for &(fid, size) in &file_sizes {
                    if let Some(ancestor_id) = filter_path_fid {
                        if !is_descendant_u64(&parent_map, fid, ancestor_id) {
                            continue;
                        }
                    }

                    let name = name_map.get(&fid).cloned().unwrap_or_default();

                    all_temp_items.push(TopItem {
                        name,
                        volume: vol.clone(),
                        path: String::new(),
                        is_directory: false,
                        size,
                        size_delta: 0,
                        churn: 0,
                        item_count: 0,
                        file_id: fid,
                    });
                }
            }

            volume_parent_maps.insert(vol.clone(), parent_map);
            volume_name_maps.insert(vol.clone(), name_map);
        }

        all_temp_items.sort_by(|a, b| {
            b.size.cmp(&a.size)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| b.file_id.cmp(&a.file_id))
        });
    } else {
        // Mode B/C: Growth/Churn over Interval
        let mut volume_intervals = Vec::new();

        if let Some(since_ts) = since {
            let now = chrono::Utc::now();
            let needed_secs = (now.timestamp() - since_ts) as f64;
            
            let mut check_vols = Vec::new();
            if let Some(ref vol) = filter_volume {
                check_vols.push(vol.clone());
            } else {
                check_vols.extend(core_types::get_registered_volumes());
            }

            for vol in check_vols {
                let min_at_str: Option<String> = conn
                    .query_row(
                        "SELECT MIN(at) FROM mutation_log WHERE volume = ?1",
                        rusqlite::params![vol],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .ok()
                    .flatten();

                let available_secs = if let Some(ref at_str) = min_at_str {
                    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(at_str) {
                        now.signed_duration_since(dt.with_timezone(&chrono::Utc)).num_seconds() as f64
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };

                let days_available = available_secs / 86400.0;
                let days_needed = needed_secs / 86400.0;

                if days_available < days_needed {
                    return Err(format!(
                        "E_INSUFFICIENT_HISTORY:{:.4}:{:.4}",
                        days_available,
                        days_needed
                    ));
                }
            }

            let since_rfc = chrono::DateTime::from_timestamp(since_ts, 0)
                .unwrap_or_default()
                .to_rfc3339();

            window_start = Some(since_rfc.clone());
            window_end = Some(now.to_rfc3339());

            for vol in target_vols.iter() {
                let seq_start: i64 = conn.query_row(
                    "SELECT COALESCE(MIN(sequence), 0) FROM mutation_log WHERE volume = ?1 AND at >= ?2",
                    rusqlite::params![vol, since_rfc],
                    |row| row.get(0),
                ).unwrap_or(0);

                let seq_end: i64 = conn.query_row(
                    "SELECT COALESCE(MAX(sequence), 0) FROM mutation_log WHERE volume = ?1",
                    rusqlite::params![vol],
                    |row| row.get(0),
                ).unwrap_or(0);

                volume_intervals.push((vol.clone(), seq_start, seq_end));
            }
        } else if let Some(b_a) = between_a {
            let b_b = between_b.ok_or("Missing parameter between_b")?;
            
            let parent_a = storage::get_parent_snapshot_by_label_or_id(b_a)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("E_NOT_FOUND: Snapshot not found: {}", b_a))?;
            let parent_b = storage::get_parent_snapshot_by_label_or_id(b_b)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("E_NOT_FOUND: Snapshot not found: {}", b_b))?;

            window_start = Some(parent_a.label.clone());
            window_end = Some(parent_b.label.clone());

            let comp_vols = if let Some(ref vol) = filter_volume {
                vec![vol.clone()]
            } else {
                let mut vols = Vec::new();
                for va in &parent_a.volumes {
                    if parent_b.volumes.iter().any(|vb| vb.volume == va.volume) {
                        vols.push(va.volume.clone());
                    }
                }
                vols
            };

            for vol in comp_vols {
                let seq_a = parent_a.volumes.iter().find(|v| v.volume == vol)
                    .map(|v| v.sequence_number)
                    .ok_or_else(|| format!("E_NOT_FOUND: Volume {} not found in snapshot {}", vol, parent_a.label))?;
                let seq_b = parent_b.volumes.iter().find(|v| v.volume == vol)
                    .map(|v| v.sequence_number)
                    .ok_or_else(|| format!("E_NOT_FOUND: Volume {} not found in snapshot {}", vol, parent_b.label))?;
                
                let (start, end) = if seq_a <= seq_b { (seq_a, seq_b) } else { (seq_b, seq_a) };
                
                let min_seq: i64 = conn.query_row(
                    "SELECT COALESCE(MIN(sequence), 0) FROM mutation_log WHERE volume = ?1",
                    rusqlite::params![vol],
                    |row| row.get(0),
                ).unwrap_or(0);

                if start < min_seq && min_seq > 1 {
                    let config = config_mgr::load_config();
                    return Err(format!(
                        "E_SNAPSHOT_DATA_EXPIRED: This comparison goes further back than DiskTracker currently keeps ({}). Try a more recent snapshot pair.",
                        config.retention
                    ));
                }

                volume_intervals.push((vol, start, end));
            }
        }

        for vol in &target_vols {
            let mut parent_map = HashMap::new();
            let mut is_dir_map = HashMap::new();
            let mut name_map = HashMap::new();

            let mut stmt = conn.prepare(
                "SELECT file_id, parent_file_id, name, is_directory FROM facts WHERE volume = ?1"
            ).map_err(|e| e.to_string())?;

            let rows = stmt.query_map(rusqlite::params![vol], |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i32>(3)? != 0,
                ))
            }).map_err(|e| e.to_string())?;

            for r in rows {
                let (fid, parent_fid, name, is_dir) = r.map_err(|e| e.to_string())?;
                parent_map.insert(fid, parent_fid);
                is_dir_map.insert(fid, is_dir);
                name_map.insert(fid, name);
            }

            let mut direct_growth = HashMap::new();
            let mut direct_churn = HashMap::new();

            if let Some(&(_, start_seq, end_seq)) = volume_intervals.iter().find(|i| &i.0 == vol) {
                let mut mut_stmt = conn.prepare(
                    "SELECT file_id, parent_file_id, name, kind, is_directory, size_delta 
                     FROM mutation_log 
                     WHERE volume = ?1 AND sequence > ?2 AND sequence <= ?3
                     ORDER BY sequence ASC"
                ).map_err(|e| e.to_string())?;

                let mut_rows = mut_stmt.query_map(
                    rusqlite::params![vol, start_seq, end_seq],
                    |row| {
                        Ok((
                            row.get::<_, u64>(0)?,
                            row.get::<_, u64>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, i32>(4)? != 0,
                            row.get::<_, i64>(5)?,
                        ))
                    }
                ).map_err(|e| e.to_string())?;

                for r in mut_rows {
                    let (fid, parent_fid, name, _kind, is_dir, size_delta) = r.map_err(|e| e.to_string())?;
                    *direct_growth.entry(fid).or_insert(0) += size_delta;
                    *direct_churn.entry(fid).or_insert(0) += 1;

                    parent_map.insert(fid, parent_fid);
                    is_dir_map.insert(fid, is_dir);
                    name_map.insert(fid, name);
                }
            }

            if rollup_folders {
                let mut rolled_growth: HashMap<u64, i64> = HashMap::new();
                let mut rolled_churn: HashMap<u64, u64> = HashMap::new();
                let mut rolled_item_count: HashMap<u64, u64> = HashMap::new();

                for (&file_id, &growth_val) in &direct_growth {
                    let churn_val = *direct_churn.get(&file_id).unwrap_or(&0);

                    let mut current_parent = parent_map.get(&file_id).copied().unwrap_or(0);
                    let mut visited = HashSet::new();
                    visited.insert(file_id);

                    while current_parent != 0 && current_parent != file_id && visited.insert(current_parent) {
                        *rolled_growth.entry(current_parent).or_insert(0) += growth_val;
                        *rolled_churn.entry(current_parent).or_insert(0) += churn_val;
                        *rolled_item_count.entry(current_parent).or_insert(0) += 1;

                        current_parent = parent_map.get(&current_parent).copied().unwrap_or(0);
                    }
                }

                for (&fid, &is_dir) in &is_dir_map {
                    if is_dir {
                        if let Some(ancestor_id) = filter_path_fid {
                            if !is_descendant_u64(&parent_map, fid, ancestor_id) {
                                continue;
                            }
                        }

                        let size_delta = *rolled_growth.get(&fid).unwrap_or(&0);
                        let churn_val = *rolled_churn.get(&fid).unwrap_or(&0);
                        let item_count = *rolled_item_count.get(&fid).unwrap_or(&0);

                        if size_delta == 0 && churn_val == 0 {
                            continue;
                        }

                        let name = name_map.get(&fid).cloned().unwrap_or_default();

                        all_temp_items.push(TopItem {
                            name,
                            volume: vol.clone(),
                            path: String::new(),
                            is_directory: true,
                            size: 0,
                            size_delta,
                            churn: churn_val,
                            item_count,
                            file_id: fid,
                        });
                    }
                }
            } else {
                // Files mode
                for (&fid, &is_dir) in &is_dir_map {
                    if !is_dir {
                        if let Some(ancestor_id) = filter_path_fid {
                            if !is_descendant_u64(&parent_map, fid, ancestor_id) {
                                continue;
                            }
                        }

                        let size_delta = *direct_growth.get(&fid).unwrap_or(&0);
                        let churn_val = *direct_churn.get(&fid).unwrap_or(&0);

                        if size_delta == 0 && churn_val == 0 {
                            continue;
                        }

                        let name = name_map.get(&fid).cloned().unwrap_or_default();

                        all_temp_items.push(TopItem {
                            name,
                            volume: vol.clone(),
                            path: String::new(),
                            is_directory: false,
                            size: 0,
                            size_delta,
                            churn: churn_val,
                            item_count: 0,
                            file_id: fid,
                        });
                    }
                }
            }

            volume_parent_maps.insert(vol.clone(), parent_map);
            volume_name_maps.insert(vol.clone(), name_map);
        }

        if churn {
            all_temp_items.sort_by(|a, b| {
                b.churn.cmp(&a.churn)
                    .then_with(|| a.name.cmp(&b.name))
                    .then_with(|| b.file_id.cmp(&a.file_id))
            });
        } else {
            all_temp_items.sort_by(|a, b| {
                b.size_delta.abs().cmp(&a.size_delta.abs())
                    .then_with(|| a.name.cmp(&b.name))
                    .then_with(|| b.file_id.cmp(&a.file_id))
            });
        }
    }

    let mut cursor_val = None;
    let mut cursor_volume = None;
    let mut cursor_file_id = None;

    if let Some(c) = cursor {
        if let Some(decoded_bytes) = base64_decode(c) {
            if let Ok(decoded_str) = String::from_utf8(decoded_bytes) {
                let parts: Vec<&str> = decoded_str.split(':').collect();
                if parts.len() == 3 {
                    cursor_val = parts[0].parse::<i64>().ok();
                    cursor_volume = Some(parts[1].to_string());
                    cursor_file_id = parts[2].parse::<u64>().ok();
                }
            }
        }
    }

    let cursor_exists = if let (Some(c_val), Some(ref c_vol), Some(c_fid)) = (cursor_val, &cursor_volume, cursor_file_id) {
        all_temp_items.iter().any(|item| {
            let item_val = if !is_interval_mode {
                item.size as i64
            } else if churn {
                item.churn as i64
            } else {
                item.size_delta
            };
            item_val == c_val && &item.volume == c_vol && item.file_id == c_fid
        })
    } else {
        false
    };

    let mut page_items = Vec::new();
    let mut accepted_folders = HashSet::new();
    let mut next_cursor = None;
    let mut found_cursor = cursor.is_none() || !cursor_exists;

    for item in all_temp_items {
        if rollup_folders && item.is_directory {
            let mut is_dup = false;
            if let Some(parent_map) = volume_parent_maps.get(&item.volume) {
                for &(ref acc_vol, acc_fid) in &accepted_folders {
                    if acc_vol == &item.volume {
                        if is_descendant_u64(parent_map, item.file_id, acc_fid) {
                            is_dup = true;
                            break;
                        }
                    }
                }
            }
            if is_dup {
                continue;
            }
            accepted_folders.insert((item.volume.clone(), item.file_id));
        }

        if !found_cursor {
            let item_val = if !is_interval_mode {
                item.size as i64
            } else if churn {
                item.churn as i64
            } else {
                item.size_delta
            };

            let matches_cursor = Some(item_val) == cursor_val
                && Some(&item.volume) == cursor_volume.as_ref()
                && Some(item.file_id) == cursor_file_id;

            if matches_cursor {
                found_cursor = true;
            }
            continue;
        }

        if page_items.len() < limit {
            page_items.push(item);
        } else {
            let last_val = if !is_interval_mode {
                item.size as i64
            } else if churn {
                item.churn as i64
            } else {
                item.size_delta
            };
            let cursor_str = format!("{}:{}:{}", last_val, item.volume, item.file_id);
            next_cursor = Some(base64_encode(cursor_str.as_bytes()));
            break;
        }
    }

    for item in &mut page_items {
        if let Some(parent_map) = volume_parent_maps.get(&item.volume) {
            if let Some(name_map) = volume_name_maps.get(&item.volume) {
                item.path = get_fact_path_local_u64(parent_map, name_map, &item.volume, item.file_id);
            }
        }
    }

    Ok(serde_json::json!(TopResponse {
        results: page_items,
        volumes_incomplete,
        next_cursor,
        window_start,
        window_end,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    struct FactNode {
        volume: String,
        file_id: u64,
        parent_file_id: u64,
        name: String,
        is_directory: bool,
        size: u64,
    }

    fn get_fact_path_local(
        parent_map: &HashMap<(String, u64), u64>,
        name_map: &HashMap<(String, u64), String>,
        volume: &str,
        file_id: u64,
    ) -> String {
        let mut current_id = file_id;
        let mut parts = Vec::new();
        let mut visited = HashSet::new();

        loop {
            if !visited.insert((volume.to_string(), current_id)) {
                break;
            }

            if let Some(&parent_id) = parent_map.get(&(volume.to_string(), current_id)) {
                if let Some(name) = name_map.get(&(volume.to_string(), current_id)) {
                    if name.is_empty() || name == volume {
                        break;
                    }
                    if current_id != file_id {
                        parts.push(name.clone());
                    }
                    if parent_id == current_id || parent_id == 0 {
                        break;
                    }
                    current_id = parent_id;
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        parts.reverse();
        parts.join("/")
    }

    #[test]
    fn test_base64_encode_decode() {
        let original = b"1024:C::12345";
        let encoded = base64_encode(original);
        assert_ne!(encoded, "");
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    fn setup_mock_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE facts (
                volume TEXT NOT NULL,
                file_id INTEGER NOT NULL,
                parent_file_id INTEGER NOT NULL,
                name TEXT NOT NULL,
                is_directory INTEGER NOT NULL,
                size INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                modified_at TEXT NOT NULL,
                attributes INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (volume, file_id)
            )",
            [],
        ).unwrap();
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
    fn test_top_mode_a_files_and_rollup() {
        let conn = setup_mock_db();

        // Setup a directory tree:
        // C: (file_id 1, root)
        // C:/dir1 (file_id 2, parent 1)
        // C:/dir1/file1.txt (file_id 3, parent 2, size 100)
        // C:/dir1/file2.txt (file_id 4, parent 2, size 200)
        // C:/file3.txt (file_id 5, parent 1, size 50)
        conn.execute(
            "INSERT INTO facts (volume, file_id, parent_file_id, name, is_directory, size, created_at, modified_at)
             VALUES ('C:', 1, 1, 'C:', 1, 0, '2026-07-12T12:00:00Z', '2026-07-12T12:00:00Z')",
            []
        ).unwrap();
        conn.execute(
            "INSERT INTO facts (volume, file_id, parent_file_id, name, is_directory, size, created_at, modified_at)
             VALUES ('C:', 2, 1, 'dir1', 1, 0, '2026-07-12T12:00:00Z', '2026-07-12T12:00:00Z')",
            []
        ).unwrap();
        conn.execute(
            "INSERT INTO facts (volume, file_id, parent_file_id, name, is_directory, size, created_at, modified_at)
             VALUES ('C:', 3, 2, 'file1.txt', 0, 100, '2026-07-12T12:00:00Z', '2026-07-12T12:00:00Z')",
            []
        ).unwrap();
        conn.execute(
            "INSERT INTO facts (volume, file_id, parent_file_id, name, is_directory, size, created_at, modified_at)
             VALUES ('C:', 4, 2, 'file2.txt', 0, 200, '2026-07-12T12:00:00Z', '2026-07-12T12:00:00Z')",
            []
        ).unwrap();
        conn.execute(
            "INSERT INTO facts (volume, file_id, parent_file_id, name, is_directory, size, created_at, modified_at)
             VALUES ('C:', 5, 1, 'file3.txt', 0, 50, '2026-07-12T12:00:00Z', '2026-07-12T12:00:00Z')",
            []
        ).unwrap();

        let query_str = "SELECT volume, file_id, parent_file_id, name, is_directory, size FROM facts WHERE volume = 'C:'".to_string();
        let mut stmt = conn.prepare(&query_str).unwrap();
        let rows = stmt.query_map([], |row| {
            Ok(FactNode {
                volume: row.get(0)?,
                file_id: row.get(1)?,
                parent_file_id: row.get(2)?,
                name: row.get(3)?,
                is_directory: row.get::<_, i32>(4)? != 0,
                size: row.get(5)?,
            })
        }).unwrap();

        let mut nodes = HashMap::new();
        let mut parent_map = HashMap::new();
        let mut name_map = HashMap::new();
        for r in rows {
            let n = r.unwrap();
            let key = (n.volume.clone(), n.file_id);
            parent_map.insert(key.clone(), n.parent_file_id);
            name_map.insert(key.clone(), n.name.clone());
            nodes.insert(key, n);
        }

        // Test file ranking:
        let mut file_items = Vec::new();
        for ((volume, file_id), node) in &nodes {
            if !node.is_directory {
                let rel_path = get_fact_path_local(&parent_map, &name_map, volume, *file_id);
                file_items.push(TopItem {
                    name: node.name.clone(),
                    volume: volume.to_string(),
                    path: rel_path,
                    is_directory: false,
                    size: node.size,
                    size_delta: 0,
                    churn: 0,
                    item_count: 0,
                    file_id: *file_id,
                });
            }
        }
        file_items.sort_by(|a, b| b.size.cmp(&a.size));

        assert_eq!(file_items.len(), 3);
        assert_eq!(file_items[0].name, "file2.txt");
        assert_eq!(file_items[0].size, 200);
        assert_eq!(file_items[0].path, "dir1");
        assert_eq!(file_items[1].name, "file1.txt");
        assert_eq!(file_items[1].size, 100);
        assert_eq!(file_items[2].name, "file3.txt");
        assert_eq!(file_items[2].size, 50);

        // Test folders rollup:
        let mut folder_sizes = HashMap::new();
        let mut folder_item_counts = HashMap::new();
        for ((volume, file_id), node) in &nodes {
            if !node.is_directory {
                let size = node.size;
                let mut current_parent = node.parent_file_id;
                let mut visited = HashSet::new();
                visited.insert(*file_id);

                while current_parent != 0 && current_parent != *file_id && visited.insert(current_parent) {
                    let parent_key = (volume.to_string(), current_parent);
                    if parent_map.contains_key(&parent_key) {
                        *folder_sizes.entry(parent_key.clone()).or_insert(0) += size;
                        *folder_item_counts.entry(parent_key.clone()).or_insert(0) += 1;
                        current_parent = *parent_map.get(&parent_key).unwrap();
                    } else {
                        break;
                    }
                }
            }
        }

        let mut folder_items = Vec::new();
        for ((volume, file_id), node) in &nodes {
            if node.is_directory {
                let size = *folder_sizes.get(&(volume.to_string(), *file_id)).unwrap_or(&0);
                let item_count = *folder_item_counts.get(&(volume.to_string(), *file_id)).unwrap_or(&0);
                let rel_path = get_fact_path_local(&parent_map, &name_map, volume, *file_id);
                folder_items.push(TopItem {
                    name: node.name.clone(),
                    volume: volume.to_string(),
                    path: rel_path,
                    is_directory: true,
                    size,
                    size_delta: 0,
                    churn: 0,
                    item_count,
                    file_id: *file_id,
                });
            }
        }
        folder_items.sort_by(|a, b| b.size.cmp(&a.size));

        // Ranked folders:
        // C: root (parent of all, total size 350)
        // C:/dir1 (contains file1 & file2, total size 300)
        assert_eq!(folder_items.len(), 2);
        assert_eq!(folder_items[0].name, "C:");
        assert_eq!(folder_items[0].size, 350);
        assert_eq!(folder_items[0].item_count, 3);
        assert_eq!(folder_items[1].name, "dir1");
        assert_eq!(folder_items[1].size, 300);
        assert_eq!(folder_items[1].item_count, 2);
    }

    #[test]
    fn test_is_descendant_u64() {
        let mut parent_map = HashMap::new();
        parent_map.insert(2, 1); // dir1 parent is root (1)
        parent_map.insert(3, 2); // file1 parent is dir1 (2)
        parent_map.insert(4, 2); // file2 parent is dir1 (2)
        parent_map.insert(5, 1); // file3 parent is root (1)

        assert!(is_descendant_u64(&parent_map, 3, 2)); // file1 is descendant of dir1
        assert!(is_descendant_u64(&parent_map, 3, 1)); // file1 is descendant of root
        assert!(!is_descendant_u64(&parent_map, 5, 2)); // file3 is NOT descendant of dir1
        assert!(is_descendant_u64(&parent_map, 5, 1)); // file3 is descendant of root
    }
}
