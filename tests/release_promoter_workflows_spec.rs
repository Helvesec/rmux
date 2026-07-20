use std::fs;
use std::path::PathBuf;

const TAG: &str = include_str!("../.github/workflows/release-tag-authoring.yml");
const PROMOTE: &str = include_str!("../.github/workflows/release-promote.yml");
const RECEIPT: &str = include_str!("../.github/workflows/release-receipt.yml");
const PUBLICATION_INPUTS: &str =
    include_str!("../.github/actions/release-publication-inputs/action.yml");
const CANDIDATE_STAGING: &str = include_str!("../scripts/release/stage-candidate-release.py");

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
            "workflow gained trigger {trigger}"
        );
    }
    assert!(!workflow.contains("runs-on: self-hosted"));
    assert!(!workflow.contains("runs-on: [self-hosted"));
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

fn assert_workflow_dispatch_only(workflow: &str) {
    assert!(workflow.contains("on:\n  workflow_dispatch:"));
    assert_eq!(workflow.matches("permissions: {}").count(), 1);
    for trigger in [
        "\n  push:",
        "\n  pull_request:",
        "\n  workflow_call:",
        "\n  workflow_run:",
        "\n  repository_dispatch:",
        "\n  release:",
        "\n  schedule:",
    ] {
        assert!(
            !workflow.contains(trigger),
            "workflow gained trigger {trigger}"
        );
    }
    assert!(!workflow.contains("runs-on: self-hosted"));
    assert!(!workflow.contains("runs-on: [self-hosted"));
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

#[test]
fn promoter_workflows_have_exact_triggers_and_remain_triple_disarmed() {
    for workflow in [TAG, PROMOTE] {
        assert_workflow_call_only(workflow);
    }
    assert_workflow_dispatch_only(RECEIPT);
    assert_eq!(TAG.matches("if: ${{ false }}").count(), 1);
    assert_eq!(PROMOTE.matches("if: ${{ false }}").count(), 3);
    assert_eq!(RECEIPT.matches("if: ${{ false }}").count(), 1);

    let activation: serde_json::Value =
        serde_json::from_str(include_str!("../.github/release/release-activation.json"))
            .expect("activation ledger");
    assert_eq!(activation["status"], "disarmed");
    assert_eq!(activation["runtime_override_allowed"], false);
    assert!(activation["capabilities"]
        .as_object()
        .expect("capability object")
        .values()
        .all(|value| value == false));

    let workflows = repo_root().join(".github/workflows");
    let new_names = [
        "release-tag-authoring.yml",
        "release-promote.yml",
        "release-receipt.yml",
    ];
    for entry in fs::read_dir(workflows).expect("list workflows") {
        let path = entry.expect("workflow entry").path();
        if new_names.iter().any(|name| path.ends_with(name)) {
            continue;
        }
        let text = fs::read_to_string(&path).expect("read workflow");
        for name in new_names {
            assert!(
                !text.contains(&format!("uses: ./.github/workflows/{name}")),
                "existing workflow {} calls disarmed {name}",
                path.display()
            );
        }
    }
}

#[test]
fn signed_tag_gate_preserves_dedicated_ssh_signature_and_app_boundary() {
    let create = job(TAG, "create-signed-tag", None);
    assert!(create.contains("if: ${{ false }}"));
    assert!(create.contains("environment: release-tagging"));
    assert!(create.contains("assert-release-capability.py signed_tag_creation"));
    assert!(create.contains("RMUX_RELEASE_SSH_SIGNING_KEY"));
    assert!(create.contains("RMUX_RELEASE_APP_PRIVATE_KEY"));
    assert!(
        create.contains("actions/create-github-app-token@fee1f7d63c2ff003460e3d139729b119787bc349")
    );
    assert!(create.contains("permission-contents: write"));
    assert!(create.contains("sign-and-push-release-tag.sh"));
    assert!(create.contains("signed-tag-proof.py create"));
    assert!(!create.contains("id-token: write"));
    assert!(!create.lines().any(|line| line.trim() == "contents: write"));
}

#[test]
fn promotion_splits_oidc_from_contents_write_and_keeps_exact_dag() {
    let verify = job(PROMOTE, "verify-candidate", Some("policy-audit"));
    let audit = job(PROMOTE, "policy-audit", Some("authorize-promotion"));
    let authorize = job(PROMOTE, "authorize-promotion", Some("publish"));
    let publish = job(PROMOTE, "publish", None);
    assert!(verify.contains("verify-candidate-artifacts.py verify-downloaded"));
    assert!(verify.contains("verify-candidate-attestations.sh"));
    assert!(verify.contains("GH_TOKEN: ${{ github.token }}"));
    assert!(verify.contains("git/ref/tags/$RMUX_RELEASE_REF"));
    assert!(verify.contains("verify-release-tag.py github-json"));
    assert!(verify.contains("differs byte-for-byte from live tag"));
    assert!(verify.contains("stage-candidate-release.py"));
    assert!(CANDIDATE_STAGING.contains("item.get(\"role\") == \"checksums\""));
    assert!(CANDIDATE_STAGING.contains("args.output / \"SHA256SUMS\""));
    assert_eq!(PROMOTE.matches("merge-multiple: true").count(), 4);
    assert!(authorize.contains("rmux-authorization/verified"));
    assert!(authorize.contains("rmux-authorization/policy"));
    assert!(audit.contains("if: ${{ false }}"));
    assert!(audit.contains("uses: ./.github/workflows/release-policy-audit.yml"));
    assert!(authorize.contains("needs: [verify-candidate, policy-audit]"));
    assert!(authorize.contains("id-token: write"));
    assert!(authorize.contains("attestations: write"));
    assert!(authorize
        .contains("RMUX_AUTHORIZATION_WORKFLOW_ID: ${{ inputs.authorization_workflow_id }}"));
    assert!(authorize.contains("[[ ! \"$RMUX_AUTHORIZATION_WORKFLOW_ID\" =~ ^[1-9][0-9]*$ ]]"));
    assert!(authorize.contains("--authorization-workflow-id \"$RMUX_AUTHORIZATION_WORKFLOW_ID\""));
    assert!(!authorize
        .contains("--authorization-workflow-id \"${{ inputs.authorization_workflow_id }}\""));
    assert!(!authorize.contains("contents: write"));
    assert!(publish.contains("needs: [verify-candidate, policy-audit, authorize-promotion]"));
    assert!(publish.contains("contents: write"));
    assert!(!publish.contains("id-token: write"));
    assert!(publish.contains("environment: release-publication"));
    assert!(publish.contains("publish-github-release.py"));
    assert!(publish.contains("SHA256SUMS.sigstore.json"));
    assert!(PUBLICATION_INPUTS.contains("actions-artifact.py verify"));
    assert!(publish.contains("uses: ./.github/actions/release-publication-inputs"));
    assert!(PUBLICATION_INPUTS.contains("Resolve the eleven original candidate artifact IDs again"));
    assert!(PUBLICATION_INPUTS.contains("inputs.manifest-artifact-id"));
    assert!(PUBLICATION_INPUTS.contains("inputs.manifest-run-id"));
    assert!(PUBLICATION_INPUTS.contains("steps.artifacts.outputs.artifact_ids"));
    assert!(PUBLICATION_INPUTS.contains("verify-candidate-artifacts.py verify-downloaded"));
    assert!(PUBLICATION_INPUTS.contains("stage-candidate-release.py"));
    assert!(
        PUBLICATION_INPUTS.contains("authorization does not bind the original candidate manifest")
    );
    assert!(!publish.contains("rmux-publication/verified"));
    assert!(!publish.contains("needs.verify-candidate.outputs.bundle_artifact_id"));
    assert!(!publish.contains("--execute"));
    assert!(!PROMOTE.contains("gh release"));
    assert!(!PROMOTE.contains("--clobber"));
}

#[test]
fn receipt_is_separate_receipt_only_and_never_writes_contents() {
    let receipt = job(RECEIPT, "receipt-only", None);
    assert!(receipt.contains("if: ${{ false }}"));
    assert!(RECEIPT.contains("on:\n  workflow_dispatch:"));
    assert!(!RECEIPT.contains("\n  workflow_call:"));
    assert!(receipt.contains("test \"$GITHUB_RUN_ATTEMPT\" = 1"));
    assert!(receipt.contains("assert-release-capability.py publication_receipt"));
    assert!(receipt.contains("publication-receipt.py create-predicate"));
    assert!(receipt.contains("publication-receipt.py create-envelope"));
    assert!(receipt.contains("id-token: write"));
    assert!(receipt.contains("attestations: write"));
    assert!(!receipt.contains("contents: write"));
    assert!(!RECEIPT.contains("gh release"));
    assert!(!RECEIPT.contains("git push"));
    assert_eq!(RECEIPT.matches("merge-multiple: true").count(), 2);
    assert!(RECEIPT.contains("rmux-receipt/authorization"));
    assert!(RECEIPT.contains("rmux-receipt/envelope"));
    assert!(RECEIPT.contains("actions/runs/$RMUX_AUTHORIZATION_RUN_ID"));
    assert!(RECEIPT.contains("releases/$RMUX_RELEASE_ID/assets?per_page=100&page=1"));
    assert!(RECEIPT.contains("git/ref/tags/$RMUX_RELEASE_REF"));
    assert!(RECEIPT.contains("git/tags/$tag_object_sha"));
    assert!(RECEIPT.contains("\"run_attempt\": 1"));
    assert!(RECEIPT.contains("live immutable Release identity differs"));
    assert!(RECEIPT.contains("live annotated tag signature or target differs"));
    assert!(RECEIPT.contains("Accept: application/octet-stream"));
    assert!(RECEIPT.contains("attestation verify"));
    assert!(RECEIPT.contains(
        "--predicate-type https://rmux.io/attestations/release-promotion-authorization/v1"
    ));
    assert!(RECEIPT.contains("--deny-self-hosted-runners"));
    assert!(RECEIPT.contains("signed authorization predicate differs"));
    assert!(RECEIPT.contains("${{ runner.temp }}/rmux-receipt/release-state.json"));
    assert!(!RECEIPT.contains("release_state_artifact_id"));
}

#[test]
fn every_release_stage_serializes_each_tag_without_cancellation() {
    for (workflow, group) in [
        (TAG, "rmux-release-tag-authoring-${{ inputs.release_ref }}"),
        (PROMOTE, "rmux-release-promote-${{ inputs.release_ref }}"),
        (RECEIPT, "rmux-release-receipt-${{ inputs.release_ref }}"),
    ] {
        assert!(workflow.contains(&format!("group: {group}")));
        assert!(workflow.contains("cancel-in-progress: false"));
    }
}

#[test]
fn only_promoter_calls_policy_audit_and_the_call_remains_disabled() {
    let workflows = repo_root().join(".github/workflows");
    let mut callers = Vec::new();
    for entry in fs::read_dir(workflows).expect("list workflows") {
        let path = entry.expect("workflow entry").path();
        let text = fs::read_to_string(&path).expect("read workflow");
        if text.contains("uses: ./.github/workflows/release-policy-audit.yml") {
            callers.push(path);
        }
    }
    assert_eq!(
        callers,
        vec![repo_root().join(".github/workflows/release-promote.yml")]
    );
    let audit = job(PROMOTE, "policy-audit", Some("authorize-promotion"));
    assert!(audit.contains("if: ${{ false }}"));
}
