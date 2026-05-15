use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Serialize;

/// Pattern → human-readable attribution label.
const GROWTH_PATTERNS: &[(&str, &str)] = &[
    ("node_modules", "npm packages"),
    (".cargo/registry", "Rust crate cache"),
    (".cargo/git", "Rust git crate cache"),
    ("overlay2", "Docker image layers"),
    ("docker/volumes", "Docker volumes"),
    (".ollama/models", "Ollama models"),
    ("huggingface", "HuggingFace models"),
    (".cache/huggingface", "HuggingFace model cache"),
    ("Google/Chrome", "Chrome cache"),
    (".cache/pip", "Python pip cache"),
    ("__pycache__", "Python bytecode"),
    (".gradle", "Gradle build cache"),
    ("target/debug", "Rust debug build artifacts"),
    ("target/release", "Rust release build artifacts"),
    (".npm", "npm cache"),
    (".pnpm-store", "pnpm package store"),
    ("Library/Caches", "macOS app caches"),
    ("AppData/Local/Temp", "Windows temp files"),
    ("/tmp", "Temporary files"),
    ("Trash", "Trash / Recycle Bin"),
    (".git/objects", "Git object store"),
    ("node_modules/.cache", "Bundler cache"),
    (".yarn/cache", "Yarn package cache"),
    ("venv", "Python virtual environment"),
    (".venv", "Python virtual environment"),
    ("site-packages", "Python site packages"),
    ("go/pkg/mod", "Go module cache"),
    (".m2/repository", "Maven package cache"),
    ("vendor", "Vendored dependencies"),
];

#[derive(Debug, Serialize)]
pub struct ExplainEntry {
    pub path: String,
    pub delta_bytes: i64,
    pub label: Option<String>,
}

/// Explain the growth between two snapshots. Applies attribution heuristics.
pub fn query_explain(
    conn: &Connection,
    snapshot_a: i64,
    snapshot_b: i64,
    top: usize,
) -> Result<Vec<ExplainEntry>> {
    // Ensure diff_cache is populated
    let cached: i64 = conn.query_row(
        "SELECT COUNT(*) FROM diff_cache WHERE snapshot_a=?1 AND snapshot_b=?2",
        params![snapshot_a, snapshot_b],
        |row| row.get(0),
    )?;

    if cached == 0 {
        conn.execute_batch(&format!(
            r#"
            INSERT INTO diff_cache (snapshot_a, snapshot_b, path_blob, bytes_a, bytes_b, delta_bytes)
            SELECT {sa},{sb}, path_blob, bytes_a, bytes_b, delta_bytes FROM (
                SELECT path_blob,
                    SUM(CASE WHEN which='A' THEN total_bytes ELSE NULL END) AS bytes_a,
                    SUM(CASE WHEN which='B' THEN total_bytes ELSE NULL END) AS bytes_b,
                    COALESCE(SUM(CASE WHEN which='B' THEN total_bytes ELSE 0 END),0)
                    - COALESCE(SUM(CASE WHEN which='A' THEN total_bytes ELSE 0 END),0) AS delta_bytes
                FROM (
                    SELECT path_blob, total_bytes, 'A' AS which FROM dir_snapshots WHERE snapshot_id={sa}
                    UNION ALL
                    SELECT path_blob, total_bytes, 'B' AS which FROM dir_snapshots WHERE snapshot_id={sb}
                ) combined
                GROUP BY path_blob
            ) computed WHERE delta_bytes != 0;
            "#,
            sa = snapshot_a, sb = snapshot_b
        ))?;
    }

    let mut stmt = conn.prepare(
        r#"
        SELECT COALESCE(ds.path_utf8, CAST(dc.path_blob AS TEXT)), dc.delta_bytes
        FROM diff_cache dc
        LEFT JOIN dir_snapshots ds
            ON ds.path_blob = dc.path_blob AND ds.snapshot_id = ?2
        WHERE dc.snapshot_a = ?1 AND dc.snapshot_b = ?2
          AND dc.delta_bytes > 0
        ORDER BY dc.delta_bytes DESC
        LIMIT ?3
        "#,
    )?;

    let rows = stmt.query_map(params![snapshot_a, snapshot_b, top as i64 * 3], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?
                .unwrap_or_else(|| "<non-utf8>".to_owned()),
            row.get::<_, i64>(1)?,
        ))
    })?;

    let mut entries: Vec<ExplainEntry> = rows
        .filter_map(|r| r.ok())
        .map(|(path, delta)| {
            let label = attribute_path(&path);
            ExplainEntry {
                path,
                delta_bytes: delta,
                label,
            }
        })
        .collect();

    // Collapse entries with same label into a single entry
    let mut attributed: Vec<ExplainEntry> = Vec::new();
    for entry in entries.drain(..) {
        if let Some(lbl) = &entry.label {
            if let Some(existing) = attributed
                .iter_mut()
                .find(|e| e.label.as_deref() == Some(lbl.as_str()))
            {
                existing.delta_bytes += entry.delta_bytes;
                continue;
            }
        }
        attributed.push(entry);
    }

    attributed.sort_by_key(|b| std::cmp::Reverse(b.delta_bytes));
    Ok(attributed)
}

fn attribute_path(path: &str) -> Option<String> {
    for (pattern, label) in GROWTH_PATTERNS {
        if path.contains(pattern) {
            return Some((*label).to_owned());
        }
    }
    None
}
