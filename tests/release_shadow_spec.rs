#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Command, Output};
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[cfg(unix)]
fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rmux-release-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create fixture directory");
    path
}

#[cfg(unix)]
fn run(program: &Path, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|error| panic!("failed to run {}: {error}", program.display()))
}

#[test]
fn release_shadow_has_no_write_authority_or_mutation_primitive() {
    let workflow = include_str!("../.github/workflows/release-shadow.yml");
    assert!(workflow.contains("on:\n  workflow_dispatch:"));
    assert_eq!(workflow.matches("permissions: {}").count(), 1);
    assert_eq!(workflow.matches("permissions:").count(), 1);
    assert!(!workflow.contains("uses:"));
    assert!(workflow.contains("method=\"GET\""));
    assert!(workflow.contains("https://api.github.com"));
    assert!(workflow.contains("https://raw.githubusercontent.com"));
    assert!(workflow.contains("refs/heads/main"));
    assert!(workflow.contains("/attempts/1/jobs"));

    for trigger in [
        "\n  push:",
        "\n  pull_request:",
        "\n  pull_request_target:",
        "\n  workflow_run:",
        "\n  repository_dispatch:",
        "\n  release:",
        "\n  schedule:",
        "\n  workflow_call:",
    ] {
        assert!(
            !workflow.contains(trigger),
            "shadow gained trigger {trigger}"
        );
    }
    for authority in [
        "github.token",
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "secrets.",
        "environment:",
        "id-token:",
        "contents: write",
        "actions: write",
        "attestations: write",
    ] {
        assert!(
            !workflow.contains(authority),
            "shadow gained authority marker {authority}"
        );
    }
    for mutation in [
        "method=\"POST\"",
        "method=\"PUT\"",
        "method=\"PATCH\"",
        "method=\"DELETE\"",
        "upload-artifact",
        "actions/cache",
        "actions/attest",
        "github-script",
        "create-github-app-token",
        "gh release",
        "gh workflow run",
        "git push",
        "git tag",
        "cargo publish",
        "choco push",
        "snapcraft upload",
        "snapcraft release",
        "--data",
        "--form",
        "GITHUB_OUTPUT",
        "GITHUB_ENV",
        "GITHUB_STEP_SUMMARY",
        "continue-on-error",
        "cancel-in-progress: true",
    ] {
        assert!(
            !workflow.contains(mutation),
            "shadow gained mutation primitive {mutation}"
        );
    }
}

#[test]
#[cfg(unix)]
fn release_contracts_validate_offline() {
    let output = run(
        &repo_root().join("scripts/release/validate-contracts.py"),
        &[],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "release-contracts-ok"
    );
}

#[test]
#[cfg(unix)]
fn exact_run_verifier_accepts_only_the_contracted_job_set() {
    let root = temp_dir("exact-run");
    let contract: serde_json::Value =
        serde_json::from_str(include_str!("../.github/release/candidate-contract.json"))
            .expect("parse candidate contract");
    let fast = contract.get("fast_run").expect("fast run contract");
    let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let run_id = 42_u64;
    let repository = serde_json::json!({
        "id": 1239918790,
        "full_name": "Helvesec/rmux",
        "default_branch": "main",
        "visibility": "public"
    });
    let run_json = serde_json::json!({
        "id": run_id,
        "workflow_id": 277622540,
        "path": ".github/workflows/ci.yml",
        "event": "push",
        "head_branch": "main",
        "head_sha": sha,
        "run_attempt": 1,
        "status": "completed",
        "conclusion": "success",
        "run_started_at": "2026-07-19T09:00:00Z",
        "updated_at": "2026-07-19T09:10:00Z",
        "repository": {"id": 1239918790, "full_name": "Helvesec/rmux"},
        "head_repository": {"id": 1239918790, "full_name": "Helvesec/rmux"}
    });
    let mut jobs = Vec::new();
    let mut next_id = 1_u64;
    for (field, conclusion) in [("success_jobs", "success"), ("skipped_jobs", "skipped")] {
        for name in fast[field].as_array().expect("job list") {
            jobs.push(job_fixture(
                next_id,
                run_id,
                sha,
                name.as_str().expect("job name"),
                conclusion,
            ));
            next_id += 1;
        }
    }
    for (name, conclusions) in fast["allowed_jobs"].as_object().expect("allowed jobs") {
        jobs.push(job_fixture(
            next_id,
            run_id,
            sha,
            name,
            conclusions[0].as_str().expect("allowed conclusion"),
        ));
        next_id += 1;
    }

    let repository_path = root.join("repository.json");
    let run_path = root.join("run.json");
    let jobs_path = root.join("jobs.json");
    fs::write(&repository_path, repository.to_string()).expect("write repository fixture");
    fs::write(&run_path, run_json.to_string()).expect("write run fixture");
    fs::write(
        &jobs_path,
        serde_json::json!({"total_count": jobs.len(), "jobs": jobs}).to_string(),
    )
    .expect("write jobs fixture");

    let verifier = repo_root().join("scripts/release/verify-fast-run.py");
    let common = vec![
        "--repository",
        "Helvesec/rmux",
        "--run-id",
        "42",
        "--expected-source-sha",
        sha,
        "--kind",
        "fast",
        "--now",
        "2026-07-19T10:00:00Z",
        "--repository-json",
        repository_path.to_str().expect("repository fixture path"),
        "--run-json",
        run_path.to_str().expect("run fixture path"),
        "--jobs-json",
        jobs_path.to_str().expect("jobs fixture path"),
    ];
    let accepted = run(&verifier, &common);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let proof: serde_json::Value =
        serde_json::from_slice(&accepted.stdout).expect("parse exact-run proof");
    assert_eq!(proof["run_id"], 42);
    assert_eq!(proof["run_attempt"], 1);

    let mut corrupted: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&jobs_path).expect("read jobs fixture"))
            .expect("parse jobs fixture");
    corrupted["jobs"].as_array_mut().expect("jobs array").pop();
    fs::write(&jobs_path, corrupted.to_string()).expect("write corrupt jobs fixture");
    let rejected = run(&verifier, &common);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("job set mismatch"));

    fs::remove_dir_all(root).expect("remove exact-run fixtures");
}

#[cfg(unix)]
fn job_fixture(id: u64, run_id: u64, sha: &str, name: &str, conclusion: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "run_id": run_id,
        "run_attempt": 1,
        "head_sha": sha,
        "workflow_name": "CI",
        "name": name,
        "status": "completed",
        "conclusion": conclusion,
        "created_at": "2026-07-19T09:00:00Z",
        "started_at": "2026-07-19T09:00:01Z",
        "completed_at": "2026-07-19T09:00:02Z"
    })
}

#[test]
#[cfg(unix)]
fn timing_collector_separates_required_and_all_job_end_times() {
    let root = temp_dir("timing");
    let run_path = root.join("run.json");
    let jobs_path = root.join("jobs.json");
    fs::write(
        &run_path,
        serde_json::json!({
            "id": 77,
            "workflow_id": 277622540,
            "path": ".github/workflows/ci.yml",
            "event": "push",
            "head_branch": "main",
            "head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "run_attempt": 1,
            "status": "completed",
            "conclusion": "success",
            "run_started_at": "2026-07-19T09:00:00Z"
        })
        .to_string(),
    )
    .expect("write timing run fixture");
    fs::write(
        &jobs_path,
        serde_json::json!({"total_count": 2, "jobs": [
            timing_job(1, "Required gate", "2026-07-19T09:02:00Z", "ubuntu-latest"),
            timing_job(2, "Auxiliary save", "2026-07-19T09:03:00Z", "macos-15")
        ]})
        .to_string(),
    )
    .expect("write timing jobs fixture");
    let output = run(
        &repo_root().join("scripts/release/measure-actions-run.py"),
        &[
            "--repository",
            "Helvesec/rmux",
            "--run-id",
            "77",
            "--attempt",
            "1",
            "--expected-sha",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "--expected-event",
            "push",
            "--expected-branch",
            "main",
            "--expected-workflow-id",
            "277622540",
            "--expected-workflow-path",
            ".github/workflows/ci.yml",
            "--expected-conclusion",
            "success",
            "--required-check",
            "Required gate",
            "--run-json",
            run_path.to_str().expect("run fixture path"),
            "--jobs-json",
            jobs_path.to_str().expect("jobs fixture path"),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse timing report");
    assert_eq!(report["summary"]["wallclock_sec"], 120);
    assert_eq!(report["summary"]["all_jobs_wallclock_sec"], 180);
    assert_eq!(report["summary"]["max_queued_jobs"], 2);
    assert_eq!(report["summary"]["max_macos_running_jobs"], 1);
    assert_eq!(report["summary"]["max_macos_queued_jobs"], 1);
    fs::remove_dir_all(root).expect("remove timing fixtures");
}

#[cfg(unix)]
fn timing_job(id: u64, name: &str, completed_at: &str, label: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "name": name,
        "status": "completed",
        "conclusion": "success",
        "created_at": "2026-07-19T09:00:00Z",
        "started_at": "2026-07-19T09:00:10Z",
        "completed_at": completed_at,
        "labels": [label],
        "steps": []
    })
}

#[test]
#[cfg(unix)]
fn candidate_controller_is_dry_run_by_default_and_requests_the_returned_run_id() {
    let controller = repo_root().join("scripts/release/dispatch-shadow-candidate.sh");
    let source = fs::read_to_string(&controller).expect("read candidate controller");
    assert!(source.contains("X-GitHub-Api-Version: 2026-03-10"));
    assert!(source.contains("return_run_details"));
    assert!(source.contains("workflow_run_id"));
    assert!(!source.contains("gh run list"));
    assert!(!source.contains("git tag"));
    assert!(!source.contains("git push"));

    let output = run(
        &controller,
        &[
            "--repository",
            "Helvesec/rmux",
            "--expected-source-sha",
            "cccccccccccccccccccccccccccccccccccccccc",
            "--fast-run-id",
            "123",
            "--release-intent-id",
            "intent-1234",
            "--planned-release-ref",
            "v1.0.0-rc.1",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse dry-run request");
    assert_eq!(request["mode"], "dry-run");
    assert_eq!(request["api_version"], "2026-03-10");
    assert_eq!(request["payload"]["return_run_details"], true);
    assert_eq!(request["payload"]["inputs"]["release_qualification"], true);
}

#[test]
fn release_verification_cli_is_pinned_and_capability_checked() {
    let installer = include_str!("../scripts/release/install-gh-2.93.0.sh");
    assert!(installer.contains("version=2.93.0"));
    assert!(installer.contains("02d1290eba130e0b896f3709ffff22e1c75a51475ddb70476a85abc6b5807af0"));
    assert!(installer.contains("release verify --help"));
    assert!(installer.contains("release verify-asset --help"));
}

#[test]
fn release_policy_surfaces_have_explicit_code_owners() {
    let owners = include_str!("../.github/CODEOWNERS");
    for protected_path in [
        "/.github/workflows/",
        "/.github/release/",
        "/scripts/release/",
        "/scripts/verify-release-tag-protection.sh",
        "/tests/release_shadow_spec.rs",
    ] {
        assert!(
            owners.lines().any(|line| line.starts_with(protected_path)),
            "release policy path has no CODEOWNER: {protected_path}"
        );
    }
}

#[test]
#[cfg(unix)]
fn policy_root_reads_committed_blob_bytes_not_the_worktree() {
    let root = temp_dir("policy-root");
    fs::create_dir_all(root.join(".github/release")).expect("create contract directory");
    fs::write(root.join("a.txt"), "first\n").expect("write first policy blob");
    fs::write(
        root.join(".github/release/candidate-contract.json"),
        r#"{"policy_paths":["a.txt"]}"#,
    )
    .expect("write policy contract");
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "RMUX test"],
        vec!["config", "user.email", "rmux-test@example.invalid"],
        vec!["add", "a.txt", ".github/release/candidate-contract.json"],
        vec!["commit", "-q", "-m", "fixture one"],
    ] {
        let output = Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .expect("run fixture git command");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let first_sha_output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&root)
        .output()
        .expect("resolve first fixture SHA");
    let first_sha = String::from_utf8(first_sha_output.stdout)
        .expect("UTF-8 fixture SHA")
        .trim()
        .to_owned();
    let first = run_policy_root(&root, &first_sha);

    fs::write(root.join("a.txt"), "uncommitted drift\n").expect("write worktree drift");
    let same_commit = run_policy_root(&root, &first_sha);
    assert_eq!(
        first["release_policy_sha256"], same_commit["release_policy_sha256"],
        "uncommitted bytes must not influence a Git-rooted policy hash"
    );

    let add = Command::new("git")
        .args(["add", "a.txt"])
        .current_dir(&root)
        .output()
        .expect("stage second fixture");
    assert!(add.status.success());
    let commit = Command::new("git")
        .args(["commit", "-q", "-m", "fixture two"])
        .current_dir(&root)
        .output()
        .expect("commit second fixture");
    assert!(commit.status.success());
    let second_sha_output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&root)
        .output()
        .expect("resolve second fixture SHA");
    let second_sha = String::from_utf8(second_sha_output.stdout)
        .expect("UTF-8 fixture SHA")
        .trim()
        .to_owned();
    let second = run_policy_root(&root, &second_sha);
    assert_ne!(
        first["release_policy_sha256"],
        second["release_policy_sha256"]
    );
    fs::remove_dir_all(root).expect("remove policy-root fixtures");
}

#[cfg(unix)]
fn run_policy_root(repository_root: &Path, source_sha: &str) -> serde_json::Value {
    let contract_path = repository_root.join(".github/release/candidate-contract.json");
    let output = run(
        &repo_root().join("scripts/release/policy-root.py"),
        &[
            "--repository-root",
            repository_root.to_str().expect("fixture root path"),
            "--contract",
            contract_path.to_str().expect("fixture contract path"),
            "--source-sha",
            source_sha,
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse policy-root output")
}
