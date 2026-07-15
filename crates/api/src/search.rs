use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tantivy::query::{
    AllQuery, BooleanQuery, FuzzyTermQuery, Occur, Query, RangeQuery, RegexQuery, TermQuery,
};
use tantivy::schema::IndexRecordOption;
use tantivy::schema::*;
use tantivy::tokenizer::NgramTokenizer;
use tantivy::{Index, IndexReader, IndexWriter, TantivyDocument, Term};

pub static REBUILD_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
pub static REBUILD_PROGRESS_COUNT: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchIndexMeta {
    pub schema_version: u32,
    pub last_synced_sequence: i64,
}

pub struct SearchIndex {
    pub index: Index,
    pub reader: IndexReader,
    pub writer: Arc<Mutex<IndexWriter>>,
    pub name: Field,
    pub name_lower: Field, // untokenized lowercase filename for exact substring search
    pub name_ngram: Field, // trigram index for substring search
    pub path: Field,
    pub ext: Field,
    pub volume: Field,
    pub size: Field,
    pub modified_at: Field,
    pub is_directory: Field,
    pub attributes: Field,
    pub file_id: Field,
    pub uid: Field,
}

static SEARCH_INDEX: OnceLock<SearchIndex> = OnceLock::new();

pub fn init_search_index() -> Result<&'static SearchIndex, String> {
    if let Some(si) = SEARCH_INDEX.get() {
        return Ok(si);
    }

    let mut index_path = storage::get_db_dir().map_err(|e| e.to_string())?;
    index_path.push("search_index");
    fs::create_dir_all(&index_path).map_err(|e| e.to_string())?;

    // Build schema — name_ngram uses a custom "ngram3" tokenizer for substring search.
    let mut schema_builder = Schema::builder();
    let name = schema_builder.add_text_field("name", TEXT | STORED);
    let name_lower = schema_builder.add_text_field("name_lower", STRING | STORED);
    let name_ngram = schema_builder.add_text_field(
        "name_ngram",
        TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("ngram3")
                .set_index_option(IndexRecordOption::Basic),
        ),
    );
    let path = schema_builder.add_text_field("path", STRING | STORED);
    let ext = schema_builder.add_text_field("ext", STRING | STORED);
    let volume = schema_builder.add_text_field("volume", STRING | STORED);
    let size = schema_builder.add_u64_field("size", INDEXED | STORED);
    let modified_at = schema_builder.add_i64_field("modified_at", INDEXED | STORED);
    let is_directory = schema_builder.add_u64_field("is_directory", INDEXED | STORED);
    let attributes = schema_builder.add_text_field("attributes", STRING | STORED);
    let file_id = schema_builder.add_u64_field("file_id", INDEXED | STORED);
    let uid = schema_builder.add_text_field("uid", STRING);
    let schema = schema_builder.build();

    let index = match Index::open_or_create(
        tantivy::directory::MmapDirectory::open(&index_path).map_err(|e| e.to_string())?,
        schema.clone(),
    ) {
        Ok(idx) => idx,
        Err(e) => {
            println!(
                "[Search] Index directory opening failed: {}. Clearing and recreating...",
                e
            );
            let _ = fs::remove_dir_all(&index_path);
            let _ = fs::create_dir_all(&index_path);
            Index::open_or_create(
                tantivy::directory::MmapDirectory::open(&index_path).map_err(|e| e.to_string())?,
                schema,
            )
            .map_err(|e| e.to_string())?
        }
    };

    // Register the trigram tokenizer (min=3, max=10) for substring/infix search.
    // This breaks "afadsffsdfsdfsdf.txt" into overlapping 3-10 char windows so
    // a query like "dfsdf" hits documents containing that substring.
    index.tokenizers().register(
        "ngram3",
        NgramTokenizer::new(3, 10, false).map_err(|e| e.to_string())?,
    );

    let reader = index.reader().map_err(|e| e.to_string())?;
    let writer = index.writer(15_000_000).map_err(|e| e.to_string())?;

    let search_index = SearchIndex {
        index,
        reader,
        writer: Arc::new(Mutex::new(writer)),
        name,
        name_lower,
        name_ngram,
        path,
        ext,
        volume,
        size,
        modified_at,
        is_directory,
        attributes,
        file_id,
        uid,
    };

    let _ = SEARCH_INDEX.set(search_index);
    Ok(SEARCH_INDEX.get().unwrap())
}

pub fn check_and_trigger_rebuild(volumes: Vec<String>) {
    let si = match init_search_index() {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("[Search] Failed to initialize search index: {}", e);
            return;
        }
    };

    let mut meta_path = match storage::get_db_dir() {
        Ok(d) => d,
        Err(_) => return,
    };
    meta_path.push("search_index");
    meta_path.push("index_meta.json");

    println!("[Search] Waiting for all drain engines to reach Live state before index rebuild...");
    tokio::spawn(async move {
        loop {
            let all_live = !volumes.is_empty()
                && volumes.iter().all(|vol| {
                    let tracker = core_types::get_volume_tracker(vol);
                    let state = *tracker.state.lock().unwrap();
                    state == core_types::DaemonState::Live
                });
            if all_live {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        println!("[Search] All volumes Live. Starting full index rebuild...");
        if let Err(e) = rebuild_index_task(si, meta_path).await {
            eprintln!("[Search] Full index rebuild task failed: {:?}", e);
        }
    });
}

struct PathResolver {
    nodes: HashMap<(String, u64), (u64, String)>,
}

impl PathResolver {
    fn new(conn: &Connection) -> Result<Self, rusqlite::Error> {
        let mut stmt = conn.prepare("SELECT volume, file_id, parent_file_id, name FROM facts")?;
        let rows = stmt.query_map([], |row| {
            let volume: String = row.get(0)?;
            let file_id: u64 = row.get(1)?;
            let parent_file_id: u64 = row.get(2)?;
            let name: String = row.get(3)?;
            Ok(((volume, file_id), (parent_file_id, name)))
        })?;

        let mut nodes = HashMap::new();
        for r in rows {
            let (k, v) = r?;
            nodes.insert(k, v);
        }
        Ok(Self { nodes })
    }

    fn resolve(&self, volume: &str, file_id: u64) -> String {
        let mut current_id = file_id;
        let mut parts = Vec::new();
        let mut visited = HashSet::new();

        loop {
            if !visited.insert(current_id) {
                break;
            }
            if let Some(&(parent_id, ref name)) = self.nodes.get(&(volume.to_string(), current_id))
            {
                if name.is_empty() || name == volume {
                    break;
                }
                parts.push(name.clone());
                if parent_id == current_id || parent_id == 0 {
                    break;
                }
                current_id = parent_id;
            } else {
                break;
            }
        }

        parts.reverse();
        parts.join("/")
    }
}

pub fn get_fact_path(
    conn: &Connection,
    volume: &str,
    file_id: u64,
) -> Result<String, rusqlite::Error> {
    let mut current_id = file_id;
    let mut parts = Vec::new();
    let mut visited = HashSet::new();

    loop {
        if !visited.insert((volume.to_string(), current_id)) {
            break;
        }

        let res: Result<(u64, String), rusqlite::Error> = conn.query_row(
            "SELECT parent_file_id, name FROM facts WHERE volume = ?1 AND file_id = ?2",
            rusqlite::params![volume, current_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );

        match res {
            Ok((parent_id, name)) => {
                if name.is_empty() || name == volume {
                    break;
                }
                parts.push(name);
                if parent_id == current_id || parent_id == 0 {
                    break;
                }
                current_id = parent_id;
            }
            Err(_) => {
                break;
            }
        }
    }

    parts.reverse();
    Ok(parts.join("/"))
}

async fn rebuild_index_task(si: &'static SearchIndex, meta_path: PathBuf) -> Result<(), String> {
    REBUILD_IN_PROGRESS.store(true, Ordering::SeqCst);
    REBUILD_PROGRESS_COUNT.store(0, Ordering::SeqCst);

    let conn = storage::get_db_connection().map_err(|e| e.to_string())?;

    println!("[Search] Resolving folder paths from database...");
    let resolver = PathResolver::new(&conn).map_err(|e| e.to_string())?;

    // Collect all rows first so we can release the DB cursor before locking the writer.
    // Holding a SQLite cursor open while locking the writer (std::Mutex) for a long time
    // starves the Tokio thread pool and causes pipe connections to time out.
    #[allow(clippy::type_complexity)]
    let all_rows: Vec<(String, u64, u64, String, bool, u64, String, String, u32)> = {
        let mut stmt = conn.prepare(
            "SELECT volume, file_id, parent_file_id, name, is_directory, size, created_at, modified_at, attributes FROM facts"
        ).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i32>(4)? != 0,
                    row.get::<_, u64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, u32>(8)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.flatten().collect()
    };

    // Delete all old documents in one quick lock acquisition.
    {
        let writer_guard = si.writer.lock().unwrap();
        writer_guard
            .delete_all_documents()
            .map_err(|e| e.to_string())?;
    }

    // Index in chunks of 5 000 documents.
    // Each chunk runs in spawn_blocking so:
    //   a) the IndexWriter MutexGuard (not Send) never crosses an .await point,
    //   b) the blocking work runs on a dedicated OS thread, keeping the async
    //      executor free for pipe connections and drain batches between chunks.
    println!(
        "[Search] Populating Tantivy index ({} facts)...",
        all_rows.len()
    );
    let mut count = 0u64;
    for chunk in all_rows.chunks(5_000) {
        // Pre-build the documents on this (async) thread — PathResolver is
        // also not Send so we must resolve paths here before entering spawn_blocking.
        let docs: Vec<TantivyDocument> = chunk
            .iter()
            .map(
                |(
                    volume,
                    file_id,
                    parent_file_id,
                    name,
                    is_dir,
                    size_val,
                    _created_str,
                    modified_str,
                    attrs,
                )| {
                    let path_str = resolver.resolve(volume, *parent_file_id);
                    let modified_ts = chrono::DateTime::parse_from_rfc3339(modified_str)
                        .map(|dt| dt.with_timezone(&chrono::Utc).timestamp())
                        .unwrap_or(0);
                    let ext_str = std::path::Path::new(name.as_str())
                        .extension()
                        .map(|e| e.to_string_lossy().to_lowercase())
                        .unwrap_or_default();

                    let mut doc = TantivyDocument::new();
                    doc.add_text(si.name, name);
                    doc.add_text(si.name_lower, name.to_lowercase());
                    doc.add_text(si.name_ngram, name.to_lowercase());
                    doc.add_text(si.path, &path_str);
                    doc.add_text(si.ext, &ext_str);
                    doc.add_text(si.volume, volume);
                    doc.add_u64(si.size, *size_val);
                    doc.add_i64(si.modified_at, modified_ts);
                    doc.add_u64(si.is_directory, if *is_dir { 1 } else { 0 });
                    doc.add_u64(si.file_id, *file_id);
                    doc.add_text(si.uid, format!("{}-{}", volume, file_id));
                    if (attrs & 1) != 0 {
                        doc.add_text(si.attributes, "readonly");
                    }
                    if (attrs & 2) != 0 {
                        doc.add_text(si.attributes, "hidden");
                    }
                    if (attrs & 4) != 0 {
                        doc.add_text(si.attributes, "system");
                    }
                    if (attrs & 32) != 0 {
                        doc.add_text(si.attributes, "archive");
                    }
                    if (attrs & 1024) != 0 {
                        doc.add_text(si.attributes, "reparse");
                    }
                    doc
                },
            )
            .collect();

        let chunk_count = docs.len() as u64;
        // Write + commit the chunk on a blocking thread.
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let mut writer_guard = si.writer.lock().unwrap();
            for doc in docs {
                writer_guard.add_document(doc).map_err(|e| e.to_string())?;
            }
            writer_guard.commit().map_err(|e| e.to_string())?;
            drop(writer_guard);
            si.reader.reload().map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())??;

        count += chunk_count;
        REBUILD_PROGRESS_COUNT.store(count, Ordering::SeqCst);
        // Brief yield lets the async executor handle any queued pipe connections.
        tokio::task::yield_now().await;
    }

    let last_seq: i64 = conn
        .query_row(
            "SELECT IFNULL(MAX(sequence), 0) FROM mutation_log",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let meta = SearchIndexMeta {
        schema_version: 3,
        last_synced_sequence: last_seq,
    };
    let meta_str = serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?;
    let _ = fs::write(&meta_path, meta_str);

    REBUILD_PROGRESS_COUNT.store(count, Ordering::SeqCst);
    REBUILD_IN_PROGRESS.store(false, Ordering::SeqCst);

    println!(
        "[Search] Search index rebuild complete! Indexed {} files.",
        count
    );
    Ok(())
}

pub fn update_fact_in_index(conn: &Connection, volume: &str, file_id: u64) -> Result<(), String> {
    let si = init_search_index()?;

    // Use try_lock with a brief spin-wait instead of blocking lock().
    // Blocking a std::Mutex here would starve the Tokio worker thread if the
    // rebuild task holds the writer for a long time.
    let writer_guard = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
        loop {
            match si.writer.try_lock() {
                Ok(g) => break g,
                Err(_) => {
                    if std::time::Instant::now() >= deadline {
                        // Writer is held by rebuild; skip this update — the rebuild
                        // will index the current state of facts when it processes this file.
                        return Ok(());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }
    };

    let uid_term = Term::from_field_text(si.uid, &format!("{}-{}", volume, file_id));
    writer_guard.delete_term(uid_term);

    #[allow(clippy::type_complexity)]
    let res: Result<(String, u64, u64, String, i32, u64, String, String, u32), rusqlite::Error> = conn.query_row(
        "SELECT volume, file_id, parent_file_id, name, is_directory, size, created_at, modified_at, attributes FROM facts WHERE volume = ?1 AND file_id = ?2",
        rusqlite::params![volume, file_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?)),
    );

    if let Ok((
        volume,
        file_id,
        parent_file_id,
        name,
        is_dir_int,
        size_val,
        created_str,
        modified_str,
        attrs,
    )) = res
    {
        let is_dir = is_dir_int != 0;
        let path_str = get_fact_path(conn, &volume, parent_file_id).unwrap_or_default();

        let _created_dt = chrono::DateTime::parse_from_rfc3339(&created_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now());
        let modified_dt = chrono::DateTime::parse_from_rfc3339(&modified_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now());

        let mut doc = TantivyDocument::new();
        doc.add_text(si.name, &name);
        doc.add_text(si.name_lower, name.to_lowercase());
        doc.add_text(si.name_ngram, &name);
        doc.add_text(si.path, &path_str);

        let ext_str = std::path::Path::new(&name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        doc.add_text(si.ext, &ext_str);
        doc.add_text(si.volume, &volume);
        doc.add_u64(si.size, size_val);
        doc.add_i64(si.modified_at, modified_dt.timestamp());
        doc.add_u64(si.is_directory, if is_dir { 1 } else { 0 });
        doc.add_u64(si.file_id, file_id);
        doc.add_text(si.uid, format!("{}-{}", volume, file_id));

        if (attrs & 1) != 0 {
            doc.add_text(si.attributes, "readonly");
        }
        if (attrs & 2) != 0 {
            doc.add_text(si.attributes, "hidden");
        }
        if (attrs & 4) != 0 {
            doc.add_text(si.attributes, "system");
        }
        if (attrs & 32) != 0 {
            doc.add_text(si.attributes, "archive");
        }
        if (attrs & 1024) != 0 {
            doc.add_text(si.attributes, "reparse");
        }

        writer_guard.add_document(doc).map_err(|e| e.to_string())?;
    }

    Ok(())
}

pub fn delete_fact_from_index(volume: &str, file_id: u64) -> Result<(), String> {
    let si = init_search_index()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
    loop {
        match si.writer.try_lock() {
            Ok(writer_guard) => {
                let uid_term = Term::from_field_text(si.uid, &format!("{}-{}", volume, file_id));
                writer_guard.delete_term(uid_term);
                return Ok(());
            }
            Err(_) => {
                if std::time::Instant::now() >= deadline {
                    return Ok(()); // Skip; rebuild will not include this deleted file
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
}

pub fn commit_search_index() -> Result<(), String> {
    let si = init_search_index()?;
    // Use try_lock with spin-wait to avoid blocking Tokio threads.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    loop {
        match si.writer.try_lock() {
            Ok(mut writer_guard) => {
                writer_guard.commit().map_err(|e| e.to_string())?;
                si.reader.reload().map_err(|e| e.to_string())?;
                return Ok(());
            }
            Err(_) => {
                if std::time::Instant::now() >= deadline {
                    return Ok(()); // Skip commit; rebuild will commit when it finishes its chunk
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
}

pub fn reconcile_search_results(
    docs: Vec<(TantivyDocument, f32)>,
    conn: &Connection,
) -> Vec<(TantivyDocument, f32)> {
    let si = match init_search_index() {
        Ok(s) => s,
        Err(_) => return docs,
    };

    if docs.is_empty() {
        return docs;
    }

    let mut doc_keys = Vec::with_capacity(docs.len());
    for (doc, _) in &docs {
        let doc_volume = doc
            .get_first(si.volume)
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let doc_file_id = doc
            .get_first(si.file_id)
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if !doc_volume.is_empty() && doc_file_id != 0 {
            doc_keys.push((doc_volume.to_string(), doc_file_id));
        }
    }

    let mut existing_keys = HashSet::new();
    for chunk in doc_keys.chunks(100) {
        let mut query = String::from("SELECT volume, file_id FROM facts WHERE ");
        let mut params = Vec::new();
        for (i, (vol, fid)) in chunk.iter().enumerate() {
            if i > 0 {
                query.push_str(" OR ");
            }
            query.push_str(&format!(
                "(volume = ?{} AND file_id = ?{})",
                i * 2 + 1,
                i * 2 + 2
            ));
            params.push(vol as &dyn rusqlite::ToSql);
            params.push(fid as &dyn rusqlite::ToSql);
        }

        if let Ok(mut stmt) = conn.prepare(&query) {
            if let Ok(mut rows) = stmt.query(rusqlite::params_from_iter(params)) {
                while let Ok(Some(row)) = rows.next() {
                    if let (Ok(vol), Ok(fid)) = (row.get::<_, String>(0), row.get::<_, u64>(1)) {
                        existing_keys.insert((vol, fid));
                    }
                }
            }
        }
    }

    let mut alive = Vec::with_capacity(docs.len());
    let mut stale_uids: Vec<(String, u64)> = Vec::new();

    for (doc, score) in docs {
        let doc_volume = match doc.get_first(si.volume).and_then(|v| v.as_str()) {
            Some(v) => v.to_string(),
            None => {
                alive.push((doc, score));
                continue;
            }
        };
        let doc_file_id = match doc.get_first(si.file_id).and_then(|v| v.as_u64()) {
            Some(id) => id,
            None => {
                alive.push((doc, score));
                continue;
            }
        };

        if existing_keys.contains(&(doc_volume.clone(), doc_file_id)) {
            alive.push((doc, score));
        } else {
            stale_uids.push((doc_volume, doc_file_id));
        }
    }

    if !stale_uids.is_empty() {
        if let Ok(si) = init_search_index() {
            if let Ok(writer_guard) = si.writer.try_lock() {
                for (vol, fid) in stale_uids {
                    let uid_term = Term::from_field_text(si.uid, &format!("{}-{}", vol, fid));
                    writer_guard.delete_term(uid_term);
                }
            }
        }
    }

    alive
}

#[allow(clippy::too_many_arguments)]
pub fn execute_search(
    query_str: &str,
    path_filter: Option<&str>,
    ext_filter: Option<&str>,
    volume_filter: Option<&str>,
    min_size: Option<u64>,
    max_size: Option<u64>,
    modified_after: Option<i64>,
    modified_before: Option<i64>,
    hidden_filter: Option<bool>,
    system_filter: Option<bool>,
    limit: usize,
    advanced: bool,
) -> Result<Vec<(TantivyDocument, f32)>, String> {
    let si = init_search_index()?;
    let searcher = si.reader.searcher();

    let mut sub_queries: Vec<(Occur, Box<dyn Query>)> = Vec::new();

    // Escape regex special chars helper for regex queries.
    fn escape_re(s: &str) -> String {
        let mut out = String::with_capacity(s.len() * 2);
        for c in s.chars() {
            if r"\.+*?()|[]{}^$".contains(c) {
                out.push('\\');
            }
            out.push(c);
        }
        out
    }

    // --- Fuzzy/Advanced or Exact Substring Text Search ---
    if !query_str.trim().is_empty() && query_str != "*" {
        let q_lower = query_str.trim().to_lowercase();

        if advanced {
            // --- Fuzzy Text Search with 5-tier ranking ---
            // Tier 1: Exact full name match       → boost 10.0  ("notes.txt" → "notes.txt")
            // Tier 2: Exact phrase (all words)    → boost  6.0  ("magic file" → "magic_files.txt")
            // Tier 3: All words present (AND)     → boost  3.0  (both "magic" and "file" in name)
            // Tier 4: Prefix per word             → boost  1.5  ("mag" → "magic*")
            // Tier 5: Fuzzy per word (edit dist.) → boost  0.8  ("magik" → fuzzy "magic")
            //
            // The final Must clause requires AT LEAST one tier to match, so any matching
            // document reaches the results list. The score from the highest-matching tier
            // determines its rank position.
            let words: Vec<&str> = q_lower.split_whitespace().collect();
            let query_parser = tantivy::query::QueryParser::for_index(&si.index, vec![si.name]);

            // Collect all tier queries into one big Should pool.
            // The Must wrapper below guarantees at least one tier matches.
            let mut tiers: Vec<(Occur, Box<dyn Query>)> = Vec::new();

            if words.is_empty() {
                sub_queries.push((Occur::Must, Box::new(AllQuery)));
            } else {
                // Tier 1 — exact full name string (works on the tokenised field via QueryParser)
                if let Ok(exact_full) = query_parser.parse_query(&format!("\"{}\"", q_lower)) {
                    let boosted = Box::new(tantivy::query::BoostQuery::new(exact_full, 10.0));
                    tiers.push((Occur::Should, boosted));
                }

                // Tier 2 — exact phrase (multi-word PhraseQuery via QueryParser "quoted")
                if words.len() > 1 {
                    if let Ok(phrase_q) = query_parser.parse_query(&format!("\"{}\"", q_lower)) {
                        let boosted = Box::new(tantivy::query::BoostQuery::new(phrase_q, 6.0));
                        tiers.push((Occur::Should, boosted));
                    }
                }

                // Tier 3 — all words must appear (AND of exact TermQuery per word)
                if words.len() > 1 {
                    let and_clauses: Vec<(Occur, Box<dyn Query>)> = words
                        .iter()
                        .map(|w| {
                            let term = Term::from_field_text(si.name, w);
                            let q: Box<dyn Query> =
                                Box::new(TermQuery::new(term, IndexRecordOption::Basic));
                            (Occur::Must, q)
                        })
                        .collect();
                    let and_query = Box::new(BooleanQuery::new(and_clauses));
                    let boosted = Box::new(tantivy::query::BoostQuery::new(and_query, 3.0));
                    tiers.push((Occur::Should, boosted));
                }

                // Tier 3b — single word: exact term match also at 3.0
                if words.len() == 1 {
                    let term = Term::from_field_text(si.name, words[0]);
                    let exact_q = Box::new(TermQuery::new(term, IndexRecordOption::Basic));
                    let boosted = Box::new(tantivy::query::BoostQuery::new(exact_q, 3.0));
                    tiers.push((Occur::Should, boosted));
                }

                // Tier 4 — prefix per word using RegexQuery("word.*") directly.
                {
                    let prefix_clauses: Vec<(Occur, Box<dyn Query>)> = words
                        .iter()
                        .filter_map(|w| {
                            let pattern = format!("{}.*", escape_re(w));
                            RegexQuery::from_pattern(&pattern, si.name).ok().map(|rq| {
                                let boosted: Box<dyn Query> =
                                    Box::new(tantivy::query::BoostQuery::new(Box::new(rq), 1.5));
                                (Occur::Must, boosted)
                            })
                        })
                        .collect();
                    if prefix_clauses.len() == words.len() {
                        let prefix_and = Box::new(BooleanQuery::new(prefix_clauses));
                        tiers.push((Occur::Should, prefix_and as Box<dyn Query>));
                    }
                }

                // Tier 4.5 — n-gram substring search (infix / LIKE '%%' match).
                {
                    let q_ngram = q_lower.replace(' ', "");
                    if q_ngram.len() >= 3 {
                        let chars: Vec<char> = q_ngram.chars().collect();
                        let mut ngram_should: Vec<(Occur, Box<dyn Query>)> = Vec::new();

                        for gram_size in 3usize..=10usize {
                            if chars.len() < gram_size {
                                break;
                            }
                            for i in 0..=(chars.len() - gram_size) {
                                let gram: String = chars[i..i + gram_size].iter().collect();
                                let term = Term::from_field_text(si.name_ngram, &gram);
                                let tq: Box<dyn Query> =
                                    Box::new(TermQuery::new(term, IndexRecordOption::Basic));
                                ngram_should.push((Occur::Should, tq));
                            }
                        }

                        if !ngram_should.is_empty() {
                            let ngram_q = Box::new(BooleanQuery::new(ngram_should));
                            let boosted = Box::new(tantivy::query::BoostQuery::new(ngram_q, 1.2));
                            tiers.push((Occur::Should, boosted as Box<dyn Query>));
                        }
                    }
                }

                // Tier 4.7 — Infix substring match (LIKE '%word%') using RegexQuery directly.
                {
                    let infix_clauses: Vec<(Occur, Box<dyn Query>)> = words
                        .iter()
                        .filter_map(|w| {
                            if w.len() >= 2 {
                                let pattern = format!(".*{}.*", escape_re(w));
                                RegexQuery::from_pattern(&pattern, si.name).ok().map(|rq| {
                                    let boosted: Box<dyn Query> = Box::new(
                                        tantivy::query::BoostQuery::new(Box::new(rq), 1.1),
                                    );
                                    (Occur::Must, boosted)
                                })
                            } else {
                                None
                            }
                        })
                        .collect();
                    if infix_clauses.len() == words.len() {
                        let infix_and = Box::new(BooleanQuery::new(infix_clauses));
                        tiers.push((Occur::Should, infix_and as Box<dyn Query>));
                    }
                }

                // Tier 5 — fuzzy per word (edit distance scales with word length)
                {
                    let fuzzy_clauses: Vec<(Occur, Box<dyn Query>)> = words
                        .iter()
                        .filter(|w| w.len() > 2)
                        .map(|w| {
                            let term = Term::from_field_text(si.name, w);
                            let max_dist = if w.len() > 5 { 2u8 } else { 1u8 };
                            let fq = FuzzyTermQuery::new(term, max_dist, true);
                            let boosted: Box<dyn Query> =
                                Box::new(tantivy::query::BoostQuery::new(Box::new(fq), 0.8));
                            (Occur::Must, boosted)
                        })
                        .collect();
                    if !fuzzy_clauses.is_empty() {
                        let fuzzy_and = Box::new(BooleanQuery::new(fuzzy_clauses));
                        tiers.push((Occur::Should, fuzzy_and as Box<dyn Query>));
                    }
                }

                // The outer Must ensures at least one tier matches (any hit qualifies).
                let text_query = Box::new(BooleanQuery::new(tiers));
                sub_queries.push((Occur::Must, text_query));
            }
        } else {
            // --- Default exact case-insensitive substring search ---
            let pattern = format!(".*{}.*", escape_re(&q_lower));
            let exact_q = RegexQuery::from_pattern(&pattern, si.name_lower)
                .map_err(|e| format!("Failed to compile regex query: {}", e))?;
            sub_queries.push((Occur::Must, Box::new(exact_q)));
        }
    } else {
        sub_queries.push((Occur::Must, Box::new(AllQuery)));
    }

    // 2. Path filter (Prefix Query using QueryParser)
    if let Some(p) = path_filter {
        let p_norm = p.replace('\\', "/").trim_start_matches('/').to_string();
        let query_parser = tantivy::query::QueryParser::for_index(&si.index, vec![si.path]);
        if let Ok(path_query) = query_parser.parse_query(&format!("{}*", p_norm)) {
            sub_queries.push((Occur::Must, path_query));
        }
    }

    // 3. Ext filter
    if let Some(e) = ext_filter {
        let e_norm = e.trim_start_matches('.').to_lowercase();
        let term = Term::from_field_text(si.ext, &e_norm);
        sub_queries.push((
            Occur::Must,
            Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
        ));
    }

    // 4. Volume filter
    if let Some(v) = volume_filter {
        let term = Term::from_field_text(si.volume, v);
        sub_queries.push((
            Occur::Must,
            Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
        ));
    }

    // 5. Size range filter
    if min_size.is_some() || max_size.is_some() {
        let min_val = min_size.unwrap_or(0);
        let max_val = max_size.unwrap_or(u64::MAX);
        let field_name = si.index.schema().get_field_name(si.size).to_string();
        let size_query = Box::new(RangeQuery::new_u64(
            field_name,
            min_val..max_val.saturating_add(1),
        ));
        sub_queries.push((Occur::Must, size_query));
    }

    // 6. Modified range filter
    if modified_after.is_some() || modified_before.is_some() {
        let min_time = modified_after.unwrap_or(i64::MIN);
        let max_time = modified_before.unwrap_or(i64::MAX);
        let field_name = si.index.schema().get_field_name(si.modified_at).to_string();
        let time_query = Box::new(RangeQuery::new_i64(
            field_name,
            min_time..max_time.saturating_add(1),
        ));
        sub_queries.push((Occur::Must, time_query));
    }

    // 7. Hidden filter
    if let Some(h) = hidden_filter {
        let term = Term::from_field_text(si.attributes, "hidden");
        let query = Box::new(TermQuery::new(term, IndexRecordOption::Basic));
        if h {
            sub_queries.push((Occur::Must, query));
        } else {
            sub_queries.push((Occur::MustNot, query));
        }
    }

    // 8. System filter
    if let Some(s) = system_filter {
        let term = Term::from_field_text(si.attributes, "system");
        let query = Box::new(TermQuery::new(term, IndexRecordOption::Basic));
        if s {
            sub_queries.push((Occur::Must, query));
        } else {
            sub_queries.push((Occur::MustNot, query));
        }
    }

    let final_query = BooleanQuery::new(sub_queries);
    let top_docs = searcher
        .search(
            &final_query,
            &tantivy::collector::TopDocs::with_limit(limit),
        )
        .map_err(|e| e.to_string())?;

    let mut docs = Vec::new();
    for (score, doc_address) in top_docs {
        if let Ok(doc) = searcher.doc(doc_address) {
            docs.push((doc, score));
        }
    }
    Ok(docs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tantivy::TantivyDocument;

    #[test]
    fn test_exact_substring_search() {
        let si = init_search_index().unwrap();
        let mut writer_guard = si.writer.lock().unwrap();
        writer_guard.delete_all_documents().unwrap();

        let names = vec!["MyImportantFile.txt", "readme.md", "DRAFT_PROPOSAL.docx"];
        for name in names {
            let mut doc = TantivyDocument::new();
            doc.add_text(si.name, name);
            doc.add_text(si.name_lower, name.to_lowercase());
            doc.add_text(si.name_ngram, name.to_lowercase());
            doc.add_text(si.path, "C:/test");
            doc.add_text(si.ext, "txt");
            doc.add_text(si.volume, "C:");
            doc.add_u64(si.size, 100);
            doc.add_i64(si.modified_at, 12345678);
            doc.add_u64(si.is_directory, 0);
            doc.add_text(si.uid, format!("C:-{}", name));
            writer_guard.add_document(doc).unwrap();
        }
        writer_guard.commit().unwrap();
        si.reader.reload().unwrap();
        drop(writer_guard);

        // 1. Lowercase search query matching mixed case filename
        let hits = execute_search(
            "important",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            10,
            false,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        let matched_name = hits[0].0.get_first(si.name).unwrap().as_str().unwrap();
        assert_eq!(matched_name, "MyImportantFile.txt");

        // 2. Uppercase search query matching lowercase filename
        let hits = execute_search(
            "README", None, None, None, None, None, None, None, None, None, 10, false,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        let matched_name = hits[0].0.get_first(si.name).unwrap().as_str().unwrap();
        assert_eq!(matched_name, "readme.md");

        // 3. Mixed case query matching uppercase filename
        let hits = execute_search(
            "draft_proposal",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            10,
            false,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        let matched_name = hits[0].0.get_first(si.name).unwrap().as_str().unwrap();
        assert_eq!(matched_name, "DRAFT_PROPOSAL.docx");

        // 4. Query that shouldn't match (not substring)
        let hits = execute_search(
            "imptnt", None, None, None, None, None, None, None, None, None, 10, false,
        )
        .unwrap();
        assert_eq!(hits.len(), 0);
    }
}
