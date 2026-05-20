use chrono::{TimeZone, Utc};
use disktracker_db::open_db;
use disktracker_db::store::{insert_snapshot, list_snapshots, resolve_snapshot_ref};
use tempfile::tempdir;

#[test]
fn resolve_snapshot_ref_by_id_and_date() {
    let dir = tempdir().unwrap();
    let conn = open_db(&dir.path().join("db.sqlite")).unwrap();

    let now = Utc::now().timestamp();
    let snap_id = insert_snapshot(&conn, "/tmp", now, now, 1, 10, 0).unwrap();

    let resolved_id = resolve_snapshot_ref(&conn, &snap_id.to_string()).unwrap();
    assert_eq!(resolved_id, snap_id);

    let date = Utc
        .timestamp_opt(now, 0)
        .single()
        .unwrap()
        .date_naive()
        .to_string();
    let resolved_date = resolve_snapshot_ref(&conn, &date).unwrap();
    assert_eq!(resolved_date, snap_id);

    let snapshots = list_snapshots(&conn).unwrap();
    assert_eq!(snapshots.len(), 1);
}

#[test]
fn identity_roundtrip() {
    use disktracker_core::arena::PathlessArena;
    use disktracker_core::identity::FsIdentity;
    use disktracker_db::store::{bulk_insert_dirs, load_snapshot_index};

    let dir = tempdir().unwrap();
    let conn = open_db(&dir.path().join("db.sqlite")).unwrap();

    let now = Utc::now().timestamp();
    let snap_id = insert_snapshot(&conn, "/tmp", now, now, 1, 10, 0).unwrap();

    // Create an arena and push some nodes
    let mut arena = PathlessArena::with_capacity(10, 100);
    let root_sym = arena.intern(b"/tmp");
    let root_idx = arena.push_node(disktracker_core::arena::NO_PARENT, root_sym, 0);

    // Set some cold metadata
    let expected_identity = FsIdentity {
        dev: 12345,
        ino: 67890,
    };
    arena.cold[root_idx as usize].mtime = 1337;
    arena.cold[root_idx as usize].identity = expected_identity;

    // Bulk insert
    bulk_insert_dirs(&conn, snap_id, &arena).unwrap();

    // Load snapshot index
    let index = load_snapshot_index(&conn, snap_id).unwrap();

    // Verify identity mapping
    let cached = index
        .by_identity
        .get(&expected_identity)
        .expect("Should find identity in index");
    assert_eq!(cached.mtime, 1337);
    assert_eq!(cached.total_bytes, 0);
    assert_eq!(cached.file_count, 0);

    // Verify path mapping
    let cached_by_path = index
        .by_path
        .get(b"/tmp" as &[u8])
        .expect("Should find path in index");
    assert_eq!(cached_by_path.mtime, 1337);
}

#[test]
fn test_schema_migration() {
    let dir = tempdir().unwrap();
    let db_file = dir.path().join("db.sqlite");

    // Initialize database with an older schema (without dev and ino in dir_snapshots)
    {
        let conn = rusqlite::Connection::open(&db_file).unwrap();
        conn.execute_batch(
            "CREATE TABLE dir_snapshots (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                snapshot_id     INTEGER NOT NULL,
                path_blob       BLOB    NOT NULL,
                path_utf8       TEXT,
                depth           INTEGER NOT NULL,
                total_bytes     INTEGER NOT NULL,
                file_count      INTEGER NOT NULL,
                mtime           INTEGER NOT NULL
            );",
        )
        .unwrap();
    }

    // Now open with open_db, which should dynamically add the dev and ino columns
    let conn = open_db(&db_file).unwrap();

    // Verify columns exist by executing a query on them
    let dev_val: i64 = conn
        .query_row("SELECT dev FROM dir_snapshots LIMIT 1", [], |r| r.get(0))
        .unwrap_or(0);
    assert_eq!(dev_val, 0);

    let ino_val: i64 = conn
        .query_row("SELECT ino FROM dir_snapshots LIMIT 1", [], |r| r.get(0))
        .unwrap_or(0);
    assert_eq!(ino_val, 0);
}

#[test]
fn test_mutation_log_operations() {
    use disktracker_db::mutation::{
        clear_mutation_log, get_mutations_since, insert_mutation, MutationRecord, MutationType,
    };

    let dir = tempdir().unwrap();
    let conn = open_db(&dir.path().join("db.sqlite")).unwrap();

    let record = MutationRecord {
        id: None,
        timestamp: 1000,
        mutation_type: MutationType::Rename,
        dev: 12,
        ino: 34,
        path_blob: b"/new/path".to_vec(),
        old_size: Some(100),
        new_size: Some(200),
        old_path_blob: Some(b"/old/path".to_vec()),
    };

    let id = insert_mutation(&conn, &record).unwrap();
    assert!(id > 0);

    let mutations = get_mutations_since(&conn, 500, 10).unwrap();
    assert_eq!(mutations.len(), 1);
    let loaded = mutations[0].clone();
    assert_eq!(loaded.id, Some(id));
    assert_eq!(loaded.timestamp, record.timestamp);
    assert_eq!(loaded.mutation_type, record.mutation_type);
    assert_eq!(loaded.dev, record.dev);
    assert_eq!(loaded.ino, record.ino);
    assert_eq!(loaded.path_blob, record.path_blob);
    assert_eq!(loaded.old_size, record.old_size);
    assert_eq!(loaded.new_size, record.new_size);
    assert_eq!(loaded.old_path_blob, record.old_path_blob);

    // Verify filter since_ts works
    let empty_mutations = get_mutations_since(&conn, 1500, 10).unwrap();
    assert_eq!(empty_mutations.len(), 0);

    // Clear mutations
    clear_mutation_log(&conn).unwrap();
    let mutations_after_clear = get_mutations_since(&conn, 500, 10).unwrap();
    assert_eq!(mutations_after_clear.len(), 0);
}

#[test]
fn test_insert_mutations_batch() {
    use disktracker_db::mutation::{get_mutations_since, insert_mutations_batch, path_to_bytes};
    use disktracker_events::{FsEvent, FsEventKind};
    use std::fs::File;
    use std::io::Write;

    let dir = tempdir().unwrap();
    let db_file = dir.path().join("db.sqlite");
    let conn = open_db(&db_file).unwrap();

    // Create a real file on disk to verify metadata resolution
    let file_path = dir.path().join("test_file.txt");
    {
        let mut file = File::create(&file_path).unwrap();
        file.write_all(b"hello world").unwrap(); // 11 bytes
    }

    let path_bytes = path_to_bytes(&file_path);
    let event = FsEvent {
        timestamp: 2000,
        kind: FsEventKind::Create,
        path: path_bytes.clone(),
        is_dir: false,
    };

    insert_mutations_batch(&conn, &[event]).unwrap();

    let mutations = get_mutations_since(&conn, 1000, 10).unwrap();
    assert_eq!(mutations.len(), 1);
    let m = &mutations[0];
    assert_eq!(m.timestamp, 2000);
    assert_eq!(m.path_blob, path_bytes);
    // Since the file exists and was written, new_size should be resolved as 11 bytes
    assert_eq!(m.new_size, Some(11));
    #[cfg(unix)]
    {
        assert!(m.dev > 0);
        assert!(m.ino > 0);
    }
}

#[test]
fn test_lazy_reconcile_correctness() {
    use disktracker_db::mutation::{
        get_mutations_since, insert_mutations_batch, lazy_reconcile, path_to_bytes,
    };
    use disktracker_events::{FsEvent, FsEventKind};
    use std::fs::{self, File};
    use std::io::Write;

    let dir = tempdir().unwrap();
    let db_file = dir.path().join("db.sqlite");
    let conn = open_db(&db_file).unwrap();

    // 1. Create directory structure on disk:
    // root_dir/
    //   subdir/
    //     file2.txt (20 bytes)
    //   file1.txt (10 bytes)
    let root_path = dir.path().join("root_dir");
    let subdir_path = root_path.join("subdir");
    fs::create_dir_all(&subdir_path).unwrap();

    let file1_path = root_path.join("file1.txt");
    {
        let mut f = File::create(&file1_path).unwrap();
        f.write_all(&[0; 10]).unwrap();
    }

    let file2_path = subdir_path.join("file2.txt");
    {
        let mut f = File::create(&file2_path).unwrap();
        f.write_all(&[0; 20]).unwrap();
    }

    // 2. Set up initial db snapshot for root_path, subdir_path
    conn.execute(
        "INSERT INTO snapshots (scan_root, started_at, finished_at, total_files, total_bytes, host)
         VALUES ('root_dir', 1000, 1010, 2, 30, 'localhost')",
        [],
    )
    .unwrap();
    let snapshot_id = conn.last_insert_rowid();

    // Manually insert parent/directory snapshots
    let root_blob = path_to_bytes(&root_path);
    let subdir_blob = path_to_bytes(&subdir_path);
    let file2_blob = path_to_bytes(&file2_path);

    conn.execute(
        "INSERT INTO dir_snapshots (snapshot_id, path_blob, path_utf8, depth, total_bytes, file_count, mtime)
         VALUES (?1, ?2, 'root_dir', 0, 30, 2, 1000)",
        rusqlite::params![snapshot_id, root_blob],
    ).unwrap();

    conn.execute(
        "INSERT INTO dir_snapshots (snapshot_id, path_blob, path_utf8, depth, total_bytes, file_count, mtime)
         VALUES (?1, ?2, 'root_dir/subdir', 1, 20, 1, 1000)",
        rusqlite::params![snapshot_id, subdir_blob],
    ).unwrap();

    // 3. Log a mutation: file2.txt is updated and grows from 20 bytes to 50 bytes.
    // Rewrite file2.txt with 50 bytes
    {
        let mut f = File::create(&file2_path).unwrap();
        f.write_all(&[0; 50]).unwrap();
    }

    let event = FsEvent {
        timestamp: 1050,
        kind: FsEventKind::Modify,
        path: file2_blob.clone(),
        is_dir: false,
    };

    insert_mutations_batch(&conn, &[event]).unwrap();

    // Verify it got inserted into the mutation log with correct old_size and new_size
    let mutations = get_mutations_since(&conn, 1020, 10).unwrap();
    assert_eq!(mutations.len(), 1);
    assert_eq!(mutations[0].old_size, None);
    assert_eq!(mutations[0].new_size, Some(50));

    // 4. Perform lazy reconciliation!
    let drift = lazy_reconcile(&conn, snapshot_id, 1020).unwrap();
    // Since file2.txt is updated from 20 to 50 bytes, drift should be 30 bytes.
    assert_eq!(drift, 30);

    // Verify bottom-up propagation:
    // subdir_path bytes should be updated from 20 to 50 bytes
    let subdir_bytes: i64 = conn
        .query_row(
            "SELECT total_bytes FROM dir_snapshots WHERE path_blob = ?1 AND snapshot_id = ?2",
            rusqlite::params![subdir_blob, snapshot_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(subdir_bytes, 50);

    // root_path bytes should be updated from 30 to 60 bytes
    let root_bytes: i64 = conn
        .query_row(
            "SELECT total_bytes FROM dir_snapshots WHERE path_blob = ?1 AND snapshot_id = ?2",
            rusqlite::params![root_blob, snapshot_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(root_bytes, 60);

    // Verify snapshot aggregate size is updated from 30 to 60 bytes
    let total_snap_bytes: i64 = conn
        .query_row(
            "SELECT total_bytes FROM snapshots WHERE id = ?1",
            rusqlite::params![snapshot_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(total_snap_bytes, 60);
}
