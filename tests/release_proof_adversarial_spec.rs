#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rmux-release-adversarial-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create fixture directory");
    path
}

fn run(program: &Path, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|error| panic!("failed to run {}: {error}", program.display()))
}

fn git(root: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git fixture command")
}

fn commit(root: &Path, message: &str) -> String {
    let staged = git(root, &["add", "."]);
    assert!(staged.status.success());
    let committed = git(root, &["commit", "-q", "-m", message]);
    assert!(committed.status.success());
    let sha = git(root, &["rev-parse", "HEAD"]);
    assert!(sha.status.success());
    String::from_utf8(sha.stdout)
        .expect("UTF-8 fixture SHA")
        .trim()
        .to_owned()
}

fn policy_root(root: &Path, sha: &str) -> serde_json::Value {
    let output = run(
        &repo_root().join("scripts/release/policy-root.py"),
        &[
            "--repository-root",
            root.to_str().expect("fixture root"),
            "--source-sha",
            sha,
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse policy root")
}

#[test]
fn policy_root_binds_the_committed_contract_and_git_mode() {
    let root = temp_dir("policy-mode");
    fs::create_dir_all(root.join(".github/release")).expect("create contract directory");
    fs::write(root.join("a.txt"), "same bytes\n").expect("write policy file");
    fs::write(
        root.join(".github/release/candidate-contract.json"),
        r#"{"policy_paths":[".github/release/candidate-contract.json","a.txt"]}"#,
    )
    .expect("write policy contract");
    assert!(git(&root, &["init", "-q"]).status.success());
    assert!(git(&root, &["config", "user.name", "RMUX test"])
        .status
        .success());
    assert!(git(
        &root,
        &["config", "user.email", "rmux-test@example.invalid"]
    )
    .status
    .success());
    let first_sha = commit(&root, "regular mode");
    let first = policy_root(&root, &first_sha);

    let mut permissions = fs::metadata(root.join("a.txt"))
        .expect("policy metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(root.join("a.txt"), permissions).expect("make policy executable");
    let second_sha = commit(&root, "executable mode");
    let second = policy_root(&root, &second_sha);

    assert_eq!(first["algorithm"], "sha256-length-delimited-v2");
    assert_eq!(first["records"][1]["mode"], "100644");
    assert_eq!(second["records"][1]["mode"], "100755");
    assert_ne!(
        first["release_policy_sha256"],
        second["release_policy_sha256"]
    );
    fs::remove_dir_all(root).expect("remove policy fixture");
}

#[test]
fn exact_run_verifier_rejects_backdating_outside_fixtures() {
    let verifier = repo_root().join("scripts/release/verify-fast-run.py");
    let source = fs::read_to_string(&verifier).expect("read verifier");
    assert!(source.contains("load_committed_contract"));
    assert!(!source.contains("parser.add_argument(\"--contract\""));
    let output = run(
        &verifier,
        &[
            "--repository",
            "Helvesec/rmux",
            "--run-id",
            "1",
            "--expected-source-sha",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--kind",
            "fast",
            "--now",
            "2020-01-01T00:00:00Z",
        ],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("--now is restricted to complete offline test fixtures"));
}

fn timing_run(attempt: u64) -> serde_json::Value {
    serde_json::json!({
        "id": 77,
        "workflow_id": 277622540,
        "path": ".github/workflows/ci.yml",
        "name": "CI",
        "event": "push",
        "head_branch": "main",
        "head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "run_attempt": attempt,
        "status": "completed",
        "conclusion": "success",
        "run_started_at": "2026-07-19T09:00:00Z",
        "updated_at": format!("2026-07-19T09:0{attempt}:00Z")
    })
}

fn timing_args<'a>(run_path: &'a Path, jobs_path: &'a Path) -> Vec<&'a str> {
    vec![
        "--repository",
        "Helvesec/rmux",
        "--run-id",
        "77",
        "--attempt",
        "1",
        "--run-json",
        run_path.to_str().expect("run fixture path"),
        "--jobs-json",
        jobs_path.to_str().expect("jobs fixture path"),
    ]
}

#[test]
fn timing_collector_rejects_fractional_inversion_and_rerun_race() {
    let root = temp_dir("timing-race");
    let run_path = root.join("run.json");
    let run_after_path = root.join("run-after.json");
    let jobs_path = root.join("jobs.json");
    fs::write(&run_path, timing_run(1).to_string()).expect("write run fixture");
    fs::write(
        &jobs_path,
        serde_json::json!({"total_count": 1, "jobs": [{
            "id": 1,
            "run_id": 77,
            "run_attempt": 1,
            "head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "name": "Fractional inversion",
            "status": "completed",
            "conclusion": "success",
            "created_at": "2026-07-19T09:00:00.900Z",
            "started_at": "2026-07-19T09:00:00.100Z",
            "completed_at": "2026-07-19T09:00:01.000Z",
            "labels": ["ubuntu-latest"],
            "steps": []
        }]})
        .to_string(),
    )
    .expect("write jobs fixture");
    let collector = repo_root().join("scripts/release/measure-actions-run.py");
    let inverted = run(&collector, &timing_args(&run_path, &jobs_path));
    assert!(!inverted.status.success());
    assert!(String::from_utf8_lossy(&inverted.stderr).contains("timestamps are not ordered"));

    fs::write(&run_after_path, timing_run(2).to_string()).expect("write changed run");
    let mut race_args = timing_args(&run_path, &jobs_path);
    race_args.extend([
        "--run-after-json",
        run_after_path.to_str().expect("run-after fixture path"),
    ]);
    let raced = run(&collector, &race_args);
    assert!(!raced.status.success());
    assert!(
        String::from_utf8_lossy(&raced.stderr).contains("run changed while jobs were paginated")
    );
    fs::remove_dir_all(root).expect("remove timing fixture");
}
