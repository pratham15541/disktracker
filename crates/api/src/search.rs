use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use rusqlite::Connection;
use tantivy::schema::*;
use tantivy::{Index, IndexWriter, TantivyDocument, Term, IndexReader};
use tantivy::query::{BooleanQuery, Occur, Query, TermQuery, RangeQuery, AllQuery, FuzzyTermQuery};
use tantivy::schema::IndexRecordOption;
use serde::{Deserialize, Serialize};

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

    let mut schema_builder = Schema::builder();
    let name = schema_builder.add_text_field("name", TEXT | STORED);
    let path = schema_builder.add_text_field("path", STRING | STORED);
    let ext = schema_builder.add_text_field("ext", STRING | STORED);
    let volume = schema_builder.add_text_field("volume", STRING | STORED);
    let size = schema_builder.add_u64_field("size", INDEXED | STORED);
    let modified_at = schema_builder.add_i64_field("modified_at", INDEXED | STORED);
    let is_directory = schema_builder.add_u64_field("is_directory", INDEXED | STORED);
    let attributes = schema_builder.add_text_field("attributes", STRING | STORED); // multivalued terms
    let file_id = schema_builder.add_u64_field("file_id", INDEXED | STORED);
    let uid = schema_builder.add_text_field("uid", STRING); // Unique identifier: "volume-file_id"
    let schema = schema_builder.build();

    let index = match Index::open_or_create(tantivy::directory::MmapDirectory::open(&index_path).map_err(|e| e.to_string())?, schema.clone()) {
        Ok(idx) => idx,
        Err(e) => {
            println!("[Search] Index directory opening failed: {}. Clearing and recreating...", e);
            let _ = fs::remove_dir_all(&index_path);
            let _ = fs::create_dir_all(&index_path);
            Index::open_or_create(tantivy::directory::MmapDirectory::open(&index_path).map_err(|e| e.to_string())?, schema)
                .map_err(|e| e.to_string())?
        }
    };

    let reader = index.reader().map_err(|e| e.to_string())?;
    let writer = index.writer(15_000_000).map_err(|e| e.to_string())?;

    let search_index = SearchIndex {
        index,
        reader,
        writer: Arc::new(Mutex::new(writer)),
        name,
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
            let all_live = !volumes.is_empty() && volumes.iter().all(|vol| {
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
            if let Some(&(parent_id, ref name)) = self.nodes.get(&(volume.to_string(), current_id)) {
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

pub fn get_fact_path(conn: &Connection, volume: &str, file_id: u64) -> Result<String, rusqlite::Error> {
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

    let mut writer_guard = si.writer.lock().unwrap();
    writer_guard.delete_all_documents().map_err(|e| e.to_string())?;

    let mut stmt = conn.prepare(
        "SELECT volume, file_id, parent_file_id, name, is_directory, size, created_at, modified_at, attributes FROM facts"
    ).map_err(|e| e.to_string())?;

    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;

    println!("[Search] Populating Tantivy index...");
    let mut count = 0;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let volume: String = row.get(0).map_err(|e| e.to_string())?;
        let file_id: u64 = row.get(1).map_err(|e| e.to_string())?;
        let parent_file_id: u64 = row.get(2).map_err(|e| e.to_string())?;
        let name: String = row.get(3).map_err(|e| e.to_string())?;
        let is_dir_int: i32 = row.get(4).map_err(|e| e.to_string())?;
        let size_val: u64 = row.get(5).map_err(|e| e.to_string())?;
        let created_str: String = row.get(6).map_err(|e| e.to_string())?;
        let modified_str: String = row.get(7).map_err(|e| e.to_string())?;
        let attrs: u32 = row.get(8).map_err(|e| e.to_string())?;

        let is_dir = is_dir_int != 0;
        let path_str = resolver.resolve(&volume, parent_file_id);

        let _created_dt = chrono::DateTime::parse_from_rfc3339(&created_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now());
        let modified_dt = chrono::DateTime::parse_from_rfc3339(&modified_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now());

        let mut doc = TantivyDocument::new();
        doc.add_text(si.name, &name);
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
        doc.add_text(si.uid, &format!("{}-{}", volume, file_id));

        // Add attribute multi-values
        if (attrs & 1) != 0 { doc.add_text(si.attributes, "readonly"); }
        if (attrs & 2) != 0 { doc.add_text(si.attributes, "hidden"); }
        if (attrs & 4) != 0 { doc.add_text(si.attributes, "system"); }
        if (attrs & 32) != 0 { doc.add_text(si.attributes, "archive"); }
        if (attrs & 1024) != 0 { doc.add_text(si.attributes, "reparse"); }

        writer_guard.add_document(doc).map_err(|e| e.to_string())?;

        count += 1;
        if count % 1000 == 0 {
            REBUILD_PROGRESS_COUNT.store(count, Ordering::SeqCst);
        }
    }

    writer_guard.commit().map_err(|e| e.to_string())?;
    si.reader.reload().map_err(|e| e.to_string())?;

    let last_seq: i64 = conn.query_row(
        "SELECT IFNULL(MAX(sequence), 0) FROM mutation_log",
        [],
        |row| row.get(0)
    ).unwrap_or(0);

    let meta = SearchIndexMeta {
        schema_version: 2,
        last_synced_sequence: last_seq,
    };
    let meta_str = serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?;
    let _ = fs::write(&meta_path, meta_str);

    REBUILD_PROGRESS_COUNT.store(count, Ordering::SeqCst);
    REBUILD_IN_PROGRESS.store(false, Ordering::SeqCst);

    println!("[Search] Search index rebuild complete! Indexed {} files.", count);
    Ok(())
}

pub fn update_fact_in_index(conn: &Connection, volume: &str, file_id: u64) -> Result<(), String> {
    let si = init_search_index()?;
    let writer_guard = si.writer.lock().unwrap();

    let uid_term = Term::from_field_text(si.uid, &format!("{}-{}", volume, file_id));
    writer_guard.delete_term(uid_term);

    let res: Result<(String, u64, u64, String, i32, u64, String, String, u32), rusqlite::Error> = conn.query_row(
        "SELECT volume, file_id, parent_file_id, name, is_directory, size, created_at, modified_at, attributes FROM facts WHERE volume = ?1 AND file_id = ?2",
        rusqlite::params![volume, file_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?)),
    );

    if let Ok((volume, file_id, parent_file_id, name, is_dir_int, size_val, created_str, modified_str, attrs)) = res {
        let is_dir = is_dir_int != 0;
        // FIX 2: Always add the document even if path resolution yields empty string
        // (parent chain may be incomplete due to batch ordering). The document will
        // still be searchable by name; reconcile_search_results will filter it against
        // facts at query time. An empty path just means path-prefix filter won't match it.
        let path_str = get_fact_path(conn, &volume, parent_file_id).unwrap_or_default();

        let _created_dt = chrono::DateTime::parse_from_rfc3339(&created_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now());
        let modified_dt = chrono::DateTime::parse_from_rfc3339(&modified_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now());

        let mut doc = TantivyDocument::new();
        doc.add_text(si.name, &name);
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
        doc.add_text(si.uid, &format!("{}-{}", volume, file_id));

        if (attrs & 1) != 0 { doc.add_text(si.attributes, "readonly"); }
        if (attrs & 2) != 0 { doc.add_text(si.attributes, "hidden"); }
        if (attrs & 4) != 0 { doc.add_text(si.attributes, "system"); }
        if (attrs & 32) != 0 { doc.add_text(si.attributes, "archive"); }
        if (attrs & 1024) != 0 { doc.add_text(si.attributes, "reparse"); }

        writer_guard.add_document(doc).map_err(|e| e.to_string())?;
    }

    Ok(())
}

pub fn delete_fact_from_index(volume: &str, file_id: u64) -> Result<(), String> {
    let si = init_search_index()?;
    let writer_guard = si.writer.lock().unwrap();

    let uid_term = Term::from_field_text(si.uid, &format!("{}-{}", volume, file_id));
    writer_guard.delete_term(uid_term);

    Ok(())
}

pub fn commit_search_index() -> Result<(), String> {
    let si = init_search_index()?;
    let mut writer_guard = si.writer.lock().unwrap();
    writer_guard.commit().map_err(|e| e.to_string())?;
    si.reader.reload().map_err(|e| e.to_string())?;
    Ok(())
}

/// Reconcile a list of Tantivy docs against the facts table.
/// Returns only the docs that still exist in facts, and schedules
/// stale index entries (no longer in facts) for deletion.
/// FIX 4: Stale deletions are only STAGED here (delete_term without commit).
/// The drain engine's next 500ms commit cycle will flush them.
/// This avoids the writer-lock race between a background thread and the drain engine.
pub fn reconcile_search_results(
    docs: Vec<TantivyDocument>,
    conn: &Connection,
) -> Vec<TantivyDocument> {
    let si = match init_search_index() {
        Ok(s) => s,
        Err(_) => return docs,
    };

    if docs.is_empty() {
        return docs;
    }

    // Extract keys from documents
    let mut doc_keys = Vec::with_capacity(docs.len());
    for doc in &docs {
        let doc_volume = doc.get_first(si.volume).and_then(|v| v.as_str()).unwrap_or("");
        let doc_file_id = doc.get_first(si.file_id).and_then(|v| v.as_u64()).unwrap_or(0);
        if !doc_volume.is_empty() && doc_file_id != 0 {
            doc_keys.push((doc_volume.to_string(), doc_file_id));
        }
    }

    // Query all existing keys in batches of 100 to stay under SQL parameter limits
    let mut existing_keys = HashSet::new();
    for chunk in doc_keys.chunks(100) {
        let mut query = String::from("SELECT volume, file_id FROM facts WHERE ");
        let mut params = Vec::new();
        for (i, (vol, fid)) in chunk.iter().enumerate() {
            if i > 0 {
                query.push_str(" OR ");
            }
            query.push_str(&format!("(volume = ?{} AND file_id = ?{})", i * 2 + 1, i * 2 + 2));
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

    for doc in docs {
        let doc_volume = match doc.get_first(si.volume).and_then(|v| v.as_str()) {
            Some(v) => v.to_string(),
            None => { alive.push(doc); continue; }
        };
        let doc_file_id = match doc.get_first(si.file_id).and_then(|v| v.as_u64()) {
            Some(id) => id,
            None => { alive.push(doc); continue; }
        };

        if existing_keys.contains(&(doc_volume.clone(), doc_file_id)) {
            alive.push(doc);
        } else {
            stale_uids.push((doc_volume, doc_file_id));
        }
    }

    // Stage removals only — no commit. The drain engine commits on its 500ms cycle.
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
) -> Result<Vec<TantivyDocument>, String> {
    let si = init_search_index()?;
    let searcher = si.reader.searcher();

    let mut sub_queries: Vec<(Occur, Box<dyn Query>)> = Vec::new();

    // 1. Text Query (Name substring match or term match) with exact boost
    if !query_str.trim().is_empty() && query_str != "*" {
        let words: Vec<&str> = query_str.split_whitespace().collect();
        if words.is_empty() {
            sub_queries.push((Occur::Must, Box::new(AllQuery)));
        } else {
            let mut word_queries = Vec::new();
            let query_parser = tantivy::query::QueryParser::for_index(&si.index, vec![si.name]);
            
            for word in words {
                let word_lower = word.to_lowercase();
                let mut should_clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
                
                // 1. Exact term match (highest priority, boosted to 5.0)
                let term = Term::from_field_text(si.name, &word_lower);
                let exact_query = Box::new(TermQuery::new(term.clone(), IndexRecordOption::Basic));
                let boosted_exact = Box::new(tantivy::query::BoostQuery::new(exact_query, 5.0));
                should_clauses.push((Occur::Should, boosted_exact));
                
                // 2. Prefix match (medium priority, e.g. "moch" matches "mocham", boosted to 1.5)
                let prefix_term_str = format!("{}*", word_lower);
                if let Ok(prefix_query) = query_parser.parse_query(&prefix_term_str) {
                    let boosted_prefix = Box::new(tantivy::query::BoostQuery::new(prefix_query, 1.5));
                    should_clauses.push((Occur::Should, boosted_prefix));
                }
                
                // 3. Fuzzy match (lowest priority, edit distance 1 or 2, boosted to 0.8)
                if word_lower.len() > 2 {
                    let max_distance = if word_lower.len() > 5 { 2 } else { 1 };
                    let fuzzy_query = FuzzyTermQuery::new(term, max_distance, true);
                    let boosted_fuzzy = Box::new(tantivy::query::BoostQuery::new(Box::new(fuzzy_query), 0.8));
                    should_clauses.push((Occur::Should, boosted_fuzzy));
                }
                
                let word_combined = Box::new(BooleanQuery::new(should_clauses));
                word_queries.push((Occur::Must, word_combined as Box<dyn Query>));
            }
            let combined_text_query = Box::new(BooleanQuery::new(word_queries));
            sub_queries.push((Occur::Must, combined_text_query));
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
        sub_queries.push((Occur::Must, Box::new(TermQuery::new(term, IndexRecordOption::Basic))));
    }

    // 4. Volume filter
    if let Some(v) = volume_filter {
        let term = Term::from_field_text(si.volume, v);
        sub_queries.push((Occur::Must, Box::new(TermQuery::new(term, IndexRecordOption::Basic))));
    }

    // 5. Size range filter
    if min_size.is_some() || max_size.is_some() {
        let min_val = min_size.unwrap_or(0);
        let max_val = max_size.unwrap_or(u64::MAX);
        let field_name = si.index.schema().get_field_name(si.size).to_string();
        let size_query = Box::new(RangeQuery::new_u64(field_name, min_val..max_val.saturating_add(1)));
        sub_queries.push((Occur::Must, size_query));
    }

    // 6. Modified range filter
    if modified_after.is_some() || modified_before.is_some() {
        let min_time = modified_after.unwrap_or(i64::MIN);
        let max_time = modified_before.unwrap_or(i64::MAX);
        let field_name = si.index.schema().get_field_name(si.modified_at).to_string();
        let time_query = Box::new(RangeQuery::new_i64(field_name, min_time..max_time.saturating_add(1)));
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
    let top_docs = searcher.search(&final_query, &tantivy::collector::TopDocs::with_limit(limit))
        .map_err(|e| e.to_string())?;

    let mut docs = Vec::new();
    for (_score, doc_address) in top_docs {
        if let Ok(doc) = searcher.doc(doc_address) {
            docs.push(doc);
        }
    }
    Ok(docs)
}
