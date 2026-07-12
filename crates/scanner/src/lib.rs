#[cfg(not(windows))]
use std::fs;
use std::io;
#[cfg(not(windows))]
use std::path::Path;
#[cfg(not(windows))]
use chrono::Utc;
use rusqlite::Connection;

#[cfg(not(windows))]
fn get_file_id(_path: &Path, metadata: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.ino()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn flush_batch(conn: &mut Connection, batch: &mut Vec<core_types::Fact>) -> Result<(), rusqlite::Error> {
    if batch.is_empty() {
        return Ok(());
    }
    let res = (|| {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute("PRAGMA defer_foreign_keys = ON", [])?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO facts (volume, file_id, parent_file_id, name, is_directory, size, created_at, modified_at, attributes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(volume, file_id) DO UPDATE SET
                    parent_file_id = excluded.parent_file_id,
                    name = excluded.name,
                    is_directory = excluded.is_directory,
                    size = excluded.size,
                    modified_at = excluded.modified_at,
                    attributes = excluded.attributes"
            )?;
            let mut visited_stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO temp.visited_files (file_id) VALUES (?1)"
            )?;
            for fact in batch.iter() {
                let created_str = fact.created_at.to_rfc3339();
                let modified_str = fact.modified_at.to_rfc3339();
                stmt.execute(rusqlite::params![
                    fact.volume,
                    fact.file_id,
                    fact.parent_file_id,
                    fact.name,
                    if fact.is_directory { 1 } else { 0 },
                    fact.size,
                    created_str,
                    modified_str,
                    fact.attributes,
                ])?;
                visited_stmt.execute(rusqlite::params![fact.file_id])?;
            }
        }
        tx.commit()?;
        Ok(())
    })();

    batch.clear();
    res
}

/// Performs a recursive crawler walk of the filesystem root for the given volume.
/// - Windows: Crawls standard drive root (e.g. `C:\` or `D:\`) using fast batch enumeration.
/// - Unix/Linux/WSL: Automatically creates and crawls mock trees under `/tmp/disktracker_mock_<volume>`.
pub fn scan_volume(volume: &str) -> io::Result<()> {
    let root_path = if cfg!(windows) {
        format!("{}\\", volume)
    } else {
        format!("/tmp/disktracker_mock_{}", volume)
    };

    #[cfg(not(windows))]
    {
        // Seed some mock folders and files on Linux/WSL to walk through
        let mock_root = Path::new(&root_path);
        let _ = fs::create_dir_all(mock_root.join("documents"));
        let _ = fs::create_dir_all(mock_root.join("downloads"));
        let _ = fs::write(mock_root.join("documents").join("invoice.pdf"), b"pdf data");
        let _ = fs::write(mock_root.join("downloads").join("setup.exe"), b"exe data");
        let _ = fs::write(mock_root.join("todo.txt"), b"todo list");
    }

    println!(
        "[Scanner - {}] Starting volume crawl from root {:?}",
        volume, root_path
    );

    let mut conn = match storage::get_db_connection() {
        Ok(c) => c,
        Err(e) => return Err(io::Error::new(io::ErrorKind::Other, e.to_string())),
    };

    conn.execute(
        "CREATE TEMP TABLE IF NOT EXISTS visited_files (file_id INTEGER PRIMARY KEY)",
        [],
    ).map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    conn.execute("DELETE FROM temp.visited_files", [])
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    let mut dirs_scanned = 0u64;
    let mut files_scanned = 0u64;
    let mut batch = Vec::with_capacity(5000);

    let vol_owned = volume.to_string();

    let tracker = core_types::get_volume_tracker(volume);
    tracker.dirs_scanned.store(0, std::sync::atomic::Ordering::Relaxed);
    tracker.files_scanned.store(0, std::sync::atomic::Ordering::Relaxed);
    *tracker.current_path.lock().unwrap() = None;

    #[cfg(windows)]
    {
        platform_windows::walk_directory_fast(&root_path, volume, |info| {
            let fact = core_types::Fact {
                volume: vol_owned.clone(),
                file_id: info.file_id,
                parent_file_id: info.parent_file_id,
                name: info.name,
                is_directory: info.is_directory,
                size: info.size,
                created_at: info.created_at,
                modified_at: info.modified_at,
                attributes: info.attributes,
            };
            batch.push(fact);

            if info.is_directory {
                dirs_scanned += 1;
                tracker.dirs_scanned.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            } else {
                files_scanned += 1;
                tracker.files_scanned.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }

            if batch.len() >= 5000 {
                let _ = flush_batch(&mut conn, &mut batch);
            }
        })?;
    }

    #[cfg(not(windows))]
    {
        let root_metadata = fs::metadata(&root_path)?;
        let root_file_id = get_file_id(Path::new(&root_path), &root_metadata);

        // Insert root itself as a fact
        let root_created = root_metadata.created().ok().map(chrono::DateTime::from).unwrap_or_else(Utc::now);
        let root_modified = root_metadata.modified().ok().map(chrono::DateTime::from).unwrap_or_else(Utc::now);
        batch.push(core_types::Fact {
            volume: vol_owned.clone(),
            file_id: root_file_id,
            parent_file_id: root_file_id,
            name: volume.to_string(),
            is_directory: true,
            size: 0,
            created_at: root_created,
            modified_at: root_modified,
            attributes: 0,
        });
        dirs_scanned += 1;
        tracker.dirs_scanned.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        fn walk(
            dir: &Path,
            parent_file_id: u64,
            volume: &str,
            vol_owned: &str,
            dirs_scanned: &mut u64,
            files_scanned: &mut u64,
            conn: &mut Connection,
            batch: &mut Vec<core_types::Fact>,
            tracker: &std::sync::Arc<core_types::VolumeProgressTracker>,
        ) {
            *tracker.current_path.lock().unwrap() = Some(dir.to_string_lossy().into_owned());
            if let Ok(entries) = fs::read_dir(dir) {
                *dirs_scanned += 1;
                tracker.dirs_scanned.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                for entry in entries.flatten() {
                    let path = entry.path();
                    let metadata = match entry.metadata() {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    let file_id = get_file_id(&path, &metadata);
                    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    let created = metadata.created().ok().map(chrono::DateTime::from).unwrap_or_else(Utc::now);
                    let modified = metadata.modified().ok().map(chrono::DateTime::from).unwrap_or_else(Utc::now);

                    let file_type = match entry.file_type() {
                        Ok(t) => t,
                        Err(_) => continue,
                    };
                    let is_dir = file_type.is_dir();
                    let is_symlink = file_type.is_symlink();
                    let size = if is_dir { 0 } else { metadata.len() };

                    let mut attrs = 0;
                    if !is_dir {
                        attrs |= 32;
                    }
                    if name.starts_with('.') {
                        attrs |= 2;
                    }
                    if is_symlink {
                        attrs |= 1024;
                    }

                    batch.push(core_types::Fact {
                        volume: vol_owned.to_string(),
                        file_id,
                        parent_file_id,
                        name: name.clone(),
                        is_directory: is_dir,
                        size,
                        created_at: created,
                        modified_at: modified,
                        attributes: attrs,
                    });

                    if batch.len() >= 5000 {
                        if let Err(e) = flush_batch(conn, batch) {
                            eprintln!("[Scanner - {}] DB error flushing batch: {:?}", volume, e);
                        }
                    }

                    if is_dir && !is_symlink {
                        walk(&path, file_id, volume, vol_owned, dirs_scanned, files_scanned, conn, batch, tracker);
                    } else if !is_dir {
                        *files_scanned += 1;
                        tracker.files_scanned.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }

        walk(
            Path::new(&root_path),
            root_file_id,
            volume,
            &vol_owned,
            &mut dirs_scanned,
            &mut files_scanned,
            &mut conn,
            &mut batch,
            &tracker,
        );
    }

    // Flush any remaining facts
    if let Err(e) = flush_batch(&mut conn, &mut batch) {
        eprintln!("[Scanner - {}] DB error flushing final batch: {:?}", volume, e);
    }

    // Delete any files in facts for this volume that were NOT visited during this scan
    println!("[Scanner - {}] Purging stale facts...", volume);
    if let Err(e) = conn.execute(
        "DELETE FROM facts WHERE volume = ?1 AND NOT EXISTS (SELECT 1 FROM temp.visited_files WHERE temp.visited_files.file_id = facts.file_id)",
        rusqlite::params![volume],
    ) {
        eprintln!("[Scanner - {}] DB error purging stale facts: {:?}", volume, e);
    }

    // Done scanning this volume. Keep current_path clean.
    *tracker.current_path.lock().unwrap() = None;

    println!(
        "[Scanner - {}] Crawl complete. Dirs scanned: {}, Files scanned: {}",
        volume, dirs_scanned, files_scanned
    );
    Ok(())
}
