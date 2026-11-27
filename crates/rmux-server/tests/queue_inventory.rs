use std::path::Path;
use std::process::Command;

#[path = "queue_inventory/python.rs"]
mod python;

#[test]
fn queue_inventory_uses_selected_python_without_path_python3() {
    let probe = python::command()
        .args([
            "-c",
            "import shutil, sys; print(sys.executable); print(shutil.which('git') or '')",
        ])
        .output()
        .expect("locate the running Python fixture interpreter");
    assert!(probe.status.success());
    let paths = String::from_utf8(probe.stdout).unwrap();
    let mut paths = paths.lines();
    let python = paths.next().expect("Python path");
    let git = Path::new(
        paths
            .next()
            .filter(|path| !path.is_empty())
            .expect("Git path"),
    );
    assert!(Path::new(&python).is_absolute());
    let directory = std::env::temp_dir().join(format!("rmux-queue-python-{}", std::process::id()));
    std::fs::create_dir(&directory).expect("create isolated PATH");
    // Inventory needs Git as well as Python; isolate only Python resolution.
    #[cfg(unix)]
    std::os::unix::fs::symlink(git, directory.join("git")).expect("retain Git in isolated PATH");
    #[cfg(unix)]
    let search_path = directory.as_os_str().to_owned();
    #[cfg(windows)]
    let search_path = {
        let git_directory = git.parent().expect("Git directory");
        assert!(!git_directory.join("python3.exe").exists());
        assert!(!git_directory.join("python3").exists());
        std::env::join_paths([directory.as_path(), git_directory]).unwrap()
    };
    let mut failures = Vec::new();
    for inert_alias in [false, true] {
        if inert_alias {
            for name in ["python3", "python3.exe"] {
                std::fs::write(directory.join(name), b"inert alias\n").unwrap();
            }
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "queue_inventory_covers_current_sources_and_executable_gate_tests",
                "--nocapture",
            ])
            .env("PATH", &search_path)
            .env("RMUX_PYTHON", python)
            .output()
            .expect("run queue inventory with isolated PATH");
        if !output.status.success() {
            failures.push(format!(
                "inert_alias={inert_alias}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn queue_inventory_covers_current_sources_and_executable_gate_tests() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let acceptance = root.join("crates/rmux-server/tests/queue_inventory_acceptance.py");
    let output = python::command()
        .arg("-B")
        .arg(acceptance)
        .arg("--source")
        .arg(&root)
        .output()
        .expect("run the queue inventory acceptance test");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
