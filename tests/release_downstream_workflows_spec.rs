use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

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
        false
    );
    assert_eq!(contract["payload_evidence"]["actions_expiry_bound"], false);
    assert_eq!(
        contract["payload_evidence"]["producer_workflow_allowlist_ready"],
        false
    );
    assert_eq!(
        contract["payload_evidence"]["activation_blockers"],
        serde_json::json!([
            "channel_payload_producer_contract_missing",
            "actions_artifact_expiry_binding_missing"
        ])
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
        contract["receipt_gate"]["activation_blockers"],
        serde_json::json!(["receipt_workflow_id_unset_until_merge"])
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
    for blocked in [
        "chocolatey",
        "crates_io",
        "snap_candidate",
        "snap_stable",
        "web_share",
    ] {
        let channel = channels
            .iter()
            .find(|channel| channel["name"] == blocked)
            .expect("blocked channel");
        assert_eq!(channel["payload_ready"], false, "{blocked} became ready");
        assert!(
            !channel["blockers"].as_array().expect("blockers").is_empty(),
            "{blocked} lost its activation blocker"
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
    assert!(summary.contains("Disabled pre-site result aggregation"));
    assert!(summary.contains("Result aggregation is unavailable in PR7"));
    assert!(rmux_io.contains("Disabled post-aggregation rmux.io deployment"));
    assert!(rmux_io.contains("rmux.io remains unavailable and disabled"));
    assert!(final_summary.contains("Disabled final result aggregation"));
    assert!(final_summary.contains("Final result aggregation is unavailable in PR7"));
    assert!(!DOWNSTREAM.contains("Require eleven exact"));
    assert!(!DOWNSTREAM.contains("Require ten exact"));
    assert!(DOWNSTREAM.contains("test \"$GITHUB_REPOSITORY\" = \"Helvesec/rmux\""));
    assert!(DOWNSTREAM.contains("test \"$GITHUB_REPOSITORY_ID\" = \"1239918790\""));
    assert_eq!(DOWNSTREAM.matches("channel-summary.py create").count(), 2);
    for writer in [linux, chocolatey, rmux_io] {
        assert!(writer.contains("assert-release-capability.py downstream_channels"));
    }
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
