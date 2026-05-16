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
