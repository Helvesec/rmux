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
const PRIOR_CONTROL: &str = "4444444444444444444444444444444444444444";
const FAILED_RUN: u64 = 30_926_195_244;
const PRIOR_RUN: u64 = 30_925_000_001;
const CURRENT_RUN: u64 = 30_930_000_001;
const RELEASE_ID: u64 = 364_986_297;
const RECEIPT: &str = include_str!("../.github/workflows/release-receipt.yml");
const RECEIPT_CREATE: &str = include_str!("../.github/actions/release-receipt-create/action.yml");

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
    run_recovery_with_prior(root, failed, jobs, artifacts, existing, None)
}

fn run_recovery_with_prior(
    root: &Path,
    failed: Value,
    jobs: Value,
    artifacts: Value,
    existing: Value,
    prior_attempts: Option<Value>,
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

    let mut command = python3::command();
    command
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
        .arg(existing);
    if let Some(prior_attempts) = prior_attempts {
        let prior_attempts = write(root, "prior-attempts.json", &prior_attempts);
        command.arg("--prior-attempts-json").arg(prior_attempts);
    }
    command
        .current_dir(repo_root())
        .output()
        .expect("run receipt recovery verifier")
}

fn failed_run(head_sha: &str, head_branch: &str, conclusion: &str) -> Value {
    failed_run_with_id(FAILED_RUN, head_sha, head_branch, conclusion)
}

fn failed_run_with_id(run_id: u64, head_sha: &str, head_branch: &str, conclusion: &str) -> Value {
    json!({
        "id": run_id,
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

fn direct_downstream_failure_jobs() -> Value {
    let mut value = downstream_failure_jobs();
    let jobs = value["jobs"].as_array_mut().expect("jobs array");
    for job in jobs.iter_mut() {
        let name = job["name"].as_str().expect("job name");
        let Some(short) = name.strip_prefix("Receipt-gated downstream publication / ") else {
            continue;
        };
        let direct = if [
            "Prepare non-authoritative downstream plan",
            "Verify exact downstream repository authority audit",
            "Prepare exact downstream payloads / Materialize exact channel payloads",
        ]
        .contains(&short)
        {
            format!("Prepare receipt-gated downstream publication / {short}")
        } else {
            short.to_owned()
        };
        job["name"] = json!(direct);
    }
    value
}

fn post_mutation_failure_jobs() -> Value {
    let mut value = direct_downstream_failure_jobs();
    let jobs = value["jobs"].as_array_mut().expect("jobs array");
    jobs.retain(|job| {
        job["name"]
            != "Build exact signed Linux repository trees / Publish only this run's authorized recovery artifact"
    });
    for job in jobs.iter_mut() {
        let name = job["name"].as_str().expect("job name");
        let replacement = match name {
            "Build exact signed Linux repository trees / Sign retained APT and RPM repository trees" => {
                Some(("Build exact signed Linux repository trees", "success", "Run ./.github/actions/release-linux-repository-build"))
            }
            "Publish exact Homebrew tap formula / Publish owned channel homebrew_tap" => {
                Some(("Publish exact Homebrew tap formula", "failure", "Run ./.github/actions/release-owned-repository-publish"))
            }
            "Publish exact Scoop manifest / Publish owned channel scoop" => {
                Some(("Publish exact Scoop manifest", "failure", "Run ./.github/actions/release-owned-repository-publish"))
            }
            "Publish exact Web Share WASM bytes / Publish owned channel web_share" => {
                Some(("Publish exact Web Share WASM bytes", "failure", "Run ./.github/actions/release-owned-repository-publish"))
            }
            _ => None,
        };
        if let Some((name, conclusion, action)) = replacement {
            job["name"] = json!(name);
            job["conclusion"] = json!(conclusion);
            let mut steps = vec![
                step("Set up job", "success"),
                step(
                    "Run actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5",
                    "success",
                ),
                step(action, conclusion),
            ];
            if conclusion == "failure" {
                steps.push(step(&format!("Post {action}"), "success"));
            }
            steps.extend([
                step(
                    "Post Run actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5",
                    "success",
                ),
                step("Complete job", "success"),
            ]);
            job["steps"] = json!(steps);
        }
    }
    value["total_count"] = json!(jobs.len());
    value
}

fn package_writer_failure_jobs() -> Value {
    let mut value = post_mutation_failure_jobs();
    let jobs = value["jobs"].as_array_mut().expect("jobs array");
    for job in jobs.iter_mut() {
        let name = job["name"].as_str().expect("job name");
        if [
            "Publish exact Homebrew tap formula",
            "Publish exact Scoop manifest",
            "Publish exact Web Share WASM bytes",
        ]
        .contains(&name)
        {
            job["conclusion"] = json!("success");
            for step in job["steps"].as_array_mut().expect("writer steps") {
                if step["name"] == "Run ./.github/actions/release-owned-repository-publish" {
                    step["conclusion"] = json!("success");
                }
            }
            continue;
        }
        if name == "Publish exact signed APT and RPM repositories" {
            let action = "Run ./.github/actions/release-linux-repository-publish";
            job["conclusion"] = json!("failure");
            job["steps"] = json!([
                step("Set up job", "success"),
                step(
                    "Run actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5",
                    "success",
                ),
                step(action, "failure"),
                step(&format!("Post {action}"), "success"),
                step(
                    "Post Run actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5",
                    "success",
                ),
                step("Complete job", "success"),
            ]);
            continue;
        }
        if name == "Publish exact crates.io package set" {
            job["name"] =
                json!("Publish exact crates.io package set / Publish exact crates.io package set");
            job["conclusion"] = json!("failure");
            job["steps"] = json!([
                step("Set up job", "success"),
                step(
                    "Run actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5",
                    "success",
                ),
                step("Run ./.github/actions/release-channel-prepare", "success"),
                step("Resolve exact crates.io execution authority", "success"),
                step(
                    "Exchange GitHub OIDC for a short-lived crates.io token",
                    "failure",
                ),
                step("Publish and redownload every exact crate", "skipped"),
                step("Normalize executable and policy-only outcomes", "skipped"),
                step("Seal exact crates.io result evidence", "skipped"),
                step(
                    "Post Exchange GitHub OIDC for a short-lived crates.io token",
                    "success",
                ),
                step(
                    "Post Run actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5",
                    "success",
                ),
                step("Complete job", "success"),
            ]);
        }
    }
    value
}

fn downstream_failure_artifacts() -> (Value, u64) {
    downstream_failure_artifacts_for(FAILED_RUN, FAILED_CONTROL, 81)
}

fn post_mutation_failure_artifacts() -> (Value, u64) {
    post_mutation_failure_artifacts_for(FAILED_RUN, FAILED_CONTROL, 81, true)
}

fn package_writer_failure_artifacts() -> (Value, u64) {
    let (mut value, receipt_id) = post_mutation_failure_artifacts();
    let artifacts = value["artifacts"].as_array_mut().expect("artifacts array");
    for channel in ["homebrew_tap", "scoop", "web_share"] {
        artifacts.push(json!({
            "id": receipt_id + artifacts.len() as u64,
            "name": format!(
                "rmux-downstream-{channel}-result-reference-{SOURCE}-{RELEASE_ID}"
            ),
            "expired": false,
            "digest": format!("sha256:{}", "a".repeat(64)),
            "workflow_run": {
                "id": FAILED_RUN,
                "head_sha": FAILED_CONTROL,
                "head_branch": "main",
                "repository_id": 1_239_918_790,
                "head_repository_id": 1_239_918_790,
            },
        }));
    }
    let total_count = artifacts.len();
    value["total_count"] = json!(total_count);
    (value, receipt_id)
}

fn post_mutation_failure_artifacts_for(
    run_id: u64,
    control_sha: &str,
    receipt_id: u64,
    include_envelopes: bool,
) -> (Value, u64) {
    let (mut value, receipt_id) = downstream_failure_artifacts_for(run_id, control_sha, receipt_id);
    let artifacts = value["artifacts"].as_array_mut().expect("artifacts array");
    let mut names = vec![format!(
        "rmux-downstream-apt_rpm-signed-{SOURCE}-{RELEASE_ID}"
    )];
    for channel in ["homebrew_tap", "scoop", "web_share"] {
        names.push(format!(
            "rmux-downstream-{channel}-result-{SOURCE}-{RELEASE_ID}"
        ));
        if include_envelopes {
            names.push(format!(
                "rmux-downstream-{channel}-result-envelope-{SOURCE}-{RELEASE_ID}"
            ));
        }
    }
    for name in names {
        artifacts.push(json!({
            "id": receipt_id + artifacts.len() as u64,
            "name": name,
            "expired": false,
            "digest": format!("sha256:{}", "a".repeat(64)),
            "workflow_run": {
                "id": run_id,
                "head_sha": control_sha,
                "head_branch": "main",
                "repository_id": 1_239_918_790,
                "head_repository_id": 1_239_918_790,
            },
        }));
    }
    value["total_count"] = json!(artifacts.len());
    (value, receipt_id)
}

fn downstream_failure_artifacts_for(
    run_id: u64,
    control_sha: &str,
    receipt_id: u64,
) -> (Value, u64) {
    let mut names = vec![
        format!("rmux-publication-receipt-{SOURCE}-{RELEASE_ID}"),
        format!("rmux-publication-receipt-envelope-{SOURCE}-{RELEASE_ID}"),
        format!("rmux-downstream-authority-{SOURCE}-{run_id}"),
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
                    "id": run_id,
                    "head_sha": control_sha,
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
fn protected_main_recovery_accepts_the_flattened_pre_mutation_topology() {
    let root = fixture_root("flattened-pre-mutation");
    let jobs = direct_downstream_failure_jobs();
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
fn protected_main_recovery_accepts_only_the_exact_post_mutation_seal_failure() {
    let root = fixture_root("post-mutation-seal");
    let jobs = post_mutation_failure_jobs();
    let (artifacts, receipt_id) = post_mutation_failure_artifacts();
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
fn protected_main_recovery_accepts_exact_package_writer_failures() {
    let root = fixture_root("package-writer-failures");
    let (artifacts, receipt_id) = package_writer_failure_artifacts();
    let output = run_recovery(
        &root,
        failed_run(FAILED_CONTROL, "main", "failure"),
        package_writer_failure_jobs(),
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
fn protected_main_recovery_rejects_partial_package_result_references() {
    let root = fixture_root("package-writer-partial-references");
    let (mut artifacts, receipt_id) = package_writer_failure_artifacts();
    let entries = artifacts["artifacts"]
        .as_array_mut()
        .expect("artifacts array");
    entries.retain(|artifact| {
        artifact["name"] != format!("rmux-downstream-scoop-result-reference-{SOURCE}-{RELEASE_ID}")
    });
    artifacts["total_count"] = json!(entries.len());
    let output = run_recovery(
        &root,
        failed_run(FAILED_CONTROL, "main", "failure"),
        package_writer_failure_jobs(),
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
fn protected_main_recovery_rejects_partial_post_mutation_result_envelopes() {
    let root = fixture_root("post-mutation-partial-envelopes");
    let (mut artifacts, receipt_id) = post_mutation_failure_artifacts();
    let entries = artifacts["artifacts"]
        .as_array_mut()
        .expect("artifacts array");
    entries.retain(|artifact| {
        artifact["name"] != format!("rmux-downstream-scoop-result-envelope-{SOURCE}-{RELEASE_ID}")
    });
    artifacts["total_count"] = json!(entries.len());
    let output = run_recovery(
        &root,
        failed_run(FAILED_CONTROL, "main", "failure"),
        post_mutation_failure_jobs(),
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
fn protected_main_recovery_accepts_complete_results_after_a_legacy_result_set() {
    let root = fixture_root("legacy-result-chain");
    let (artifacts, receipt_id) = post_mutation_failure_artifacts();
    let (prior_artifacts, prior_receipt_id) =
        post_mutation_failure_artifacts_for(PRIOR_RUN, PRIOR_CONTROL, 181, false);
    let mut prior_attempts = serde_json::Map::new();
    prior_attempts.insert(
        PRIOR_RUN.to_string(),
        json!({
            "run": failed_run_with_id(PRIOR_RUN, PRIOR_CONTROL, "main", "failure"),
            "jobs": post_mutation_failure_jobs(),
            "artifacts": prior_artifacts,
            "commit": {
                "sha": PRIOR_CONTROL,
                "commit": {"verification": {"verified": true, "reason": "valid"}},
            },
        }),
    );
    let output = run_recovery_with_prior(
        &root,
        failed_run(FAILED_CONTROL, "main", "failure"),
        post_mutation_failure_jobs(),
        artifacts,
        json!({
            "total_count": 2,
            "artifacts": [
                {
                    "id": prior_receipt_id,
                    "name": format!("rmux-publication-receipt-{SOURCE}-{RELEASE_ID}"),
                    "expired": false,
                    "workflow_run": {"id": PRIOR_RUN},
                },
                {
                    "id": receipt_id,
                    "name": format!("rmux-publication-receipt-{SOURCE}-{RELEASE_ID}"),
                    "expired": false,
                    "workflow_run": {"id": FAILED_RUN},
                },
            ],
        }),
        Some(Value::Object(prior_attempts)),
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
    assert!(RECEIPT.contains("uses: ./.github/actions/release-receipt-create"));
    assert!(RECEIPT_CREATE.contains("scripts/release/receipt-recovery.py"));
    assert!(RECEIPT_CREATE.contains("test \"$GITHUB_REF\" = refs/heads/main"));
    assert!(RECEIPT_CREATE.contains("test \"$GITHUB_REF\" = \"refs/tags/$RMUX_RELEASE_REF\""));
    assert!(RECEIPT_CREATE.contains("--recovered-from-run-id \"$RMUX_FAILED_RECEIPT_RUN_ID\""));
}
