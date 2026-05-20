use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

fn cmd() -> Command {
    Command::cargo_bin("disktracker").unwrap()
}

#[test]
fn e2e_snapshot_flow() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();
    let db_path = dir.path().join("data.db");

    fs::write(root.join("a.txt"), b"hello").unwrap();

    let root_str = root.to_str().unwrap();
    let db_str = db_path.to_str().unwrap();

    cmd()
        .args(["scan", root_str, "--db", db_str, "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"snapshot_id\""));

    cmd()
        .args(["list", "--db", db_str, "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"scan_root\""));

    fs::write(root.join("b.txt"), b"world!").unwrap();

    cmd()
        .args(["scan", root_str, "--db", db_str, "--json"])
        .assert()
        .success();

    cmd()
        .args(["diff", "--db", db_str, "--min-delta", "1", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"net_change_bytes\""));

    cmd()
        .args(["report", "--last", "1d", "--db", db_str, "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"net_change_bytes\""));

    cmd()
        .args(["timeline", root_str, "--db", db_str, "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_bytes\""));
}

#[test]
fn e2e_maintenance_commands() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();
    let db_path = dir.path().join("data.db");

    fs::write(root.join("a.txt"), b"hello").unwrap();

    let root_str = root.to_str().unwrap();
    let db_str = db_path.to_str().unwrap();

    cmd()
        .args(["scan", root_str, "--db", db_str, "--json"])
        .assert()
        .success();

    fs::write(root.join("b.txt"), b"world!").unwrap();

    cmd()
        .args(["scan", root_str, "--db", db_str, "--json"])
        .assert()
        .success();

    cmd()
        .args(["explain", "--last", "1d", "--db", db_str, "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"delta_bytes\"").or(predicate::str::contains("[]")));

    cmd()
        .args(["reconcile", "--db", db_str, "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"snapshot_count\""));

    cmd()
        .args([
            "prune",
            "--keep-last",
            "1",
            "--dry-run",
            "--db",
            db_str,
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"dry_run\""));

    cmd()
        .args(["watch", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("debounce"));
}

#[test]
fn e2e_lazy_reconcile_flow() {
    use disktracker_db::mutation::{insert_mutations_batch, path_to_bytes};
    use disktracker_db::open_db;
    use disktracker_db::watch_state::{upsert_watch_state, WatchState};
    use disktracker_events::{FsEvent, FsEventKind};
    use std::fs::{self, File};
    use std::io::Write;

    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();
    let db_path = dir.path().join("data.db");

    let root_str = root.to_str().unwrap();
    let db_str = db_path.to_str().unwrap();

    // 1. Initial full scan to set up snapshot
    let file_path = root.join("a.txt");
    {
        let mut f = File::create(&file_path).unwrap();
        f.write_all(&[0; 100]).unwrap();
    }

    cmd()
        .args(["scan", root_str, "--db", db_str, "--json"])
        .assert()
        .success();

    // Let's open the db to register watch state & log a mutation
    let conn = open_db(&db_path).unwrap();

    // Query snapshot ID
    let snapshot_id: i64 = conn
        .query_row("SELECT MAX(id) FROM snapshots", [], |row| row.get(0))
        .unwrap();

    // Let's add a watch state entry
    let watch_state = WatchState {
        watch_root: path_to_bytes(&root),
        last_event_time: Some(1000),
        last_reconcile_time: Some(1000),
        last_snapshot_id: Some(snapshot_id),
    };
    upsert_watch_state(&conn, &watch_state).unwrap();

    // 2. Modify file to trigger a drift
    {
        let mut f = File::create(&file_path).unwrap();
        f.write_all(&[0; 150]).unwrap(); // grow by 50 bytes
    }

    let event = FsEvent {
        timestamp: 1010,
        kind: FsEventKind::Modify,
        path: path_to_bytes(&file_path),
        is_dir: false,
    };
    insert_mutations_batch(&conn, &[event]).unwrap();

    // 3. Run CLI reconcile command (default is lazy reconcile)
    cmd()
        .args(["reconcile", "--db", db_str, "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"drift_bytes\": 50"));
}

#[test]
fn e2e_scan_all_command() {
    let dir = tempdir().unwrap();
    let root1 = dir.path().join("drive1");
    let root2 = dir.path().join("drive2");
    fs::create_dir(&root1).unwrap();
    fs::create_dir(&root2).unwrap();
    let db_path = dir.path().join("data.db");

    fs::write(root1.join("a.txt"), b"hello").unwrap();
    fs::write(root2.join("b.txt"), b"world!!").unwrap();

    let db_str = db_path.to_str().unwrap();
    let mock_roots = format!("{},{}", root1.to_str().unwrap(), root2.to_str().unwrap());

    // Run scan --all
    cmd()
        .env("DISKTRACKER_MOCK_ALL_DRIVES", &mock_roots)
        .args(["scan", "--all", "--db", db_str, "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"snapshot_id\""));

    // Verify both snapshots exist in the database
    let conn = disktracker_db::open_db(&db_path).unwrap();
    let snaps = disktracker_db::store::list_snapshots(&conn).unwrap();
    assert_eq!(snaps.len(), 2);

    let root1_str = root1.to_string_lossy().into_owned();
    let root2_str = root2.to_string_lossy().into_owned();
    assert!(snaps.iter().any(|s| s.scan_root == root1_str));
    assert!(snaps.iter().any(|s| s.scan_root == root2_str));
}

#[test]
fn e2e_watch_all_command() {
    let dir = tempdir().unwrap();
    let root1 = dir.path().join("drive1");
    let root2 = dir.path().join("drive2");
    fs::create_dir(&root1).unwrap();
    fs::create_dir(&root2).unwrap();
    let db_path = dir.path().join("data.db");

    fs::write(root1.join("a.txt"), b"hello").unwrap();
    fs::write(root2.join("b.txt"), b"world!!").unwrap();

    let db_str = db_path.to_str().unwrap();
    let mock_roots = format!("{},{}", root1.to_str().unwrap(), root2.to_str().unwrap());

    // Spawn watch --all as a child process
    let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin("disktracker"));
    command.env("DISKTRACKER_MOCK_ALL_DRIVES", &mock_roots);
    command.args([
        "watch",
        "--all",
        "--db",
        db_str,
        "--debounce-ms",
        "50",
        "--flush-secs",
        "1",
    ]);

    let mut child = command.spawn().unwrap();

    // Sleep a short while to allow the watcher to perform initial scanning
    std::thread::sleep(std::time::Duration::from_millis(600));

    // Grow file inside root1
    fs::write(root1.join("a.txt"), b"hello, universe!").unwrap();

    // Sleep to let watcher detect event, debounce, and flush
    std::thread::sleep(std::time::Duration::from_millis(2500));

    // Terminate watch daemon
    child.kill().unwrap();
    let _ = child.wait();

    // Verify snapshots and watch state were created
    let conn = disktracker_db::open_db(&db_path).unwrap();
    let snaps = disktracker_db::store::list_snapshots(&conn).unwrap();
    assert!(!snaps.is_empty());

    // Reconcile and check sizes
    let root1_str = root1.to_string_lossy().into_owned();

    // Get all snapshots for root1
    let mut stmt = conn
        .prepare("SELECT total_bytes FROM snapshots WHERE scan_root = ?1 ORDER BY id ASC")
        .unwrap();
    let sizes: Vec<i64> = stmt
        .query_map([&root1_str], |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    assert!(
        sizes.len() >= 2,
        "Expected at least 2 snapshots for root1 (initial + flush)"
    );
    let first_size = sizes.first().unwrap();
    let last_size = sizes.last().unwrap();

    // Size should have increased because we appended 11 bytes to a.txt
    assert!(
        last_size > first_size,
        "Watcher should have recorded an increased size for root1"
    );
}
