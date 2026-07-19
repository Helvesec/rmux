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
    assert_eq!(workflow.matches("urllib.request.Request(").count(), 1);
    assert_eq!(workflow.matches("urllib.request.urlopen(").count(), 1);
    assert!(workflow.contains("MAX_GET_ATTEMPTS = 3"));
    assert!(workflow.contains("X-RateLimit-Remaining"));
    for runner_proof in [
        "github_standard_hosted",
        "runner_group_id",
        "runner_group_name",
        "runner_name",
        "self-hosted",
    ] {
        assert!(
            workflow.contains(runner_proof),
            "shadow does not bind runner proof field {runner_proof}"
        );
    }

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
        "data=",
        "import subprocess",
        "import requests",
        "http.client",
        "socket.",
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
    fs::create_dir_all(root.join(".github/release")).expect("create contract directory");
    fs::write(
        root.join(".github/release/candidate-contract.json"),
        include_str!("../.github/release/candidate-contract.json"),
    )
    .expect("write candidate contract fixture");
    for arguments in [
        vec!["init", "-q"],
        vec!["config", "user.name", "RMUX test"],
        vec!["config", "user.email", "rmux-test@example.invalid"],
        vec!["add", ".github/release/candidate-contract.json"],
        vec!["commit", "-q", "-m", "candidate contract fixture"],
    ] {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(&root)
            .output()
            .expect("run git fixture command");
        assert!(output.status.success());
    }
    let sha_output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&root)
        .output()
        .expect("resolve contract fixture SHA");
    let sha = String::from_utf8(sha_output.stdout)
        .expect("UTF-8 contract fixture SHA")
        .trim()
        .to_owned();
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
        "name": "CI",
        "event": "push",
        "head_branch": "main",
        "head_sha": &sha,
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
            let name = name.as_str().expect("job name");
            jobs.push(job_fixture(
                next_id,
                run_id,
                &sha,
                name,
                conclusion,
                contracted_runner_label(&contract, name),
            ));
            next_id += 1;
        }
    }
    for (name, conclusions) in fast["allowed_jobs"].as_object().expect("allowed jobs") {
        jobs.push(job_fixture(
            next_id,
            run_id,
            &sha,
            name,
            conclusions[0].as_str().expect("allowed conclusion"),
            contracted_runner_label(&contract, name),
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
        &sha,
        "--kind",
        "fast",
        "--repository-root",
        root.to_str().expect("fixture repository root"),
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
    assert_eq!(proof["jobs"][0]["runner_group_id"], 0);
    assert_eq!(proof["jobs"][0]["runner_group_name"], "GitHub Actions");

    let original_jobs = fs::read_to_string(&jobs_path).expect("read accepted jobs fixture");
    let mut self_hosted: serde_json::Value =
        serde_json::from_str(&original_jobs).expect("parse accepted jobs fixture");
    let executed = self_hosted["jobs"]
        .as_array_mut()
        .expect("jobs array")
        .iter_mut()
        .find(|job| job["conclusion"] == "success")
        .expect("executed fixture job");
    executed["runner_group_id"] = serde_json::json!(1);
    executed["runner_group_name"] = serde_json::json!("Default");
    executed["runner_name"] = serde_json::json!("GitHub Actions 1");
    fs::write(&jobs_path, self_hosted.to_string()).expect("write self-hosted fixture");
    let rejected_runner = run(&verifier, &common);
    assert!(!rejected_runner.status.success());
    assert!(String::from_utf8_lossy(&rejected_runner.stderr).contains("group ID"));

    let mut boolean_group: serde_json::Value =
        serde_json::from_str(&original_jobs).expect("parse accepted jobs fixture");
    let boolean_group_job = boolean_group["jobs"]
        .as_array_mut()
        .expect("jobs array")
        .iter_mut()
        .find(|job| job["conclusion"] == "success")
        .expect("executed fixture job");
    boolean_group_job["runner_group_id"] = serde_json::json!(false);
    fs::write(&jobs_path, boolean_group.to_string()).expect("write boolean group ID");
    let rejected_boolean_group = run(&verifier, &common);
    assert!(!rejected_boolean_group.status.success());
    assert!(String::from_utf8_lossy(&rejected_boolean_group.stderr)
        .contains("group ID must be the integer zero"));

    let mut invalid_skip: serde_json::Value =
        serde_json::from_str(&original_jobs).expect("parse accepted jobs fixture");
    let skipped = invalid_skip["jobs"]
        .as_array_mut()
        .expect("jobs array")
        .iter_mut()
        .find(|job| job["conclusion"] == "skipped")
        .expect("skipped fixture job");
    skipped["runner_id"] = serde_json::json!(1234);
    skipped["runner_name"] = serde_json::json!("GitHub Actions 1234");
    skipped["runner_group_id"] = serde_json::json!(0);
    skipped["runner_group_name"] = serde_json::json!("GitHub Actions");
    fs::write(&jobs_path, invalid_skip.to_string()).expect("write invalid skipped job");
    let rejected_skip = run(&verifier, &common);
    assert!(!rejected_skip.status.success());
    assert!(String::from_utf8_lossy(&rejected_skip.stderr).contains("skipped job"));

    let mut missing_skip_field: serde_json::Value =
        serde_json::from_str(&original_jobs).expect("parse accepted jobs fixture");
    let skipped = missing_skip_field["jobs"]
        .as_array_mut()
        .expect("jobs array")
        .iter_mut()
        .find(|job| job["conclusion"] == "skipped")
        .expect("skipped fixture job");
    skipped
        .as_object_mut()
        .expect("skipped job object")
        .remove("runner_id");
    fs::write(&jobs_path, missing_skip_field.to_string()).expect("write missing runner ID");
    let rejected_missing_skip_field = run(&verifier, &common);
    assert!(!rejected_missing_skip_field.status.success());
    assert!(String::from_utf8_lossy(&rejected_missing_skip_field.stderr)
        .contains("is missing runner_id"));

    let mut boolean_job_id: serde_json::Value =
        serde_json::from_str(&original_jobs).expect("parse accepted jobs fixture");
    boolean_job_id["jobs"][0]["id"] = serde_json::json!(true);
    fs::write(&jobs_path, boolean_job_id.to_string()).expect("write boolean job ID");
    let rejected_boolean_job_id = run(&verifier, &common);
    assert!(!rejected_boolean_job_id.status.success());
    assert!(String::from_utf8_lossy(&rejected_boolean_job_id.stderr)
        .contains("every job must have a positive integer ID"));

    let mut wrong_label: serde_json::Value =
        serde_json::from_str(&original_jobs).expect("parse accepted jobs fixture");
    wrong_label["jobs"][0]["labels"] = serde_json::json!(["self-hosted"]);
    fs::write(&jobs_path, wrong_label.to_string()).expect("write wrong runner label");
    let rejected_label = run(&verifier, &common);
    assert!(!rejected_label.status.success());
    assert!(String::from_utf8_lossy(&rejected_label.stderr).contains("runner labels"));

    fs::write(&jobs_path, &original_jobs).expect("restore accepted jobs fixture");

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
fn contracted_runner_label<'a>(contract: &'a serde_json::Value, name: &str) -> &'a str {
    contract["runner_policy"]["jobs_by_label"]
        .as_object()
        .expect("runner jobs by label")
        .iter()
        .find_map(|(label, names)| {
            names
                .as_array()
                .expect("runner job list")
                .iter()
                .any(|value| value.as_str() == Some(name))
                .then_some(label.as_str())
        })
        .unwrap_or_else(|| panic!("missing runner label for {name}"))
}

#[cfg(unix)]
fn job_fixture(
    id: u64,
    run_id: u64,
    sha: &str,
    name: &str,
    conclusion: &str,
    runner_label: &str,
) -> serde_json::Value {
    let (runner_id, runner_name, runner_group_id, runner_group_name) = if conclusion == "skipped" {
        (
            serde_json::Value::Null,
            serde_json::Value::Null,
            serde_json::Value::Null,
            serde_json::Value::Null,
        )
    } else {
        (
            serde_json::json!(id + 1000),
            serde_json::json!(format!("GitHub Actions {}", id + 1000)),
            serde_json::json!(0),
            serde_json::json!("GitHub Actions"),
        )
    };
    serde_json::json!({
        "id": id,
        "run_id": run_id,
        "run_attempt": 1,
        "head_sha": sha,
        "workflow_name": "CI",
        "name": name,
        "status": "completed",
        "conclusion": conclusion,
        "labels": [runner_label],
        "runner_id": runner_id,
        "runner_name": runner_name,
        "runner_group_id": runner_group_id,
        "runner_group_name": runner_group_name,
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
            "name": "CI",
            "event": "push",
            "head_branch": "main",
            "head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "run_attempt": 1,
            "status": "completed",
            "conclusion": "success",
            "run_started_at": "2026-07-19T09:00:00Z",
            "updated_at": "2026-07-19T09:03:00Z"
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
        "run_id": 77,
        "run_attempt": 1,
        "head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
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
    assert!(!source.contains("--fast-run-id"));
    assert!(!source.contains("--release-intent-id"));
    assert!(!source.contains("--planned-release-ref"));

    let output = run(
        &controller,
        &[
            "--repository",
            "Helvesec/rmux",
            "--expected-source-sha",
            "cccccccccccccccccccccccccccccccccccccccc",
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
    assert_eq!(request["binding"], "none-baseline-qualification-only");

    let foreign = run(
        &controller,
        &[
            "--repository",
            "someone/else",
            "--expected-source-sha",
            "cccccccccccccccccccccccccccccccccccccccc",
        ],
    );
    assert!(!foreign.status.success());
    assert!(String::from_utf8_lossy(&foreign.stderr)
        .contains("repository must be exactly Helvesec/rmux"));
}

#[test]
#[cfg(unix)]
fn local_actions_are_confined_to_the_policy_directory() {
    let root = temp_dir("local-action-policy");
    fs::create_dir_all(root.join(".github/workflows")).expect("create workflow directory");
    fs::create_dir_all(root.join(".github/actions/example")).expect("create action directory");
    fs::write(
        root.join(".github/workflows/ci.yml"),
        "jobs:\n  test:\n    steps:\n      - uses: ./.github/actions/example\n",
    )
    .expect("write workflow fixture");
    fs::write(
        root.join(".github/actions/example/action.yml"),
        "name: example\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: true\n",
    )
    .expect("write action fixture");
    fs::write(root.join(".github/actions/example/helper.sh"), "true\n")
        .expect("write action helper");
    for arguments in [
        vec!["init", "-q"],
        vec!["config", "user.name", "RMUX test"],
        vec!["config", "user.email", "rmux-test@example.invalid"],
        vec!["add", "."],
    ] {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(&root)
            .output()
            .expect("run local action fixture command");
        assert!(output.status.success());
    }
    let policy = repo_root().join("scripts/release/local_action_policy.py");
    let root_text = root.to_str().expect("fixture root");
    let accepted = run(&policy, &["--repository-root", root_text]);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );

    fs::create_dir_all(root.join("tools/release-action")).expect("create foreign action");
    fs::write(
        root.join("tools/release-action/action.yml"),
        "name: foreign\nruns:\n  using: composite\n  steps: []\n",
    )
    .expect("write foreign action");
    fs::write(
        root.join(".github/workflows/ci.yml"),
        "jobs:\n  test:\n    steps:\n      - uses : ./tools/release-action\n",
    )
    .expect("reference foreign action");
    assert!(Command::new("git")
        .args(["add", "."])
        .current_dir(&root)
        .status()
        .expect("stage foreign action fixture")
        .success());
    let rejected_workflow = run(&policy, &["--repository-root", root_text]);
    assert!(!rejected_workflow.status.success());
    assert!(String::from_utf8_lossy(&rejected_workflow.stderr)
        .contains("manifests must live below .github/actions"));

    fs::write(
        root.join(".github/workflows/ci.yml"),
        "jobs:\n  test:\n    steps:\n      - uses: ./.github/actions/example\n",
    )
    .expect("restore workflow fixture");
    fs::write(
        root.join(".github/actions/example/action.yml"),
        "name: example\nruns:\n  using: composite\n  steps:\n    - {uses: ./tools/release-action}\n",
    )
    .expect("reference foreign action from composite");
    assert!(Command::new("git")
        .args(["add", "."])
        .current_dir(&root)
        .status()
        .expect("stage composite fixture")
        .success());
    let rejected_composite = run(&policy, &["--repository-root", root_text]);
    assert!(!rejected_composite.status.success());
    assert!(String::from_utf8_lossy(&rejected_composite.stderr)
        .contains("manifests must live below .github/actions"));

    fs::remove_file(root.join("tools/release-action/action.yml"))
        .expect("remove foreign action manifest");
    fs::write(
        root.join("tools/release-action/Action.yml"),
        "name: wrong-case\nruns:\n  using: composite\n  steps: []\n",
    )
    .expect("write wrong-case action manifest");
    assert!(Command::new("git")
        .args(["add", "-A"])
        .current_dir(&root)
        .status()
        .expect("stage wrong-case action fixture")
        .success());
    let rejected_wrong_case = run(&policy, &["--repository-root", root_text]);
    assert!(!rejected_wrong_case.status.success());
    assert!(String::from_utf8_lossy(&rejected_wrong_case.stderr)
        .contains("must use the exact lowercase name"));
    fs::remove_dir_all(root).expect("remove local action fixtures");
}

#[test]
fn candidate_artifacts_allow_only_standard_github_runner_labels() {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../.github/release/schemas/candidate-manifest.schema.json"
    ))
    .expect("parse candidate manifest schema");
    assert_eq!(
        schema["properties"]["artifacts"]["items"]["properties"]["runner_image"]["enum"],
        serde_json::json!([
            "macos-15",
            "macos-15-intel",
            "ubuntu-latest",
            "windows-latest"
        ])
    );
}

#[test]
fn draft_authority_schemas_cannot_drive_a_workflow() {
    for schema in [
        include_str!("../.github/release/schemas/candidate-manifest.schema.json"),
        include_str!("../.github/release/schemas/promotion-authorization.schema.json"),
        include_str!("../.github/release/schemas/publication-receipt.schema.json"),
    ] {
        let schema: serde_json::Value = serde_json::from_str(schema).expect("parse draft schema");
        assert_eq!(schema["x-rmux-status"], "draft-non-authoritative");
        assert!(schema["description"]
            .as_str()
            .expect("schema description")
            .contains("MUST NOT authorize publication"));
    }

    for entry in
        fs::read_dir(repo_root().join(".github/workflows")).expect("read workflow directory")
    {
        let path = entry.expect("workflow entry").path();
        if !matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("yml" | "yaml")
        ) {
            continue;
        }
        let workflow = fs::read_to_string(&path).expect("read workflow");
        for authority_schema in [
            "promotion-authorization.schema.json",
            "publication-receipt.schema.json",
        ] {
            assert!(
                !workflow.contains(authority_schema),
                "{} consumes draft authority schema {authority_schema}",
                path.display()
            );
        }
    }
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
        "/.github/CODEOWNERS",
        "/.github/workflows/",
        "/.github/release/",
        "/Cargo.lock",
        "/rust-toolchain.toml",
        "/scripts/",
        "/snap/snapcraft.yaml",
        "/tests/",
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
        r#"{"policy_paths":[".github/release/candidate-contract.json","a.txt"]}"#,
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
    fs::write(
        root.join(".github/release/candidate-contract.json"),
        r#"{"policy_paths":[".github/release/candidate-contract.json"]}"#,
    )
    .expect("write uncommitted contract drift");
    let same_commit = run_policy_root(&root, &first_sha);
    assert_eq!(
        first["release_policy_sha256"], same_commit["release_policy_sha256"],
        "uncommitted bytes must not influence a Git-rooted policy hash"
    );

    fs::write(
        root.join(".github/release/candidate-contract.json"),
        r#"{"policy_paths":[".github/release/candidate-contract.json","a.txt"]}"#,
    )
    .expect("restore policy contract");
    let add = Command::new("git")
        .args(["add", "a.txt", ".github/release/candidate-contract.json"])
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
    let output = run(
        &repo_root().join("scripts/release/policy-root.py"),
        &[
            "--repository-root",
            repository_root.to_str().expect("fixture root path"),
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
