#[path = "support/python3.rs"]
mod python3;

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{SystemTime, UNIX_EPOCH};

const SOURCE: &str = "1111111111111111111111111111111111111111";
const CONTROL: &str = "2222222222222222222222222222222222222222";
const FAILED_CONTROL: &str = "3333333333333333333333333333333333333333";
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

fn run_recovery(
    root: &Path,
    failed: Value,
    jobs: Value,
    artifacts: Value,
    existing: Value,
) -> Output {
    let failed = write(root, "failed.json", &failed);
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
    let jobs = write(root, "jobs.json", &jobs);
    let artifacts = write(root, "artifacts.json", &artifacts);
    let failed_commit = write(
        root,
        "failed-commit.json",
        &json!({"sha": FAILED_CONTROL, "commit": {"verification": {"verified": true, "reason": "valid"}}}),
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
        .arg(jobs)
        .arg("--failed-artifacts-json")
        .arg(artifacts)
        .arg("--failed-control-commit-json")
        .arg(failed_commit)
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

fn failed_run(head_sha: &str, head_branch: &str, conclusion: &str) -> Value {
    json!({
        "id": FAILED_RUN,
        "workflow_id": 316_435_347,
        "path": ".github/workflows/release-receipt.yml",
        "event": "workflow_dispatch",
        "run_attempt": 1,
        "head_sha": head_sha,
        "head_branch": head_branch,
        "status": "completed",
        "conclusion": conclusion,
        "repository": repository(),
        "head_repository": repository(),
    })
}

fn startup_recovery(root: &Path, conclusion: &str, existing: Value) -> Output {
    run_recovery(
        root,
        failed_run(SOURCE, "v0.10.0", conclusion),
        json!({"total_count": 0, "jobs": []}),
        json!({"total_count": 0, "artifacts": []}),
        existing,
    )
}

fn step(name: &str, conclusion: &str) -> Value {
    json!({"name": name, "status": "completed", "conclusion": conclusion})
}

fn downstream_failure_jobs() -> Value {
    let successes = [
        "Verify immutable Release and create receipt",
        "Audit live downstream repository authority",
        "Receipt-gated downstream publication / Prepare non-authoritative downstream plan",
        "Receipt-gated downstream publication / Verify exact downstream repository authority audit",
        "Receipt-gated downstream publication / Prepare exact downstream payloads / Materialize exact channel payloads",
    ];
    let owned = [
        "Receipt-gated downstream publication / Publish exact Homebrew tap formula / Publish owned channel homebrew_tap",
        "Receipt-gated downstream publication / Publish exact Scoop manifest / Publish owned channel scoop",
        "Receipt-gated downstream publication / Publish exact Web Share WASM bytes / Publish owned channel web_share",
    ];
    let skipped = [
        "Receipt-gated downstream publication / Build exact signed Linux repository trees / Publish only this run's authorized recovery artifact",
        "Receipt-gated downstream publication / Publish exact signed APT and RPM repositories",
        "Receipt-gated downstream publication / Publish exact crates.io package set",
        "Receipt-gated downstream publication / Record denied RC Linux repository channel",
        "Receipt-gated downstream publication / Submit exact Chocolatey package",
        "Receipt-gated downstream publication / Record disabled Snap stable channel",
        "Receipt-gated downstream publication / Record manual Homebrew Core submission",
        "Receipt-gated downstream publication / Record manual WinGet submission",
        "Receipt-gated downstream publication / Publish exact Snap candidate revisions",
        "Receipt-gated downstream publication / Aggregate ten exact pre-site results",
        "Receipt-gated downstream publication / Record blocked automated rmux.io update",
        "Receipt-gated downstream publication / Prepare manual rmux.io handoff",
        "Receipt-gated downstream publication / Aggregate all eleven exact channel results",
    ];
    let mut jobs = Vec::new();
    for name in successes {
        jobs.push(json!({"name": name, "conclusion": "success", "steps": []}));
    }
    jobs.push(json!({
        "name": "Receipt-gated downstream publication / Build exact signed Linux repository trees / Sign retained APT and RPM repository trees",
        "conclusion": "failure",
        "steps": [
            step("Import distinct repository signing keys", "failure"),
            step("Authenticate retained history and generate signed repositories", "skipped"),
            step("Add static package host files and exact checksum inventory", "skipped"),
            step("Upload the exact signed repository tree", "skipped"),
        ],
    }));
    for name in owned {
        jobs.push(json!({
            "name": name,
            "conclusion": "failure",
            "steps": [
                step("Mint a repository-scoped downstream writer token", "failure"),
                step("Publish and reread exact repository bytes", "skipped"),
                step("Seal exact owned repository result evidence", "skipped"),
            ],
        }));
    }
    for name in skipped {
        jobs.push(json!({"name": name, "conclusion": "skipped", "steps": []}));
    }
    json!({"total_count": jobs.len(), "jobs": jobs})
}

fn downstream_failure_artifacts() -> (Value, u64) {
    let receipt_id = 81_u64;
    let mut names = vec![
        format!("rmux-publication-receipt-{SOURCE}-{RELEASE_ID}"),
        format!("rmux-publication-receipt-envelope-{SOURCE}-{RELEASE_ID}"),
        format!("rmux-downstream-authority-{SOURCE}-{FAILED_RUN}"),
        format!("rmux-downstream-plan-{SOURCE}-{RELEASE_ID}"),
    ];
    for channel in [
        "apt_rpm",
        "chocolatey",
        "crates_io",
        "homebrew_core",
        "homebrew_tap",
        "scoop",
        "snap_candidate",
        "snap_stable",
        "web_share",
        "winget",
    ] {
        names.push(format!(
            "rmux-downstream-{channel}-payload-{SOURCE}-{RELEASE_ID}"
        ));
    }
    let artifacts: Vec<Value> = names
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            json!({
                "id": receipt_id + index as u64,
                "name": name,
                "expired": false,
                "digest": format!("sha256:{}", "a".repeat(64)),
                "workflow_run": {
                    "id": FAILED_RUN,
                    "head_sha": FAILED_CONTROL,
                    "head_branch": "main",
                    "repository_id": 1_239_918_790,
                    "head_repository_id": 1_239_918_790,
                },
            })
        })
        .collect();
    (
        json!({"total_count": artifacts.len(), "artifacts": artifacts}),
        receipt_id,
    )
}

#[test]
fn protected_main_recovery_accepts_only_one_empty_startup_failure() {
    let root = fixture_root("success");
    let output = startup_recovery(
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
    let output = startup_recovery(&root, "failure", json!({"total_count": 0, "artifacts": []}));
    fs::remove_dir_all(&root).expect("remove fixture root");
    assert!(!output.status.success());

    let root = fixture_root("existing-receipt");
    let output = startup_recovery(
        &root,
        "startup_failure",
        json!({"total_count": 1, "artifacts": [{"expired": false}]}),
    );
    fs::remove_dir_all(&root).expect("remove fixture root");
    assert!(!output.status.success());
}

#[test]
fn protected_main_recovery_accepts_only_the_exact_pre_mutation_failure() {
    let root = fixture_root("pre-mutation");
    let jobs = downstream_failure_jobs();
    let (artifacts, receipt_id) = downstream_failure_artifacts();
    let output = run_recovery(
        &root,
        failed_run(FAILED_CONTROL, "main", "failure"),
        jobs,
        artifacts,
        json!({
            "total_count": 1,
            "artifacts": [{
                "id": receipt_id,
                "name": format!("rmux-publication-receipt-{SOURCE}-{RELEASE_ID}"),
                "expired": false,
                "workflow_run": {"id": FAILED_RUN},
            }],
        }),
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
fn protected_main_recovery_rejects_any_owned_writer_mutation() {
    let root = fixture_root("writer-mutation");
    let mut jobs = downstream_failure_jobs();
    let entries = jobs["jobs"].as_array_mut().expect("jobs array");
    let owned = entries
        .iter_mut()
        .find(|job| {
            job["name"]
                .as_str()
                .is_some_and(|name| name.contains("Publish exact Scoop manifest"))
        })
        .expect("owned writer job");
    let steps = owned["steps"].as_array_mut().expect("step array");
    let writer = steps
        .iter_mut()
        .find(|step| step["name"] == "Publish and reread exact repository bytes")
        .expect("writer step");
    writer["conclusion"] = json!("success");
    let (artifacts, receipt_id) = downstream_failure_artifacts();
    let output = run_recovery(
        &root,
        failed_run(FAILED_CONTROL, "main", "failure"),
        jobs,
        artifacts,
        json!({
            "total_count": 1,
            "artifacts": [{
                "id": receipt_id,
                "name": format!("rmux-publication-receipt-{SOURCE}-{RELEASE_ID}"),
                "expired": false,
                "workflow_run": {"id": FAILED_RUN},
            }],
        }),
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
