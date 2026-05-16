use disktracker_db::open_db;
use disktracker_watch::incremental::{path_to_bytes, IncrementalEngine};
use std::fs::{self, OpenOptions};
use std::io::Write;
use tempfile::tempdir;

#[test]
fn incremental_engine_tracks_growth() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();

    let file_path = root.join("file.txt");
    fs::write(&file_path, b"12345").unwrap();

    let (mut engine, _result) = IncrementalEngine::init(root.clone(), vec![], false);

    let mut file = OpenOptions::new().append(true).open(&file_path).unwrap();
    file.write_all(b"6789").unwrap();
    file.sync_all().ok();

    engine.dirty.mark_dirty(path_to_bytes(&root));

    let conn = open_db(&dir.path().join("db.sqlite")).unwrap();
    let deltas = engine.process_dirty_batch(&conn).unwrap();

    assert_eq!(deltas.len(), 1);
    assert!(deltas[0].delta_bytes >= 4);
}
