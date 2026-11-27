#[test]
fn publication_rehearsal_is_ordered_isolated_and_dry_run_only() {
    let python = if cfg!(windows) {
        "python.exe"
    } else {
        "python3"
    };
    let output = std::process::Command::new(python)
        .args([
            "-m",
            "unittest",
            "discover",
            "-s",
            "scripts/release",
            "-p",
            "test_publication*.py",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run publication rehearsal acceptance test");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
