use chrono::Utc;
use disktracker_core::arena::{DirNode, PathlessArena};
use disktracker_db::explain::query_explain;
use disktracker_db::open_db;
use disktracker_db::store::{bulk_insert_dirs, insert_snapshot};
use tempfile::tempdir;

fn make_arena(root_bytes: u64, node_modules_bytes: u64) -> PathlessArena {
    let mut arena = PathlessArena::with_capacity(8, 128);
    let root_sym = arena.intern(b"/");
    let root_idx = arena.push(DirNode {
        parent: None,
        name: root_sym,
        total_bytes: root_bytes,
        file_count: 0,
        mtime: 0,
        depth: 0,
    });
    let nm_sym = arena.intern(b"node_modules");
    arena.push(DirNode {
        parent: PathlessArena::encode_parent(root_idx),
        name: nm_sym,
        total_bytes: node_modules_bytes,
        file_count: 0,
        mtime: 0,
        depth: 1,
    });
    arena
}

#[test]
fn explain_attributes_known_paths() {
    let dir = tempdir().unwrap();
    let conn = open_db(&dir.path().join("db.sqlite")).unwrap();

    let now = Utc::now().timestamp();
    let snap_a = insert_snapshot(&conn, "/", now - 10, now - 9, 1, 100, 0).unwrap();
    bulk_insert_dirs(&conn, snap_a, &make_arena(100, 80)).unwrap();

    let snap_b = insert_snapshot(&conn, "/", now, now + 1, 1, 300, 0).unwrap();
    bulk_insert_dirs(&conn, snap_b, &make_arena(300, 250)).unwrap();

    let entries = query_explain(&conn, snap_a, snap_b, 10).unwrap();
    let found = entries
        .iter()
        .any(|e| e.label.as_deref() == Some("npm packages"));
    assert!(found);
}
