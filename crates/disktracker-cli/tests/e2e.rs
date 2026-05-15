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
