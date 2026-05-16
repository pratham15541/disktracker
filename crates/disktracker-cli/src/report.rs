use crate::diff::{DiffEntry, DiffResult};
use crate::store::SnapshotRecord;
use chrono::{DateTime, TimeZone, Utc};
use disktracker_db::{explain::ExplainEntry, prune::PruneResult, timeline::TimelineEntry};
use serde::Serialize;

// ─── Byte formatting ──────────────────────────────────────────────────────────

pub fn fmt_bytes(bytes: i64) -> String {
    let abs = bytes.unsigned_abs();
    let sign = if bytes < 0 { "-" } else { "+" };
    if abs >= 1_000_000_000 {
        format!("{}{:.1} GB", sign, abs as f64 / 1_000_000_000.0)
    } else if abs >= 1_000_000 {
        format!("{}{:.1} MB", sign, abs as f64 / 1_000_000.0)
    } else if abs >= 1_000 {
        format!("{}{:.1} KB", sign, abs as f64 / 1_000.0)
    } else {
        format!("{}{} B", sign, abs)
    }
}

pub fn fmt_bytes_plain(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{} B", bytes)
    }
}

pub fn fmt_ts(ts: i64) -> String {
    let dt: DateTime<Utc> = Utc.timestamp_opt(ts, 0).single().unwrap_or(Utc::now());
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

pub fn fmt_date(ts: i64) -> String {
    let dt: DateTime<Utc> = Utc.timestamp_opt(ts, 0).single().unwrap_or(Utc::now());
    dt.format("%b %-d").to_string()
}

pub fn fmt_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

// ─── Scan summary ────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ScanSummaryJson {
    pub snapshot_id: i64,
    pub directories: u64,
    pub files: u64,
    pub total_bytes: u64,
    pub duration_ms: u64,
    pub db_path: String,
    pub error_count: u32,
}

pub struct ScanSummary<'a> {
    pub root: &'a str,
    pub directories: u64,
    pub files: u64,
    pub total_bytes: u64,
    pub duration_ms: u64,
    pub snapshot_id: i64,
    pub db_path: &'a str,
    pub error_count: u32,
}

pub fn print_scan_summary(summary: &ScanSummary<'_>, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ScanSummaryJson {
                snapshot_id: summary.snapshot_id,
                directories: summary.directories,
                files: summary.files,
                total_bytes: summary.total_bytes,
                duration_ms: summary.duration_ms,
                db_path: summary.db_path.to_owned(),
                error_count: summary.error_count,
            })
            .unwrap()
        );
        return;
    }
    println!("Scanning {} ...", summary.root);
    println!("  Directories : {}", fmt_number(summary.directories));
    println!("  Files       : {}", fmt_number(summary.files));
    println!("  Total size  : {}", fmt_bytes_plain(summary.total_bytes));
    println!(
        "  Duration    : {:.1} s",
        summary.duration_ms as f64 / 1000.0
    );
    if summary.error_count > 0 {
        println!(
            "  Errors      : {} (permission denied / skipped)",
            fmt_number(summary.error_count as u64)
        );
    }
    println!("  Snapshot ID : {}", summary.snapshot_id);
    println!("  Stored in   : {}", summary.db_path);
}

// ─── Snapshot list ───────────────────────────────────────────────────────────

pub fn print_snapshot_list(snapshots: &[SnapshotRecord], json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &snapshots
                    .iter()
                    .map(|s| serde_json::json!({
                        "id": s.id, "scan_root": s.scan_root,
                        "started_at": s.started_at, "finished_at": s.finished_at,
                        "total_files": s.total_files, "total_bytes": s.total_bytes,
                        "error_count": s.error_count, "host": s.host,
                    }))
                    .collect::<Vec<_>>()
            )
            .unwrap()
        );
        return;
    }
    let sep = "─".repeat(78);
    println!(
        "  {:<4}  {:<19}  {:<30}  {:>12}  {:>10}",
        "ID", "Date/Time", "Root", "Files", "Size"
    );
    println!("  {}", sep);
    for s in snapshots {
        let root_short = if s.scan_root.len() > 28 {
            format!("…{}", &s.scan_root[s.scan_root.len().saturating_sub(27)..])
        } else {
            s.scan_root.clone()
        };
        println!(
            "  {:<4}  {:<19}  {:<30}  {:>12}  {:>10}",
            s.id,
            fmt_ts(s.started_at),
            root_short,
            fmt_number(s.total_files as u64),
            fmt_bytes_plain(s.total_bytes as u64),
        );
    }
}

// ─── Diff output ─────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct DiffJson {
    pub snapshot_a: i64,
    pub snapshot_b: i64,
    pub started_a: String,
    pub started_b: String,
    pub entries: Vec<serde_json::Value>,
    pub net_change_bytes: i64,
}

pub fn print_diff(result: &DiffResult, json: bool) {
    if json {
        let entries: Vec<_> = result
            .entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "path": e.path, "bytes_a": e.bytes_a, "bytes_b": e.bytes_b,
                    "delta_bytes": e.delta_bytes,
                    "status": match (e.bytes_a, e.bytes_b) {
                        (None, _) => "new", (_, None) => "deleted",
                        _ if e.delta_bytes > 0 => "grew", _ => "shrank",
                    }
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&DiffJson {
                snapshot_a: result.snapshot_a,
                snapshot_b: result.snapshot_b,
                started_a: fmt_ts(result.started_a),
                started_b: fmt_ts(result.started_b),
                entries,
                net_change_bytes: result.net_change,
            })
            .unwrap()
        );
        return;
    }

    println!(
        "Diff: snapshot #{} ({}) → snapshot #{} ({})\n",
        result.snapshot_a,
        fmt_ts(result.started_a),
        result.snapshot_b,
        fmt_ts(result.started_b),
    );
    let sep = "─".repeat(65);
    let grew: Vec<&DiffEntry> = result
        .entries
        .iter()
        .filter(|e| e.delta_bytes > 0)
        .collect();
    let shrank: Vec<&DiffEntry> = result
        .entries
        .iter()
        .filter(|e| e.delta_bytes < 0)
        .collect();
    if !grew.is_empty() {
        println!("  GREW\n  {}", sep);
        for e in &grew {
            println!("  {:>10}  {}", fmt_bytes(e.delta_bytes), e.path);
        }
        println!();
    }
    if !shrank.is_empty() {
        println!("  SHRANK\n  {}", sep);
        for e in &shrank {
            let note = if e.bytes_b.is_none() {
                "  (deleted)"
            } else {
                ""
            };
            println!("  {:>10}  {}{}", fmt_bytes(e.delta_bytes), e.path, note);
        }
        println!();
    }
    if grew.is_empty() && shrank.is_empty() {
        println!("  No changes above threshold.\n");
    }
    let days = (result.started_b - result.started_a) / 86400;
    let timeframe = match days {
        0 => "less than a day".to_owned(),
        1 => "1 day".to_owned(),
        n => format!("{} days", n),
    };
    println!(
        "  NET CHANGE: {} over {}",
        fmt_bytes(result.net_change),
        timeframe
    );
}

// ─── Report output ───────────────────────────────────────────────────────────

pub fn print_report(result: &DiffResult, window_label: &str, depth_limit: Option<u16>, json: bool) {
    if json {
        print_diff(result, true);
        return;
    }
    println!("Growth report — last {}\n", window_label);
    let grew: Vec<&DiffEntry> = result
        .entries
        .iter()
        .filter(|e| e.delta_bytes > 0)
        .filter(|e| {
            depth_limit.is_none_or(|max_d| {
                let depth = e.path.chars().filter(|&c| c == '/' || c == '\\').count() as u16;
                depth <= max_d
            })
        })
        .collect();
    if grew.is_empty() {
        println!("  No growth detected in this window.");
        return;
    }
    println!("  FASTEST GROWING FOLDERS\n  {}", "─".repeat(65));
    for e in &grew {
        println!("  {:>10}  {}", fmt_bytes(e.delta_bytes), e.path);
    }
    println!();
    println!("  NET CHANGE: {}", fmt_bytes(result.net_change));
}

// ─── Explain output ───────────────────────────────────────────────────────────

pub fn print_explain(entries: &[ExplainEntry], window: &str, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(entries).unwrap());
        return;
    }
    println!("Largest growth sources — last {}\n", window);
    println!("  {}", "─".repeat(65));
    let mut any = false;
    for e in entries {
        if e.delta_bytes <= 0 {
            continue;
        }
        any = true;
        let label = e.label.as_deref().unwrap_or(&e.path);
        println!("  {:>10}  {}", fmt_bytes(e.delta_bytes), label);
    }
    if !any {
        println!("  No significant growth detected.");
    }
    println!();
}

// ─── Timeline output ──────────────────────────────────────────────────────────

pub fn print_timeline(entries: &[TimelineEntry], path: &str, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(entries).unwrap());
        return;
    }
    println!("Timeline for: {}\n", path);
    if entries.is_empty() {
        println!("  No data found for this path. Run `disktracker scan` to capture snapshots.");
        return;
    }
    println!("  {:<12}  {:>12}  {:>12}", "Date", "Size", "Change");
    println!("  {}", "─".repeat(42));
    let mut prev: Option<i64> = None;
    for e in entries {
        let change = prev
            .map(|p| fmt_bytes(e.total_bytes - p))
            .unwrap_or_else(|| "—".to_owned());
        println!(
            "  {:<12}  {:>12}  {:>12}",
            fmt_date(e.timestamp),
            fmt_bytes_plain(e.total_bytes as u64),
            change,
        );
        prev = Some(e.total_bytes);
    }
    println!();
}

// ─── Reconcile output ─────────────────────────────────────────────────────────

pub fn print_reconcile(
    watch_root: Option<&str>,
    last_event: Option<i64>,
    last_reconcile: Option<i64>,
    snapshot_count: usize,
    new_snap_id: Option<i64>,
    drift_bytes: i64,
    json: bool,
) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "watch_root": watch_root,
                "last_event_ts": last_event,
                "last_reconcile_ts": last_reconcile,
                "snapshot_count": snapshot_count,
                "new_snapshot_id": new_snap_id,
                "drift_bytes": drift_bytes,
            }))
            .unwrap()
        );
        return;
    }
    println!("Reconcile report\n");
    if let Some(root) = watch_root {
        println!("  Watch root    : {}", root);
    }
    if let Some(ts) = last_event {
        println!("  Last event    : {}", fmt_ts(ts));
    }
    if let Some(ts) = last_reconcile {
        println!("  Last reconcile: {}", fmt_ts(ts));
    }
    println!("  Snapshots     : {}", snapshot_count);
    if let Some(id) = new_snap_id {
        println!("  New snapshot  : #{}", id);
    }
    if drift_bytes.abs() > 0 {
        println!("  Drift vs prev : {}", fmt_bytes(drift_bytes));
    } else {
        println!("  Drift vs prev : none detected");
    }
    println!();
}

// ─── Prune output ──────────────────────────────────────────────────────────────

pub fn print_prune(result: &PruneResult, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(result).unwrap());
        return;
    }

    if result.dry_run {
        if result.deleted_snapshot_count == 0 {
            println!("Nothing to prune — no snapshots match the criteria.");
            return;
        }
        println!("Prune preview (--dry-run — no changes made)\n");
        println!("  Snapshots to delete : {}", result.deleted_snapshot_count);
        println!(
            "  IDs                 : {}",
            fmt_id_list(&result.deleted_snapshots)
        );
        println!(
            "  Dir rows freed      : {}",
            fmt_number(result.deleted_dir_rows as u64)
        );
        if result.deleted_delta_rows > 0 {
            println!(
                "  Delta rows freed    : {}",
                fmt_number(result.deleted_delta_rows as u64)
            );
        }
        println!(
            "  Represented size    : {}",
            fmt_bytes_plain(result.freed_bytes_approx.unsigned_abs())
        );
        println!();
        println!("  Run without --dry-run to apply.");
    } else {
        if result.deleted_snapshot_count == 0 {
            println!("Nothing to prune — no snapshots match the criteria.");
            return;
        }
        println!("Pruned {} snapshot(s)\n", result.deleted_snapshot_count);
        println!(
            "  Deleted IDs         : {}",
            fmt_id_list(&result.deleted_snapshots)
        );
        println!(
            "  Dir rows removed    : {}",
            fmt_number(result.deleted_dir_rows as u64)
        );
        if result.deleted_delta_rows > 0 {
            println!(
                "  Delta rows removed  : {}",
                fmt_number(result.deleted_delta_rows as u64)
            );
        }
        println!(
            "  Represented size    : {}",
            fmt_bytes_plain(result.freed_bytes_approx.unsigned_abs())
        );
        println!();
    }
}

fn fmt_id_list(ids: &[i64]) -> String {
    const MAX: usize = 8;
    if ids.len() <= MAX {
        ids.iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        let shown: Vec<String> = ids[..MAX].iter().map(|id| id.to_string()).collect();
        format!("{} … (+{})", shown.join(", "), ids.len() - MAX)
    }
}
