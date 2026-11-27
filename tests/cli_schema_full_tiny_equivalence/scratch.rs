#[cfg(not(target_has_atomic = "64"))]
use portable_atomic::AtomicU64;
use std::path::PathBuf;
#[cfg(target_has_atomic = "64")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

static NEXT_INVOCATION: AtomicU64 = AtomicU64::new(0);

pub fn create_root() -> PathBuf {
    let parent =
        std::env::var_os("RMUX_TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    std::fs::create_dir_all(&parent).unwrap();
    let invocation = NEXT_INVOCATION.fetch_add(1, Ordering::Relaxed);
    let root = parent.join(format!("schema-{}-{invocation}", std::process::id()));
    // Do not adopt a directory left by an earlier process with a reused PID.
    std::fs::create_dir(&root).unwrap();
    root
}

#[test]
fn overlapping_schema_invocations_keep_independent_scratch_directories() {
    // Reproduce the lifetime overlap of the schema and diagnose tests without
    // relying on thread scheduling: both allocate before either cleans up.
    let first = create_root();
    let second = create_root();
    std::fs::remove_dir_all(&first).unwrap();
    std::fs::remove_dir_all(&second)
        .unwrap_or_else(|error| panic!("second invocation lost {}: {error}", second.display()));
    assert_ne!(first, second, "each CLI invocation must own its directory");
}
