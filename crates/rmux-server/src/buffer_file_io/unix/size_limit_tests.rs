//! Regression coverage for R3319 H2: `load-buffer` must refuse an endless or
//! oversized source with an error instead of growing the daemon until an
//! allocation failure aborts it.
//!
//! `/dev/zero` drives the special-file loop that FIFOs share; the macOS FIFO
//! helper's drain has its own test in `darwin_fifo_reader`.

use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::{read, MAX_BUFFER_FILE_BYTES};

static TEST_ID: AtomicU64 = AtomicU64::new(0);

fn test_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "rmux-buffer-limit-{label}-{}-{}",
        std::process::id(),
        TEST_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Creates a sparse file: no disk blocks are written for its length.
fn sparse_file(label: &str, length: usize) -> PathBuf {
    let path = test_path(label);
    File::create(&path)
        .and_then(|file| file.set_len(length as u64))
        .expect("create sparse test file");
    path
}

async fn read_within_deadline(path: PathBuf) -> io::Result<Vec<u8>> {
    tokio::time::timeout(Duration::from_secs(30), read(path))
        .await
        .expect("a bounded buffer read must finish")
}

#[tokio::test]
async fn endless_character_device_is_refused_at_the_buffer_limit() {
    let error = read_within_deadline(PathBuf::from("/dev/zero"))
        .await
        .expect_err("/dev/zero must not load");
    assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
}

#[tokio::test]
async fn oversized_regular_file_is_refused() {
    let path = sparse_file("oversized", MAX_BUFFER_FILE_BYTES + 1);
    let result = read_within_deadline(path.clone()).await;
    std::fs::remove_file(&path).expect("remove oversized test file");

    let error = result.expect_err("a file past the limit must not load");
    assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
}

#[tokio::test]
async fn regular_file_at_the_buffer_limit_loads_whole() {
    let path = sparse_file("at-limit", MAX_BUFFER_FILE_BYTES);
    let result = read_within_deadline(path.clone()).await;
    std::fs::remove_file(&path).expect("remove at-limit test file");

    assert_eq!(
        result.expect("a file of exactly the limit loads").len(),
        MAX_BUFFER_FILE_BYTES
    );
}
