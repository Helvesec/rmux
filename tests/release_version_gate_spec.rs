#[cfg(target_os = "macos")]
#[test]
fn release_version_gate_runs_with_macos_system_python() {
    use std::env;
    use std::process::Command;

    let path = env::var_os("PATH").expect("PATH");
    let mut forced_path = env::split_paths(&path).collect::<Vec<_>>();
    forced_path.retain(|entry| entry != std::path::Path::new("/usr/bin"));
    forced_path.insert(0, "/usr/bin".into());

    let output = Command::new("bash")
        .arg("scripts/check-release-versions.sh")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env(
            "PATH",
            env::join_paths(forced_path).expect("join PATH with /usr/bin first"),
        )
        .output()
        .expect("run release version gate");

    assert!(
        output.status.success(),
        "release version gate failed with macOS system Python:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(output.stdout.ends_with(b"release-version-check=ok\n"));
}
