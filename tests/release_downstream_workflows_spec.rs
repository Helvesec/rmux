use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const DOWNSTREAM: &str = include_str!("../.github/workflows/release-downstream.yml");

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn job<'a>(workflow: &'a str, id: &str, next: Option<&str>) -> &'a str {
    let marker = format!("\n  {id}:\n");
    let tail = workflow
        .split(&marker)
        .nth(1)
        .unwrap_or_else(|| panic!("missing job {id}"));
    match next {
        Some(next_id) => tail
            .split(&format!("\n  {next_id}:\n"))
            .next()
            .expect("job boundary"),
        None => tail,
    }
}

fn artifact_verification<'a>(workflow: &'a str, artifact_name: &str) -> &'a str {
    let marker = format!("--name \"{artifact_name}\"");
    workflow
        .split(&marker)
        .nth(1)
        .unwrap_or_else(|| panic!("missing artifact verification for {artifact_name}"))
        .split("--max-attempts 1")
        .next()
        .expect("artifact verification boundary")
}

fn assert_workflow_call_only(workflow: &str) {
    assert!(workflow.contains("on:\n  workflow_call:"));
    assert_eq!(workflow.matches("permissions: {}").count(), 1);
    for trigger in [
        "\n  push:",
        "\n  pull_request:",
        "\n  workflow_dispatch:",
        "\n  workflow_run:",
        "\n  repository_dispatch:",
        "\n  release:",
        "\n  schedule:",
    ] {
        assert!(
            !workflow.contains(trigger),
            "downstream workflow gained trigger {trigger}"
        );
    }
    for line in workflow.lines().map(str::trim) {
        if let Some(target) = line.strip_prefix("uses: ") {
            if target.starts_with("./") {
                continue;
            }
            let (_, revision) = target.rsplit_once('@').expect("Action pin");
            assert_eq!(revision.len(), 40, "Action is not pinned: {target}");
            assert!(revision.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
    }
}

fn calls_disarmed_workflow(line: &str, workflow_name: &str) -> bool {
    let normalized = line.trim().strip_prefix("- ").unwrap_or(line.trim()).trim();
    let Some(target) = normalized.strip_prefix("uses:") else {
        return false;
    };
    let target = target
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(|character| character == '\'' || character == '"')
        .to_ascii_lowercase();
    target == format!("./.github/workflows/{workflow_name}")
        || target.starts_with(&format!("helvesec/rmux/.github/workflows/{workflow_name}@"))
}

#[test]
fn downstream_workflow_is_uncalled_read_only_and_disarmed() {
    assert_workflow_call_only(DOWNSTREAM);
    assert_eq!(DOWNSTREAM.matches("if: ${{ false }}").count(), 5);
    assert!(!DOWNSTREAM.contains("contents: write"));
    assert!(!DOWNSTREAM.contains("id-token: write"));
    assert!(!DOWNSTREAM.contains("attestations: write"));
    assert!(!DOWNSTREAM.contains("secrets:"));
    assert!(!DOWNSTREAM.contains("environment:"));
    assert!(!DOWNSTREAM.contains("self-hosted"));
    assert!(!DOWNSTREAM.contains("larger-runner"));

    let activation: serde_json::Value =
        serde_json::from_str(include_str!("../.github/release/release-activation.json"))
            .expect("activation ledger");
    assert_eq!(activation["status"], "disarmed");
    assert_eq!(activation["runtime_override_allowed"], false);
    assert_eq!(activation["capabilities"]["downstream_channels"], false);

    let workflows = repo_root().join(".github/workflows");
    for entry in fs::read_dir(workflows).expect("list workflows") {
        let path = entry.expect("workflow entry").path();
        let text = fs::read_to_string(&path).expect("read workflow");
        assert!(
            !text
                .lines()
                .any(|line| calls_disarmed_workflow(line, "release-downstream.yml")),
            "existing workflow {} calls the disarmed downstream workflow",
            path.display()
        );
    }
}

#[test]
fn downstream_caller_guard_rejects_relative_and_absolute_targets() {
    for line in [
        "    uses: ./.github/workflows/release-downstream.yml",
        "    uses: Helvesec/rmux/.github/workflows/release-downstream.yml@0123456789012345678901234567890123456789",
        "    uses: 'helvesec/RMUX/.github/workflows/release-downstream.yml@main'",
    ] {
        assert!(calls_disarmed_workflow(line, "release-downstream.yml"));
    }
    assert!(!calls_disarmed_workflow(
        "    uses: Other/repo/.github/workflows/release-downstream.yml@main",
        "release-downstream.yml"
    ));
}

#[test]
fn exact_receipt_ids_digests_origin_and_documents_are_bound() {
    let prepare = job(DOWNSTREAM, "prepare-plan", Some("stage-linux-channels"));
    for input in [
        "receipt_run_id:",
        "receipt_run_workflow_id:",
        "receipt_workflow_id:",
        "receipt_artifact_id:",
        "receipt_artifact_digest:",
        "receipt_envelope_artifact_id:",
        "receipt_envelope_artifact_digest:",
        "receipt_predicate_sha256:",
        "receipt_envelope_sha256:",
        "expected_source_sha:",
        "release_id:",
        "release_ref:",
        "release_kind:",
    ] {
        assert!(DOWNSTREAM.contains(input), "missing exact input {input}");
    }
    assert_eq!(prepare.matches("actions-artifact.py verify").count(), 2);
    for artifact_name in [
        "rmux-publication-receipt-$RMUX_EXPECTED_SOURCE_SHA-$RMUX_RELEASE_ID",
        "rmux-publication-receipt-envelope-$RMUX_EXPECTED_SOURCE_SHA-$RMUX_RELEASE_ID",
    ] {
        assert!(artifact_verification(prepare, artifact_name)
            .contains("--expected-workflow-path .github/workflows/release-receipt.yml"));
    }
    assert_eq!(
        prepare
            .matches("--expected-workflow-path .github/workflows/release-receipt.yml")
            .count(),
        2
    );
    assert!(!prepare.contains("--expected-workflow-path .github/workflows/release-promote.yml"));
    assert!(
        prepare.contains("test \"$RMUX_RECEIPT_RUN_WORKFLOW_ID\" = \"$RMUX_RECEIPT_WORKFLOW_ID\"")
    );
    assert!(prepare.contains("--expected-event workflow_dispatch"));
    assert!(prepare.contains("--expected-head-branch \"$RMUX_RELEASE_REF\""));
    assert_eq!(prepare.matches("artifact-ids:").count(), 2);
    assert_eq!(
        prepare
            .matches("run-id: ${{ inputs.receipt_run_id }}")
            .count(),
        2
    );
    assert!(!prepare.contains("pattern:"));
    assert_eq!(prepare.matches("merge-multiple: true").count(), 2);
    assert!(prepare.contains("receipt predicate acquired downstream authority"));
    assert!(prepare.contains("receipt envelope acquired downstream authority"));
    assert!(prepare.contains("receipt artifact contains a symlink"));
    assert!(prepare.contains("receipt artifact file set differs"));
    assert!(prepare.contains("release-state.json"));
    assert!(prepare.contains("verify-receipt-attestation.py"));
    assert!(prepare.contains("GH_TOKEN: ${{ github.token }}"));
    assert!(prepare.contains("install-gh-2.93.0.sh"));
    assert!(prepare.contains("RMUX_RECEIPT_PREDICATE_SHA256"));
    assert!(prepare.contains("RMUX_RECEIPT_ENVELOPE_SHA256"));
    assert!(prepare.contains("channel-policy.py create-plan"));
    assert!(prepare.contains("channel-policy.py verify-plan"));
    assert!(prepare.contains("--snap-candidate-opt-in"));
}

#[test]
fn all_eleven_channels_are_default_denied_and_rmux_io_is_last() {
    let contract: serde_json::Value = serde_json::from_str(include_str!(
        "../.github/release/downstream-channel-contract.json"
    ))
    .expect("downstream contract");
    assert_eq!(contract["status"], "review-only-disarmed");
    assert_eq!(contract["execution"]["only_trigger"], "workflow_call");
    assert_eq!(contract["execution"]["public_callers"], 0);
    assert_eq!(
        contract["execution"]["required_caller_repository"],
        "Helvesec/rmux"
    );
    assert_eq!(
        contract["execution"]["required_caller_repository_id"],
        1239918790
    );
    assert_eq!(contract["execution"]["privileged_job_condition"], "false");
    assert_eq!(contract["execution"]["maximum_parallel_channels"], 4);
    assert_eq!(contract["execution"]["github_hosted_only"], true);
    assert_eq!(contract["execution"]["native_rebuild_allowed"], false);
    assert_eq!(contract["execution"]["rmux_io_last"], true);
    assert_eq!(
        contract["payload_evidence"]["canonical_provenance_ready"],
        true
    );
    assert_eq!(contract["payload_evidence"]["actions_expiry_bound"], true);
    assert_eq!(
        contract["payload_evidence"]["producer_workflow_allowlist_ready"],
        true
    );
    assert_eq!(
        contract["payload_evidence"]["producer"],
        serde_json::json!({
            "workflow_id": 316435347,
            "workflow_path": ".github/workflows/release-receipt.yml",
            "job_workflow_path": ".github/workflows/release-downstream.yml",
            "required_run_attempt": 1
        })
    );
    assert_eq!(
        contract["payload_evidence"]["activation_blockers"],
        serde_json::json!([])
    );
    assert_eq!(contract["result_evidence"]["result_reference_ready"], true);
    assert_eq!(
        contract["result_evidence"]["result_reference_schema"],
        ".github/release/schemas/downstream-channel-result-reference.schema.json"
    );
    assert_eq!(
        contract["result_evidence"]["attestation_verification_ready"],
        true
    );
    assert_eq!(
        contract["result_evidence"]["result_aggregation_ready"],
        true
    );
    assert_eq!(
        contract["result_evidence"]["aggregation_blockers"],
        serde_json::json!([])
    );
    assert_eq!(
        contract["result_evidence"]["summary_phases"],
        serde_json::json!(["pre-site", "final"])
    );
    assert_eq!(contract["receipt_gate"]["attestation_required"], true);
    assert_eq!(
        contract["receipt_gate"]["attestation_subject_name"],
        "release-state.json"
    );
    assert_eq!(
        contract["receipt_gate"]["attestation_subject_in_bundle"],
        true
    );
    assert_eq!(
        contract["receipt_gate"]["workflow_id"],
        serde_json::json!(316435347)
    );
    assert_eq!(
        contract["receipt_gate"]["activation_blockers"],
        serde_json::json!([])
    );

    let channels = contract["channels"].as_array().expect("channels");
    assert_eq!(channels.len(), 11);
    let names: BTreeSet<_> = channels
        .iter()
        .map(|channel| channel["name"].as_str().expect("channel name"))
        .collect();
    assert_eq!(
        names,
        BTreeSet::from([
            "apt_rpm",
            "chocolatey",
            "crates_io",
            "homebrew_core",
            "homebrew_tap",
            "rmux_io",
            "scoop",
            "snap_candidate",
            "snap_stable",
            "web_share",
            "winget",
        ])
    );
    for ready in ["chocolatey", "crates_io", "snap_candidate", "web_share"] {
        let channel = channels
            .iter()
            .find(|channel| channel["name"] == ready)
            .expect("canonical payload channel");
        assert_eq!(channel["payload_ready"], true, "{ready} lost its payload");
    }
    let web_share = channels
        .iter()
        .find(|channel| channel["name"] == "web_share")
        .expect("web-share channel");
    assert_eq!(
        web_share["blockers"],
        serde_json::json!([
            "floating_actions_present",
            "unprotected_secret_deployment_workflow"
        ])
    );
    let snap_stable = channels
        .iter()
        .find(|channel| channel["name"] == "snap_stable")
        .expect("Snap stable channel");
    assert_eq!(snap_stable["payload_ready"], false);
    assert_eq!(snap_stable["payload_roles"], serde_json::json!([]));
    assert_eq!(
        snap_stable["blockers"],
        serde_json::json!(["denied_until_support_decision"])
    );

    let canonical: serde_json::Value = serde_json::from_str(include_str!(
        "../.github/release/canonical-build-contract.json"
    ))
    .expect("canonical build contract");
    let supplemental_roles: BTreeSet<_> = canonical["platforms"]
        .as_array()
        .expect("canonical platforms")
        .iter()
        .flat_map(|platform| {
            platform["supplemental_roles"]
                .as_array()
                .expect("supplemental roles")
        })
        .map(|role| role.as_str().expect("supplemental role"))
        .collect();
    for role in [
        "chocolatey-package",
        "crate-package-set",
        "snap-amd64",
        "snap-arm64",
        "wasm-byte-set",
        "wasm-provenance",
    ] {
        assert!(
            supplemental_roles.contains(role),
            "downstream payload role {role} is not sealed canonically"
        );
    }

    let linux = job(DOWNSTREAM, "stage-linux-channels", Some("stage-chocolatey"));
    let chocolatey = job(DOWNSTREAM, "stage-chocolatey", Some("channel-summary"));
    let summary = job(DOWNSTREAM, "channel-summary", Some("deploy-rmux-io"));
    let rmux_io = job(DOWNSTREAM, "deploy-rmux-io", Some("final-channel-summary"));
    let final_summary = job(DOWNSTREAM, "final-channel-summary", None);
    assert!(linux.contains("max-parallel: 3"));
    assert!(linux.contains("if: ${{ false }}"));
    assert!(chocolatey.contains("if: ${{ false }}"));
    assert!(summary.contains("if: ${{ false }}"));
    assert!(rmux_io.contains("if: ${{ false }}"));
    assert!(final_summary.contains("if: ${{ false }}"));
    assert!(summary.contains("needs: [prepare-plan, stage-linux-channels, stage-chocolatey]"));
    assert!(rmux_io.contains("needs: [prepare-plan, channel-summary]"));
    assert!(final_summary.contains("needs: [prepare-plan, channel-summary, deploy-rmux-io]"));
    assert!(summary.contains("Disabled pre-site aggregation wiring"));
    assert!(summary.contains("writers expose ten exact result references"));
    assert!(rmux_io.contains("Disabled rmux.io writer"));
    assert!(rmux_io.contains("consumes the exact pre-site summary"));
    assert!(final_summary.contains("Disabled final aggregation wiring"));
    assert!(final_summary.contains("exact rmux.io result reference"));
    assert!(!DOWNSTREAM.contains("Consume eleven exact"));
    assert!(!DOWNSTREAM.contains("Consume ten exact"));
    assert!(DOWNSTREAM.contains("test \"$GITHUB_REPOSITORY\" = \"Helvesec/rmux\""));
    assert!(DOWNSTREAM.contains("test \"$GITHUB_REPOSITORY_ID\" = \"1239918790\""));
    assert_eq!(DOWNSTREAM.matches("channel-summary.py create").count(), 0);
    for writer in [linux, chocolatey, rmux_io] {
        assert!(writer.contains("assert-release-capability.py downstream_channels"));
    }
}

#[test]
fn public_owned_downstream_protection_layers_are_recorded_but_disarmed() {
    let registry: serde_json::Value = serde_json::from_str(include_str!(
        "../.github/release/downstream-repositories.json"
    ))
    .expect("downstream repository registry");
    assert_eq!(registry["writer_app"]["configured"], true);
    assert_eq!(registry["writer_app"]["app_id"], 4352876);
    assert_eq!(registry["writer_app"]["installation_id"], 147959477);
    assert_eq!(registry["writer_app"]["repository_selection"], "selected");
    assert_eq!(registry["writer_app"]["pat_fallback"], false);
    let repositories = registry["repositories"].as_array().expect("repositories");

    for key in [
        "homebrew-rmux",
        "rmux-packages",
        "rmux-web-share",
        "scoop-rmux",
    ] {
        let repository = repositories
            .iter()
            .find(|repository| repository["key"] == key)
            .unwrap_or_else(|| panic!("missing downstream repository {key}"));
        assert_eq!(repository["branch_protected"], true, "{key}");
        assert_eq!(repository["ruleset_count"], 1, "{key}");
        assert_eq!(repository["environment_count"], 1, "{key}");
        let blockers = repository["blockers"].as_array().expect("blockers");
        assert!(
            blockers
                .iter()
                .all(|blocker| blocker != "environment_admin_bypass_enabled"),
            "{key} still reports environment bypass"
        );
        assert!(
            blockers
                .iter()
                .all(|blocker| blocker != "downstream_writer_app_missing"),
            "{key} still reports a missing writer App"
        );
        assert!(
            blockers
                .iter()
                .all(|blocker| blocker != "repository_protection_missing"),
            "{key} still reports missing repository protection"
        );
    }

    for key in ["homebrew-rmux", "rmux-packages", "scoop-rmux"] {
        let repository = repositories
            .iter()
            .find(|repository| repository["key"] == key)
            .unwrap_or_else(|| panic!("missing downstream repository {key}"));
        assert_eq!(repository["activation_ready"], true, "{key}");
        assert_eq!(repository["blockers"], serde_json::json!([]), "{key}");
    }

    let web_share = repositories
        .iter()
        .find(|repository| repository["key"] == "rmux-web-share")
        .expect("missing downstream repository rmux-web-share");
    assert_eq!(web_share["activation_ready"], false);
    assert_eq!(
        web_share["blockers"],
        serde_json::json!([
            "floating_actions_present",
            "unprotected_secret_deployment_workflow"
        ])
    );

    let rmux_io = repositories
        .iter()
        .find(|repository| repository["key"] == "rmux.io")
        .expect("missing downstream repository rmux.io");
    assert_eq!(rmux_io["visibility"], "private");
    assert_eq!(rmux_io["activation_ready"], false);
    assert_eq!(
        rmux_io["blockers"],
        serde_json::json!([
            "private_repository_protection_unavailable_on_current_plan",
            "manual_site_update_required"
        ])
    );
}

#[test]
fn downstream_repository_verifier_accepts_github_ruleset_arrays() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rmux-downstream-repository-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("create fixture directory");
    let write = |name: &str, value: serde_json::Value| {
        let path = root.join(name);
        fs::write(&path, serde_json::to_vec(&value).expect("encode fixture"))
            .expect("write fixture");
        path
    };
    let metadata = write(
        "metadata.json",
        serde_json::json!({
            "id": 1259133629,
            "full_name": "Helvesec/homebrew-rmux",
            "visibility": "public",
            "default_branch": "main",
            "archived": false
        }),
    );
    let protection = write(
        "protection.json",
        serde_json::json!({
            "enforce_admins": {"enabled": true},
            "allow_force_pushes": {"enabled": false},
            "allow_deletions": {"enabled": false},
            "required_signatures": {"enabled": true}
        }),
    );
    let rulesets = write(
        "rulesets.json",
        serde_json::json!([{"enforcement": "active"}]),
    );
    let environments = write(
        "environments.json",
        serde_json::json!({"environments": [{
            "name": "release-homebrew-tap",
            "can_admins_bypass": false,
            "protection_rules": [{"type": "required_reviewers"}]
        }]}),
    );
    let runners = write("runners.json", serde_json::json!({"total_count": 0}));
    let installation = write(
        "installation.json",
        serde_json::json!({
            "id": 147959477,
            "app_id": 4352876,
            "repository_selection": "selected",
            "permissions": {"actions": "read", "contents": "write", "metadata": "read"},
            "events": [],
            "repository_ids": [1249553407, 1258602064, 1259133629, 1259135161]
        }),
    );
    let output = Command::new("python3")
        .arg(repo_root().join("scripts/release/verify-downstream-repository.py"))
        .args(["fixtures", "--repository-key", "homebrew-rmux"])
        .arg("--metadata")
        .arg(metadata)
        .arg("--protection")
        .arg(protection)
        .arg("--rulesets")
        .arg(rulesets)
        .arg("--environments")
        .arg(environments)
        .arg("--runners")
        .arg(runners)
        .arg("--installation")
        .arg(installation)
        .current_dir(repo_root())
        .output()
        .expect("run downstream repository verifier");
    fs::remove_dir_all(root).expect("remove fixture directory");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn disarmed_workflow_has_no_store_or_repository_mutation_primitive() {
    for forbidden in [
        "cargo publish",
        "choco push",
        "snapcore/action-publish",
        "snapcraft upload",
        "wrangler pages deploy",
        "git push",
        "gh release",
        "gh pr create",
        "curl -X POST",
        "curl -X PUT",
        "curl -X PATCH",
        "curl -X DELETE",
        "repository_dispatch",
    ] {
        assert!(
            !DOWNSTREAM.contains(forbidden),
            "disarmed workflow contains mutation primitive {forbidden}"
        );
    }
}
