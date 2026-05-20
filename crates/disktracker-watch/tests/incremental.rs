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

#[test]
fn test_non_recursive_subtree_propagation() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    let subdir1 = root.join("subdir1");
    let subdir2 = root.join("subdir2");

    fs::create_dir_all(&subdir1).unwrap();
    fs::create_dir_all(&subdir2).unwrap();

    let nested_file1 = subdir1.join("nested_file.txt");
    fs::write(&nested_file1, [0; 10]).unwrap(); // 10 bytes

    let file2 = subdir2.join("file2.txt");
    fs::write(&file2, [0; 20]).unwrap(); // 20 bytes

    // 1. Initialize engine
    let (mut engine, _result) = IncrementalEngine::init(root.clone(), vec![], false);

    let root_bytes = path_to_bytes(&root);
    let sub1_bytes = path_to_bytes(&subdir1);
    let sub2_bytes = path_to_bytes(&subdir2);

    assert_eq!(engine.current_size(&root_bytes), 30);
    assert_eq!(engine.current_size(&sub1_bytes), 10);
    assert_eq!(engine.current_size(&sub2_bytes), 20);

    // 2. Modify nested_file.txt (grow from 10 to 25 bytes)
    fs::write(&nested_file1, [0; 25]).unwrap();

    // Mark subdir1 dirty
    engine.dirty.mark_dirty(sub1_bytes.clone());

    let conn = open_db(&dir.path().join("db.sqlite")).unwrap();
    let deltas = engine.process_dirty_batch(&conn).unwrap();

    // Verify deltas and propagation
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].delta_bytes, 15);
    assert_eq!(engine.current_size(&sub1_bytes), 25);
    // Root size should have been recursively updated to 45 (30 + 15)
    assert_eq!(engine.current_size(&root_bytes), 45);
    // Subdir2 size must remain unchanged (20) without being rescanned
    assert_eq!(engine.current_size(&sub2_bytes), 20);

    // 3. Delete subdir2 entirely
    fs::remove_dir_all(&subdir2).unwrap();

    // Mark root dirty
    engine.dirty.mark_dirty(root_bytes.clone());
    let deltas2 = engine.process_dirty_batch(&conn).unwrap();

    // Root should decrease by 20 bytes
    assert_eq!(deltas2.len(), 1);
    assert_eq!(deltas2[0].delta_bytes, -20);
    assert_eq!(engine.current_size(&root_bytes), 25);

    // subdir2 must have been recursively cleaned up from state
    assert_eq!(engine.current_size(&sub2_bytes), 0);
    assert!(!engine.state.contains_key(&sub2_bytes));
}

#[test]
fn test_sibling_with_shared_prefix() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    let subdir = root.join("sub");
    let sibling = root.join("sub_sibling");

    fs::create_dir_all(&subdir).unwrap();
    fs::create_dir_all(&sibling).unwrap();

    let file1 = subdir.join("file.txt");
    fs::write(&file1, [0; 10]).unwrap();

    let file2 = sibling.join("file.txt");
    fs::write(&file2, [0; 20]).unwrap();

    let (mut engine, _result) = IncrementalEngine::init(root.clone(), vec![], false);

    let root_bytes = path_to_bytes(&root);
    let sub_bytes = path_to_bytes(&subdir);
    let sib_bytes = path_to_bytes(&sibling);

    assert_eq!(engine.current_size(&root_bytes), 30);
    assert_eq!(engine.current_size(&sub_bytes), 10);
    assert_eq!(engine.current_size(&sib_bytes), 20);

    // Modify a file in subdir
    fs::write(&file1, [0; 15]).unwrap();

    // Mark subdir dirty
    engine.dirty.mark_dirty(sub_bytes.clone());

    let conn = open_db(&dir.path().join("db.sqlite")).unwrap();
    let _deltas = engine.process_dirty_batch(&conn).unwrap();

    // Verify sibling is NOT deleted or changed!
    assert!(engine.state.contains_key(&sib_bytes));
    assert_eq!(engine.current_size(&sib_bytes), 20);
    assert_eq!(engine.current_size(&sub_bytes), 15);
    assert_eq!(engine.current_size(&root_bytes), 35);
}

#[test]
fn test_mmap_index_serialization_and_binary_search() {
    let temp = tempdir().unwrap();
    let mmap_path = temp.path().join("index.mmap");

    // 1. Create a dummy state and insert elements out of order (to test alphabetical sorting on save)
    let mut state = disktracker_watch::mmap_index::MmapState::new(None);
    state.insert(b"/root/z_dir".to_vec(), 100);
    state.insert(b"/root/a_dir".to_vec(), 200);
    state.insert(b"/root/m_dir".to_vec(), 300);

    // Save state
    state.write_to_file(&mmap_path).unwrap();

    // 2. Load the state back from the memory map
    let loaded = disktracker_watch::mmap_index::MmapState::load_from_file(&mmap_path).unwrap();
    assert_eq!(loaded.len(), 3);

    // Check sizes - must be fast lookup directly via binary search on mapped bytes
    assert_eq!(loaded.get(b"/root/a_dir"), Some(200));
    assert_eq!(loaded.get(b"/root/m_dir"), Some(300));
    assert_eq!(loaded.get(b"/root/z_dir"), Some(100));
    assert_eq!(loaded.get(b"/root/nonexistent"), None);

    // Check sorted order of active entries
    let active = loaded.get_active_entries();
    assert_eq!(active[0].0, b"/root/a_dir");
    assert_eq!(active[1].0, b"/root/m_dir");
    assert_eq!(active[2].0, b"/root/z_dir");
}

#[test]
fn test_mmap_state_overlay_and_tombstones() {
    let temp = tempdir().unwrap();
    let mmap_path = temp.path().join("index.mmap");

    // 1. Setup base index
    let mut state = disktracker_watch::mmap_index::MmapState::new(None);
    state.insert(b"/root".to_vec(), 500);
    state.insert(b"/root/subdir1".to_vec(), 200);
    state.insert(b"/root/subdir2".to_vec(), 300);
    state.write_to_file(&mmap_path).unwrap();

    // 2. Hydrate from file
    let mut state = disktracker_watch::mmap_index::MmapState::load_from_file(&mmap_path).unwrap();

    // 3. Make runtime mutations in the LSM-style overlay
    state.insert(b"/root/subdir1".to_vec(), 250); // modify
    state.insert(b"/root/subdir3".to_vec(), 150); // new directory

    // Check lookups see the overlay updates
    assert_eq!(state.get(b"/root/subdir1"), Some(250));
    assert_eq!(state.get(b"/root/subdir2"), Some(300)); // still from base mmap
    assert_eq!(state.get(b"/root/subdir3"), Some(150)); // from overlay

    // 4. Test subtree removal / tombstones
    state.remove_subtree(b"/root/subdir1");

    // subdir1 and all subdirectories should return None (tombstoned)
    assert_eq!(state.get(b"/root/subdir1"), None);
    assert_eq!(state.get(b"/root/subdir2"), Some(300)); // unaffected sibling

    // Write back and reload (compaction pass)
    state.write_to_file(&mmap_path).unwrap();
    let compacted = disktracker_watch::mmap_index::MmapState::load_from_file(&mmap_path).unwrap();

    assert_eq!(compacted.get(b"/root/subdir1"), None);
    assert_eq!(compacted.get(b"/root/subdir2"), Some(300));
    assert_eq!(compacted.get(b"/root/subdir3"), Some(150));
    assert_eq!(compacted.len(), 3); // /root, /root/subdir2, /root/subdir3
}

#[test]
fn test_watch_multi_roots() {
    let dir = tempdir().unwrap();
    let root1 = dir.path().join("root1");
    let root2 = dir.path().join("root2");

    fs::create_dir_all(&root1).unwrap();
    fs::create_dir_all(&root2).unwrap();

    let file1 = root1.join("a.txt");
    fs::write(&file1, [0; 50]).unwrap();

    let file2 = root2.join("b.txt");
    fs::write(&file2, [0; 100]).unwrap();

    // Initialize with two roots
    let (mut engine, _result) =
        IncrementalEngine::init_multi(vec![root1.clone(), root2.clone()], vec![], false);

    let root1_bytes = path_to_bytes(&root1);
    let root2_bytes = path_to_bytes(&root2);

    assert_eq!(engine.current_size(&root1_bytes), 50);
    assert_eq!(engine.current_size(&root2_bytes), 100);

    // Grow root1 file
    fs::write(&file1, [0; 80]).unwrap();
    engine.dirty.mark_dirty(root1_bytes.clone());

    // Grow root2 file
    fs::write(&file2, [0; 150]).unwrap();
    engine.dirty.mark_dirty(root2_bytes.clone());

    let conn = open_db(&dir.path().join("db.sqlite")).unwrap();
    let _deltas = engine.process_dirty_batch(&conn).unwrap();

    // Verify deltas and sizes for both roots are correctly recorded and propagated
    assert_eq!(engine.current_size(&root1_bytes), 80);
    assert_eq!(engine.current_size(&root2_bytes), 150);
}
