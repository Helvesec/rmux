use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_path(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    std::env::temp_dir().join(format!("rmux-{label}-{}-{nonce}", std::process::id()))
}

fn job_block<'a>(workflow: &'a str, job: &str, next_job: &str) -> &'a str {
    workflow
        .split(&format!("\n  {job}:\n"))
        .nth(1)
        .unwrap_or_else(|| panic!("missing job {job}"))
        .split(&format!("\n  {next_job}:\n"))
        .next()
        .unwrap_or_else(|| panic!("unbounded job {job}"))
}

#[test]
fn candidate_route_proves_fast_before_release_only_work() {
    let ci = include_str!("../.github/workflows/ci.yml");
    let verify = job_block(ci, "verify-fast-evidence", "release-candidate-delta");
    let caller = job_block(
        ci,
        "release-candidate-delta",
        "release-candidate-delta-gate",
    );

    for input in [
        "fast_run_id:",
        "expected_source_sha:",
        "release_intent_id:",
        "planned_release_ref:",
        "release_kind:",
    ] {
        assert!(ci.contains(input), "candidate input {input} disappeared");
    }
    for evidence in [
        "name: Verify exhaustive fast evidence",
        "scripts/release/verify-fast-run.py",
        "--kind fast",
        "name: Reject untrusted candidate inputs before checkout",
        "test \"$GITHUB_SHA\" = \"$CANDIDATE_EXPECTED_SOURCE_SHA\"",
        "ref: ${{ github.sha }}",
        "persist-credentials: false",
        "rmux-fast-proof-${{ inputs.expected_source_sha }}",
    ] {
        assert!(verify.contains(evidence), "fast proof lost {evidence}");
    }
    assert!(caller.contains("needs: verify-fast-evidence"));
    assert!(caller.contains("uses: ./.github/workflows/release-candidate-delta.yml"));
    assert!(ci.contains("'rmux-release-candidate'"));
    assert!(ci.contains("cancel-in-progress: false"));
    assert!(!verify.contains("--run-id \"${{ inputs.fast_run_id }}\""));
    assert!(!ci.split("jobs:").next().unwrap().contains("actions: read"));
    assert!(!verify.contains("contents: write"));
    assert!(!caller.contains("contents: write"));
}

#[test]
fn release_candidate_gate_runner_contract_matches_workflow() {
    let ci = include_str!("../.github/workflows/ci.yml");
    let gate = job_block(ci, "release-candidate-gate", "linux-quality");
    assert!(gate.contains("runs-on: ubuntu-22.04"));

    let contract: serde_json::Value =
        serde_json::from_str(include_str!("../.github/release/candidate-contract.json"))
            .expect("parse candidate contract");
    let jobs = &contract["runner_policy"]["jobs_by_label"];
    let pinned = jobs["ubuntu-22.04"]
        .as_array()
        .expect("ubuntu-22.04 runner inventory");
    let floating = jobs["ubuntu-latest"]
        .as_array()
        .expect("ubuntu-latest runner inventory");
    assert!(pinned.iter().any(|job| job == "Release candidate gate"));
    assert!(!floating.iter().any(|job| job == "Release candidate gate"));
}

#[test]
fn candidate_mode_does_not_replay_fast_jobs() {
    let ci = include_str!("../.github/workflows/ci.yml");
    let fast_jobs = [
        ("linux-quality", "linux-build"),
        ("linux-build", "wasm-crypto"),
        ("wasm-crypto", "linux-tests"),
        ("linux-tests", "linux-source-gates"),
        ("linux-source-gates", "tmux-oracle-linux"),
        ("linux-sdk-smoke", "linux-perf-smoke"),
        ("linux-perf-smoke", "linux-dependency-audit"),
        ("linux-dependency-audit", "platform-runtime"),
        ("platform-runtime", "windows-test-archive"),
        ("windows-test-archive", "windows-test-build"),
        ("windows-test-build", "windows-tests"),
        ("windows-tests", "windows-tests-gate"),
        ("windows-cross", "windows-runtime-smoke"),
    ];
    let guard = "!(github.event_name == 'workflow_dispatch' && inputs.release_qualification)";
    for (job, next) in fast_jobs {
        assert!(
            job_block(ci, job, next).contains(guard),
            "fast job {job} is not excluded from candidate mode"
        );
    }
    assert!(job_block(ci, "windows-tests-gate", "windows-test-archive-cache").contains(guard));
    assert!(job_block(ci, "windows-test-archive-cache", "windows-package-build").contains(guard));
}

#[test]
fn candidate_delta_is_call_only_and_has_no_publication_authority() {
    let delta = include_str!("../.github/workflows/release-candidate-delta.yml");
    assert!(delta.contains("workflow_call:"));
    assert!(!delta.contains("workflow_dispatch:"));
    assert!(!delta.contains("push:"));
    assert!(!delta.contains("pull_request:"));
    assert!(!delta.contains("self-hosted"));
    for forbidden in [
        "contents: write",
        "packages: write",
        "id-token: write",
        "attestations: write",
        "environment:",
        "secrets:",
        "gh release",
        "cargo publish",
        "snapcraft upload",
    ] {
        assert!(
            !delta.contains(forbidden),
            "candidate delta gained forbidden capability {forbidden}"
        );
    }
    for line in delta
        .lines()
        .filter(|line| line.trim_start().starts_with("- uses:"))
    {
        let action = line
            .split_once("uses:")
            .expect("uses line")
            .1
            .split_whitespace()
            .next()
            .expect("action value");
        let revision = action
            .rsplit_once('@')
            .expect("third-party Action is pinned")
            .1;
        assert_eq!(
            revision.len(),
            40,
            "Action revision is not a full SHA: {action}"
        );
        assert!(revision
            .chars()
            .all(|character| character.is_ascii_hexdigit()));
    }
}

#[test]
fn candidate_delta_reuses_one_exact_tmux_oracle() {
    let ci = include_str!("../.github/workflows/ci.yml");
    let delta = include_str!("../.github/workflows/release-candidate-delta.yml");
    let oracle = job_block(ci, "tmux-oracle-linux", "release-review-perf");
    assert_eq!(oracle.matches("scripts/oracle/build-tmux37.sh").count(), 1);
    assert!(oracle.contains("rmux-tmux-oracle-${{ github.sha }}"));
    assert!(!delta.contains("scripts/oracle/build-tmux37.sh"));
    assert!(delta.contains("run-id: ${{ inputs.fast_run_id }}"));
    assert!(delta.contains("rmux-tmux-oracle-${{ inputs.expected_source_sha }}"));
    assert!(delta.contains("e802909de06012a4df6209d55e86487c56223163"));
    assert!(delta.contains("87f2e99e3b685973f2ca002ffd6ed7e51a5744f7009daae5a15670b6d532db96"));
}

#[test]
fn release_review_candidate_mode_only_runs_true_deltas() {
    let gate = include_str!("../scripts/release-review-gate.sh");
    let delta = include_str!("../.github/workflows/release-candidate-delta.yml");
    assert!(gate.contains("--evidence-mode"));
    assert!(gate.contains("full|candidate-delta"));
    assert!(gate.contains("section $section is already covered by the exact fast proof"));
    assert!(delta.contains("section: [static, cli, tmux]"));
    assert!(!delta.contains("matrix.section == 'lint'"));
    assert!(!delta.contains("matrix.section == 'server'"));
    assert!(!delta.contains("matrix.section == 'runtime-sdk'"));
    assert!(include_str!("../.github/workflows/ci.yml").contains("--retries 0"));
}

#[test]
fn candidate_intent_validator_binds_source_version_and_attempt() {
    let output = temp_path("candidate-intent");
    let sha = "0123456789abcdef0123456789abcdef01234567";
    let result = Command::new("python3")
        .args([
            "scripts/release/validate-candidate-intent.py",
            "--expected-source-sha",
            sha,
            "--actual-source-sha",
            sha,
            "--fast-run-id",
            "29692655372",
            "--release-intent-id",
            "shadow:0.9.0:test",
            "--planned-release-ref",
            "v0.9.0",
            "--release-kind",
            "shadow",
            "--github-ref",
            "refs/heads/main",
            "--github-run-attempt",
            "1",
            "--output",
        ])
        .arg(&output)
        .output()
        .expect("run candidate intent validator");
    assert!(
        result.status.success(),
        "validator failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let payload = fs::read_to_string(&output).expect("candidate output");
    assert!(payload.contains("\"source_git_sha\": \"0123456789abcdef"));
    assert!(payload.contains("\"planned_release_ref\": \"v0.9.0\""));
    fs::remove_file(&output).expect("remove candidate output");

    let rejected = Command::new("python3")
        .args([
            "scripts/release/validate-candidate-intent.py",
            "--expected-source-sha",
            sha,
            "--actual-source-sha",
            "1123456789abcdef0123456789abcdef01234567",
            "--fast-run-id",
            "1",
            "--release-intent-id",
            "shadow:bad-source",
            "--planned-release-ref",
            "v0.9.0",
            "--release-kind",
            "shadow",
            "--github-ref",
            "refs/heads/main",
            "--github-run-attempt",
            "1",
            "--output",
        ])
        .arg(temp_path("rejected-intent"))
        .status()
        .expect("run rejected candidate intent");
    assert!(!rejected.success());
}

#[test]
fn dispatcher_is_dry_run_and_repository_scoped_by_default() {
    let dispatcher = include_str!("../scripts/release/dispatch-release-candidate.sh");
    assert!(dispatcher.contains("X-GitHub-Api-Version: 2026-03-10"));
    assert!(dispatcher.contains("for _ in 1 2 3 4 5 6"));
    assert!(dispatcher.contains(".head_repository.id == 1239918790"));
    // Windows resolves `bash.exe` to the WSL launcher before Git Bash. The
    // portable contract is checked statically on every target and executed by
    // the Linux lane, where this repository's release shell runs.
    #[cfg(unix)]
    {
        let output = Command::new("bash")
            .args([
                "scripts/release/dispatch-release-candidate.sh",
                "--fast-run-id",
                "29692655372",
                "--expected-source-sha",
                "0123456789abcdef0123456789abcdef01234567",
                "--release-intent-id",
                "shadow:0.9.0:test",
                "--planned-release-ref",
                "v0.9.0",
                "--release-kind",
                "shadow",
            ])
            .output()
            .expect("dry-run candidate dispatcher");
        assert!(
            output.status.success(),
            "dispatcher failed with stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).expect("UTF-8 dispatcher output");
        assert!(stdout.contains("\"mode\": \"dry-run\""));
        assert!(stdout.contains("repos/Helvesec/rmux/actions/workflows/ci.yml/dispatches"));
        assert!(stdout.contains("\"publication_authority\": false"));
    }
}
