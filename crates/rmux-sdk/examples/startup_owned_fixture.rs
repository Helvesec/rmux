//! Finite daemon startup fixture for the SDK ownership regression.
use std::path::PathBuf;
use std::time::Duration;

fn main() {
    let mut args = std::env::args_os().skip(1);
    assert_eq!(
        args.next().as_deref(),
        Some(std::ffi::OsStr::new("--__internal-daemon"))
    );
    let endpoint = PathBuf::from(args.next().expect("private endpoint"));
    std::fs::write(
        endpoint.with_extension("pid"),
        std::process::id().to_string(),
    )
    .expect("record launched child");
    // Never bind: the caller must time out while this exact child is alive.
    // A broken implementation is still allowed to finish naturally; the test
    // records its failure before reaping it, without an external kill.
    std::thread::sleep(Duration::from_secs(3));
}
