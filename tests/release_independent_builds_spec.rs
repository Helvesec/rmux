#[test]
fn independent_build_comparison_contract() {
    let output = std::process::Command::new(if cfg!(windows) {
        "python.exe"
    } else {
        "python3"
    })
    .arg("tests/release_independent_builds_spec.py")
    .current_dir(env!("CARGO_MANIFEST_DIR"))
    .output()
    .expect("run independent build comparison contract");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
