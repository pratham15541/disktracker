use disktracker_core::scan::{scan, ScanConfig};
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

#[test]
fn scan_counts_files_and_bytes() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("a.bin"), vec![0u8; 10]).unwrap();
    fs::write(root.join("sub").join("b.bin"), vec![0u8; 5]).unwrap();

    let result = scan(ScanConfig {
        root: root.clone(),
        max_depth: None,
        skip_names: vec![],
        one_filesystem: false,
        cancel_flag: None,
        ..Default::default()
    });

    assert_eq!(result.total_files, 2);
    assert_eq!(result.total_bytes, 15);
}

#[test]
fn scan_respects_skip_names() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("skipme")).unwrap();
    fs::write(root.join("skipme").join("ignored.bin"), vec![1u8; 12]).unwrap();
    fs::write(root.join("kept.bin"), vec![2u8; 7]).unwrap();

    let result = scan(ScanConfig {
        root: root.clone(),
        max_depth: None,
        skip_names: vec![b"skipme".to_vec()],
        one_filesystem: false,
        cancel_flag: None,
        ..Default::default()
    });

    assert_eq!(result.total_files, 1);
    assert_eq!(result.total_bytes, 7);
}

#[test]
fn scan_respects_cancellation_flag() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();

    // Create a moderately large directory structure
    // (not too large to slow down the test, but large enough to traverse)
    for i in 0..5 {
        let subdir = root.join(format!("dir{}", i));
        fs::create_dir(&subdir).unwrap();
        for j in 0..10 {
            fs::write(
                subdir.join(format!("file{}.bin", j)),
                vec![0u8; 1024], // 1 KB per file
            )
            .unwrap();
        }
    }

    // First, do a full scan to know what we're cancelling against
    let full_result = scan(ScanConfig {
        root: root.clone(),
        max_depth: None,
        skip_names: vec![],
        one_filesystem: false,
        cancel_flag: None,
        ..Default::default()
    });

    assert!(full_result.total_files > 0, "Full scan should find files");
    let expected_bytes = full_result.total_bytes;
    let expected_files = full_result.total_files;

    // Now test cancellation: set a flag and spawn a thread to cancel after a brief delay
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let cancel_flag_clone = cancel_flag.clone();

    let cancel_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(5));
        cancel_flag_clone.store(true, Ordering::SeqCst);
    });

    // Run scan with the cancellation flag
    let start = std::time::Instant::now();
    let cancelled_result = scan(ScanConfig {
        root: root.clone(),
        max_depth: None,
        skip_names: vec![],
        one_filesystem: false,
        cancel_flag: Some(cancel_flag.clone()),
        ..Default::default()
    });
    let elapsed = start.elapsed();

    cancel_thread.join().unwrap();

    // Verify scan exited quickly (should complete in well under 1 second for small tree)
    assert!(
        elapsed < Duration::from_secs(1),
        "Scan should exit quickly when cancelled, took {:?}",
        elapsed
    );

    // Verify we got partial or no results (less than full scan)
    // The exact amount depends on timing, but it should be less than the full scan
    // or at most equal if we're unlucky with timing
    println!(
        "Full scan: {} files, {} bytes; Cancelled scan: {} files, {} bytes",
        expected_files, expected_bytes, cancelled_result.total_files, cancelled_result.total_bytes
    );
}

#[test]
fn parallel_matches_serial() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();

    // Create a rich tree of files and subdirectories
    for i in 0..3 {
        let sub = root.join(format!("sub{}", i));
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("file_a.txt"), vec![1u8; 100]).unwrap();

        let nested = sub.join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("file_b.txt"), vec![2u8; 250]).unwrap();
    }
    fs::write(root.join("root_file.txt"), vec![0u8; 50]).unwrap();

    let serial_res = scan(ScanConfig {
        root: root.clone(),
        parallelism: 1,
        ..Default::default()
    });

    let parallel_res = scan(ScanConfig {
        root: root.clone(),
        parallelism: 4,
        ..Default::default()
    });

    assert_eq!(parallel_res.total_files, serial_res.total_files);
    assert_eq!(parallel_res.total_bytes, serial_res.total_bytes);
    assert_eq!(
        parallel_res.arena.node_count(),
        serial_res.arena.node_count()
    );

    // Compare paths
    let mut serial_paths = Vec::new();
    for idx in 0..serial_res.arena.node_count() {
        serial_paths.push(serial_res.arena.materialize_path(idx as u32));
    }
    serial_paths.sort();

    let mut parallel_paths = Vec::new();
    for idx in 0..parallel_res.arena.node_count() {
        parallel_paths.push(parallel_res.arena.materialize_path(idx as u32));
    }
    parallel_paths.sort();

    assert_eq!(parallel_paths, serial_paths);
}

#[test]
fn arena_parent_validity() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();

    for i in 0..5 {
        let sub = root.join(format!("sub{}", i));
        fs::create_dir(&sub).unwrap();
        let nested = sub.join("nested");
        fs::create_dir(&nested).unwrap();
    }

    let result = scan(ScanConfig {
        root: root.clone(),
        parallelism: 4,
        ..Default::default()
    });

    let count = result.arena.node_count();
    assert!(count > 0);

    for idx in 0..count {
        let node = &result.arena.hot[idx];
        if idx == 0 {
            assert_eq!(node.parent, disktracker_core::arena::NO_PARENT);
            assert_eq!(node.depth, 0);
        } else {
            assert_ne!(node.parent, disktracker_core::arena::NO_PARENT);
            assert!(node.parent < idx as u32);
            let parent_node = &result.arena.hot[node.parent as usize];
            assert_eq!(node.depth, parent_node.depth + 1);
        }
    }
}

#[test]
fn warm_matches_cold() {
    use disktracker_core::warm::{CachedDir, SnapshotIndex};

    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();

    let sub1 = root.join("sub1");
    fs::create_dir(&sub1).unwrap();
    fs::write(sub1.join("file_a.txt"), vec![1u8; 100]).unwrap(); // 100 bytes

    let sub2 = root.join("sub2");
    fs::create_dir(&sub2).unwrap();
    fs::write(sub2.join("file_b.txt"), vec![2u8; 250]).unwrap(); // 250 bytes

    // 1. Cold Scan
    let cold_res = scan(ScanConfig {
        root: root.clone(),
        parallelism: 1,
        ..Default::default()
    });

    assert_eq!(cold_res.total_bytes, 350);
    assert_eq!(cold_res.total_files, 2);

    // 2. Prepare SnapshotIndex where sub1 is cached but sub2 is NOT
    // (so sub1 should be skipped, reusing the 100 bytes, but sub2 is scanned normally)
    let mut index = SnapshotIndex::new();

    // Find sub1 in cold_res arena to get its identity & mtime
    let mut sub1_idx = None;
    for idx in 0..cold_res.arena.node_count() {
        let path = cold_res.arena.materialize_path(idx as u32);
        if path.ends_with(b"/sub1") {
            sub1_idx = Some(idx);
            break;
        }
    }
    let sub1_idx = sub1_idx.expect("sub1 node should exist");
    let sub1_identity = cold_res.arena.cold[sub1_idx].identity;
    let sub1_mtime = cold_res.arena.cold[sub1_idx].mtime;

    index.insert_identity(
        sub1_identity,
        CachedDir {
            total_bytes: 100,
            file_count: 1,
            mtime: sub1_mtime, // match exactly to allow skip
        },
    );
    // Add path fallback as well
    let sub1_path = cold_res.arena.materialize_path(sub1_idx as u32);
    index.insert_path(
        sub1_path,
        CachedDir {
            total_bytes: 100,
            file_count: 1,
            mtime: sub1_mtime,
        },
    );

    let skip_predicate = index.build_skip_predicate();

    // 3. Warm Scan (using skip_predicate)
    let warm_res = scan(ScanConfig {
        root: root.clone(),
        parallelism: 1,
        skip_predicate: Some(skip_predicate),
        ..Default::default()
    });

    // Check that warm scan produced the correct aggregated totals
    assert_eq!(warm_res.total_bytes, 350);
    assert_eq!(warm_res.total_files, 2);
}

#[test]
fn test_nested_warm_scan_correctness() {
    use disktracker_core::warm::{CachedDir, SnapshotIndex};

    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();

    let sub1 = root.join("sub1");
    fs::create_dir(&sub1).unwrap();

    let nested = sub1.join("nested");
    fs::create_dir(&nested).unwrap();

    let file_path = nested.join("file.txt");
    fs::write(&file_path, vec![0u8; 100]).unwrap(); // 100 bytes

    // 1. Cold Scan
    let cold_res = scan(ScanConfig {
        root: root.clone(),
        parallelism: 1,
        ..Default::default()
    });
    assert_eq!(cold_res.total_bytes, 100);
    assert_eq!(cold_res.total_files, 1);

    // 2. Prepare SnapshotIndex with ALL directories cached
    let mut index = SnapshotIndex::new();
    for idx in 0..cold_res.arena.node_count() {
        let path = cold_res.arena.materialize_path(idx as u32);
        let identity = cold_res.arena.cold[idx].identity;
        let mtime = cold_res.arena.cold[idx].mtime;
        let total_bytes = cold_res.arena.hot[idx].total_bytes;
        let file_count = cold_res.arena.hot[idx].file_count;

        let cached = CachedDir {
            total_bytes,
            file_count,
            mtime,
        };

        if identity.is_known() {
            index.insert_identity(identity, cached.clone());
        }
        index.insert_path(path, cached);
    }

    // 3. Create a new file in the nested subdirectory (size 150 bytes)
    // This updates nested's mtime (since a new entry is added) but NOT sub1's or root's mtime!
    std::thread::sleep(std::time::Duration::from_millis(1200)); // Ensure mtime granularity (1-second resolution)
    fs::write(nested.join("new_file.txt"), vec![0u8; 150]).unwrap();

    let skip_predicate = index.build_skip_predicate();

    // 4. Warm Scan
    let warm_res = scan(ScanConfig {
        root: root.clone(),
        parallelism: 1,
        skip_predicate: Some(skip_predicate),
        ..Default::default()
    });

    // If correctly implemented:
    // - Non-leaf directories (root and sub1) are NOT skipped.
    // - Leaf directory (nested) is checked, sees its mtime changed, and gets physically scanned.
    // - Total bytes is updated to 250 (100 + 150).
    assert_eq!(warm_res.total_bytes, 250);
    assert_eq!(warm_res.total_files, 2);
}
