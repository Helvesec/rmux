#[path = "support/python3.rs"]
mod python3;

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{SystemTime, UNIX_EPOCH};

const SOURCE: &str = "1111111111111111111111111111111111111111";
const CONTROL: &str = "2222222222222222222222222222222222222222";
const FAILED_RUN: u64 = 30_926_195_244;
const CURRENT_RUN: u64 = 30_930_000_001;
const RELEASE_ID: u64 = 364_986_297;
const RECEIPT: &str = include_str!("../.github/workflows/release-receipt.yml");

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rmux-receipt-recovery-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&root).expect("create fixture root");
    root
}

fn write(root: &Path, name: &str, value: &Value) -> PathBuf {
    let path = root.join(name);
    fs::write(
        &path,
        serde_json::to_vec_pretty(value).expect("serialize fixture"),
    )
    .expect("write fixture");
    path
}

fn repository() -> Value {
    json!({"id": 1_239_918_790})
}

fn run_recovery(root: &Path, failed_conclusion: &str, existing: Value) -> Output {
    let failed = write(
        root,
        "failed.json",
        &json!({
            "id": FAILED_RUN,
            "workflow_id": 316_435_347,
            "path": ".github/workflows/release-receipt.yml",
            "event": "workflow_dispatch",
            "run_attempt": 1,
            "head_sha": SOURCE,
            "head_branch": "v0.10.0",
            "status": "completed",
            "conclusion": failed_conclusion,
            "repository": repository(),
            "head_repository": repository(),
        }),
    );
    let current = write(
        root,
        "current.json",
        &json!({
            "id": CURRENT_RUN,
            "workflow_id": 316_435_347,
            "path": ".github/workflows/release-receipt.yml",
            "event": "workflow_dispatch",
            "run_attempt": 1,
            "head_sha": CONTROL,
            "head_branch": "main",
            "status": "in_progress",
            "conclusion": null,
            "repository": repository(),
            "head_repository": repository(),
        }),
    );
    let empty_jobs = write(root, "jobs.json", &json!({"total_count": 0, "jobs": []}));
    let empty_artifacts = write(
        root,
        "artifacts.json",
        &json!({"total_count": 0, "artifacts": []}),
    );
    let main_ref = write(
        root,
        "main-ref.json",
        &json!({"ref": "refs/heads/main", "object": {"type": "commit", "sha": CONTROL}}),
    );
    let commit = write(
        root,
        "commit.json",
        &json!({"sha": CONTROL, "commit": {"verification": {"verified": true, "reason": "valid"}}}),
    );
    let ci = write(
        root,
        "ci.json",
        &json!({"workflow_runs": [{
            "workflow_id": 277_622_540,
            "head_sha": CONTROL,
            "head_branch": "main",
            "event": "push",
            "run_attempt": 1,
            "status": "completed",
            "conclusion": "success",
            "repository": repository(),
        }]}),
    );
    let existing = write(root, "existing.json", &existing);

    python3::command()
        .arg(repo_root().join("scripts/release/receipt-recovery.py"))
        .args([
            "--failed-run-id",
            &FAILED_RUN.to_string(),
            "--current-run-id",
            &CURRENT_RUN.to_string(),
            "--control-sha",
            CONTROL,
            "--source-sha",
            SOURCE,
            "--release-id",
            &RELEASE_ID.to_string(),
            "--release-ref",
            "v0.10.0",
            "--failed-run-json",
        ])
        .arg(failed)
        .arg("--failed-jobs-json")
        .arg(empty_jobs)
        .arg("--failed-artifacts-json")
        .arg(empty_artifacts)
        .arg("--current-run-json")
        .arg(current)
        .arg("--main-ref-json")
        .arg(main_ref)
        .arg("--control-commit-json")
        .arg(commit)
        .arg("--ci-runs-json")
        .arg(ci)
        .arg("--existing-receipts-json")
        .arg(existing)
        .current_dir(repo_root())
        .output()
        .expect("run receipt recovery verifier")
}

#[test]
fn protected_main_recovery_accepts_only_one_empty_startup_failure() {
    let root = fixture_root("success");
    let output = run_recovery(
        &root,
        "startup_failure",
        json!({"total_count": 0, "artifacts": []}),
    );
    fs::remove_dir_all(&root).expect("remove fixture root");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn protected_main_recovery_rejects_job_failures_and_existing_receipts() {
    let root = fixture_root("wrong-conclusion");
    let output = run_recovery(&root, "failure", json!({"total_count": 0, "artifacts": []}));
    fs::remove_dir_all(&root).expect("remove fixture root");
    assert!(!output.status.success());

    let root = fixture_root("existing-receipt");
    let output = run_recovery(
        &root,
        "startup_failure",
        json!({"total_count": 1, "artifacts": [{"expired": false}]}),
    );
    fs::remove_dir_all(&root).expect("remove fixture root");
    assert!(!output.status.success());
}

#[test]
fn receipt_recovery_is_explicit_and_normal_dispatch_remains_tag_bound() {
    assert!(RECEIPT.contains("failed_receipt_run_id:"));
    assert!(RECEIPT.contains("scripts/release/receipt-recovery.py"));
    assert!(RECEIPT.contains("test \"$GITHUB_REF\" = refs/heads/main"));
    assert!(RECEIPT.contains("test \"$GITHUB_REF\" = \"refs/tags/$RMUX_RELEASE_REF\""));
    assert!(RECEIPT.contains("--recovered-from-run-id \"$RMUX_FAILED_RECEIPT_RUN_ID\""));
}
