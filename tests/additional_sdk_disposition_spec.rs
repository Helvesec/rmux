//! Portable policy checks are also part of the normal Cargo integration-test entrypoint.
use std::process::Command;

#[test]
fn additional_sdk_distribution_policy() {
    let python = if cfg!(windows) {
        "python.exe"
    } else {
        "python3"
    };
    let output = Command::new(python)
        .arg("tests/additional_sdk_disposition.py")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run portable SDK distribution policy regressions");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
