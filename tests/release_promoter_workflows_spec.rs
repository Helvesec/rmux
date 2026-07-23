use std::fs;
use std::path::PathBuf;

const TAG: &str = include_str!("../.github/workflows/release-tag-authoring.yml");
const PROMOTE: &str = include_str!("../.github/workflows/release-promote.yml");
const RECEIPT: &str = include_str!("../.github/workflows/release-receipt.yml");
const GITHUB_PUBLISH: &str = include_str!("../.github/workflows/release-github-publish.yml");
const RECEIPT_DISPATCH: &str = include_str!("../.github/workflows/release-receipt-dispatch.yml");
const SIMULATION: &str = include_str!("../.github/workflows/release-promotion-simulation.yml");
const SCHEMA_VALIDATOR_REQUIREMENTS: &str =
    include_str!("../.github/release/schema-validator-requirements.txt");
const PUBLICATION_INPUTS: &str =
    include_str!("../.github/actions/release-publication-inputs/action.yml");
const CANDIDATE_STAGING: &str = include_str!("../scripts/release/stage-candidate-release.py");
const PROMOTION_SIMULATION: &str = include_str!("../scripts/release/promotion-simulation.py");
const CI: &str = include_str!("../.github/workflows/ci.yml");
const LEGACY_RELEASE: &str = include_str!("../.github/workflows/release.yml");
const LEGACY_CHOCOLATEY: &str = include_str!("../.github/workflows/publish-chocolatey.yml");
const SECURITY: &str = include_str!("../SECURITY.md");

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
fn promoter_workflows_have_exact_dispatch_only_triggers_and_are_active() {
    for workflow in [TAG, PROMOTE] {
        assert_workflow_dispatch_only(workflow);
    }
    assert_workflow_dispatch_only(RECEIPT);
    for workflow in [GITHUB_PUBLISH, RECEIPT_DISPATCH] {
        assert_workflow_call_only(workflow);
    }
    assert_eq!(TAG.matches("if: ${{ false }}").count(), 0);
    assert_eq!(PROMOTE.matches("if: ${{ false }}").count(), 0);
    assert_eq!(RECEIPT.matches("if: ${{ false }}").count(), 0);

    let activation: serde_json::Value =
        serde_json::from_str(include_str!("../.github/release/release-activation.json"))
            .expect("activation ledger");
    assert_eq!(activation["status"], "active");
    assert_eq!(activation["runtime_override_allowed"], false);
    assert!(activation["capabilities"]
        .as_object()
        .expect("capability object")
        .values()
        .all(|value| value == true));

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
                "existing workflow {} unexpectedly calls {name}",
                path.display()
            );
        }
    }
}

#[test]
fn legacy_tag_and_chocolatey_publishers_are_inert_after_cutover() {
    assert!(!CI
        .split("\npermissions:")
        .next()
        .expect("CI header")
        .contains("\n    tags:"));
    assert!(!LEGACY_RELEASE.contains("\n  push:"));
    for (job_id, next_job) in [
        ("source-gates", "build"),
        (
            "validate-external-configuration",
            "package-repository-snapshot",
        ),
    ] {
        let block = job(LEGACY_RELEASE, job_id, Some(next_job));
        assert!(block.contains("if: ${{ false }}"), "{job_id}");
    }
    let chocolatey = job(LEGACY_CHOCOLATEY, "publish", None);
    assert!(chocolatey.contains("if: ${{ false }}"));
}

#[test]
fn signed_tag_gate_preserves_dedicated_ssh_signature_and_app_boundary() {
    let create = job(TAG, "create-signed-tag", Some("dispatch-promoter"));
    assert!(!create.contains("if: ${{ false }}"));
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
fn signed_tag_artifact_stages_all_evidence_at_its_root() {
    let create = job(TAG, "create-signed-tag", Some("dispatch-promoter"));
    assert!(create.contains("bundle=\"$RUNNER_TEMP/rmux-signed-tag\""));
    assert!(create.contains(
        "\"$RUNNER_TEMP/rmux-tag-input/candidate-reference.json\" \\\n            \"$bundle/candidate-reference.json\""
    ));
    for filename in [
        "candidate-reference.json",
        "signed-tag-proof.json",
        "tag-verification.json",
    ] {
        assert!(
            create.contains(&format!(
                "${{{{ runner.temp }}}}/rmux-signed-tag/{filename}"
            )),
            "signed-tag artifact does not stage {filename} at its root"
        );
    }
    assert!(!create.contains(
        "${{ runner.temp }}/rmux-tag-input/candidate-reference.json\n            ${{ runner.temp }}/signed-tag-proof.json"
    ));
}

#[test]
fn signed_tag_gate_accepts_rfc3339_fractional_seconds() {
    for variable in ["RMUX_MANIFEST_CREATED_AT", "RMUX_MANIFEST_EXPIRES_AT"] {
        let check = format!(
            r#"[[ "${variable}" =~ ^[0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}}T[0-9]{{2}}:[0-9]{{2}}:[0-9]{{2}}(\.[0-9]{{1,9}})?Z$ ]]"#
        );
        assert!(TAG.contains(&check), "missing fractional timestamp gate");
    }
}

#[test]
fn promotion_splits_oidc_from_contents_write_and_keeps_exact_dag() {
    let verify = job(PROMOTE, "verify-candidate", Some("prepare-policy-audit"));
    let prepare = job(PROMOTE, "prepare-policy-audit", Some("policy-audit"));
    let audit = job(PROMOTE, "policy-audit", Some("authorize-promotion"));
    let authorize = job(PROMOTE, "authorize-promotion", Some("publish"));
    let publish = job(PROMOTE, "publish", Some("dispatch-receipt"));
    assert!(verify.contains("verify-candidate-artifacts.py verify-downloaded"));
    assert!(verify.contains("verify-candidate-attestations.sh"));
    assert!(verify.contains("GH_TOKEN: ${{ github.token }}"));
    assert!(verify.contains("git/ref/tags/$RMUX_RELEASE_REF"));
    assert!(verify.contains("verify-release-tag.py github-json"));
    assert!(verify.contains("differs byte-for-byte from live tag"));
    assert!(verify.contains("stage-candidate-release.py"));
    assert!(CANDIDATE_STAGING.contains("item.get(\"role\") not in PUBLIC_ASSET_ROLES"));
    assert!(CANDIDATE_STAGING.contains("args.output / \"SHA256SUMS\""));
    assert_eq!(PROMOTE.matches("merge-multiple: true").count(), 4);
    assert!(authorize.contains("rmux-authorization/verified"));
    assert!(authorize.contains("rmux-authorization/policy"));
    assert!(!prepare.contains("if: ${{ false }}"));
    assert!(prepare.contains("permissions:\n      contents: read"));
    assert!(prepare.contains("uses: ./.github/workflows/release-policy-audit.yml"));
    assert!(!audit.contains("if: ${{ false }}"));
    assert!(audit.contains("environment: release-policy-audit"));
    assert!(audit.contains("uses: ./.github/actions/release-policy-audit"));
    assert!(audit.contains("audit-workflow-id: 316435346"));
    assert!(audit.contains("audit-workflow-path: .github/workflows/release-promote.yml"));
    assert!(authorize.contains("needs: [verify-candidate, policy-audit]"));
    assert!(authorize.contains("id-token: write"));
    assert!(authorize.contains("attestations: write"));
    assert!(authorize.contains("RMUX_AUTHORIZATION_WORKFLOW_ID: \"316435346\""));
    assert!(authorize.contains("[[ ! \"$RMUX_AUTHORIZATION_WORKFLOW_ID\" =~ ^[1-9][0-9]*$ ]]"));
    assert!(authorize.contains("--authorization-workflow-id \"$RMUX_AUTHORIZATION_WORKFLOW_ID\""));
    assert!(!authorize.contains("contents: write"));
    assert!(publish.contains("needs: [verify-candidate, policy-audit, authorize-promotion]"));
    assert!(publish.contains("contents: write"));
    assert!(!publish.contains("id-token: write"));
    assert!(publish.contains("uses: ./.github/workflows/release-github-publish.yml"));
    assert!(GITHUB_PUBLISH.contains("environment: release-publication"));
    assert!(GITHUB_PUBLISH.contains("publish-github-release.py"));
    assert!(GITHUB_PUBLISH.contains("SHA256SUMS.sigstore.json"));
    assert!(PUBLICATION_INPUTS.contains("actions-artifact.py verify"));
    assert!(GITHUB_PUBLISH.contains("uses: ./.github/actions/release-publication-inputs"));
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
    assert!(GITHUB_PUBLISH.contains("--execute --token \"$GH_TOKEN\""));
    assert!(!PROMOTE.contains("gh release"));
    assert!(!PROMOTE.contains("--clobber"));
}

#[test]
fn receipt_is_separate_receipt_only_and_never_writes_contents() {
    let receipt = job(RECEIPT, "receipt-only", Some("downstream"));
    assert!(!receipt.contains("if: ${{ false }}"));
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
    assert!(RECEIPT.contains("not isinstance(target, dict)"));
    assert!(RECEIPT.contains("target.get(\"type\") != \"commit\""));
    assert!(RECEIPT.contains("target.get(\"sha\") != os.environ[\"RMUX_EXPECTED_SOURCE_SHA\"]"));
    assert!(!RECEIPT.contains("target != {\"type\": \"commit\""));
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
fn release_verification_docs_match_current_and_legacy_bundle_types() {
    assert!(SECURITY.contains("For `v0.9.1` and later"));
    assert!(SECURITY.contains("cosign verify-blob-attestation"));
    assert!(SECURITY.contains("release-promote\\.yml@refs/tags/"));
    assert!(
        SECURITY.contains("--type https://rmux.io/attestations/release-promotion-authorization/v1")
    );
    assert!(SECURITY.contains("For releases from `v0.6.5` through `v0.9.0`"));
    assert!(SECURITY.contains("cosign verify-blob \\"));
    assert!(SECURITY.contains("release\\.yml@refs/tags/"));
}

#[test]
fn dispatch_chain_binds_main_to_the_exact_signed_tag() {
    let tag_dispatch = job(TAG, "dispatch-promoter", None);
    assert!(tag_dispatch.contains("assert-release-capability.py promotion_authorization"));
    assert!(tag_dispatch.contains("actions/workflows/release-promote.yml/dispatches"));
    assert!(tag_dispatch.contains("\"ref\": inputs[\"release_ref\"]"));
    assert!(tag_dispatch.contains("\"workflow_id\": 316435346"));
    assert!(tag_dispatch.contains("\"head_branch\": os.environ[\"RMUX_RELEASE_REF\"]"));
    assert!(tag_dispatch.contains("\"head_sha\": os.environ[\"RMUX_EXPECTED_SOURCE_SHA\"]"));

    assert!(PROMOTE.contains("test \"$GITHUB_REF\" = \"refs/tags/$RMUX_RELEASE_REF\""));
    assert!(PROMOTE.contains("test \"$GITHUB_SHA\" = \"$RMUX_EXPECTED_SOURCE_SHA\""));
    assert!(RECEIPT_DISPATCH.contains("assert-release-capability.py publication_receipt"));
    assert!(RECEIPT_DISPATCH.contains("actions/workflows/release-receipt.yml/dispatches"));
    assert!(RECEIPT_DISPATCH.contains("\"workflow_id\": 316435347"));
    assert!(RECEIPT_DISPATCH.contains("\"head_branch\": os.environ[\"RMUX_RELEASE_REF\"]"));
    assert!(RECEIPT_DISPATCH.contains("\"head_sha\": os.environ[\"RMUX_EXPECTED_SOURCE_SHA\"]"));
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
fn only_promoter_and_nonpublishing_simulation_call_policy_audit() {
    let workflows = repo_root().join(".github/workflows");
    let mut callers = Vec::new();
    for entry in fs::read_dir(workflows).expect("list workflows") {
        let path = entry.expect("workflow entry").path();
        let text = fs::read_to_string(&path).expect("read workflow");
        if text.contains("uses: ./.github/workflows/release-policy-audit.yml") {
            callers.push(path);
        }
    }
    callers.sort();
    assert_eq!(
        callers,
        vec![
            repo_root().join(".github/workflows/release-promote.yml"),
            repo_root().join(".github/workflows/release-promotion-simulation.yml"),
        ]
    );
    let audit = job(PROMOTE, "policy-audit", Some("authorize-promotion"));
    assert!(!audit.contains("if: ${{ false }}"));
}

#[test]
fn promotion_simulation_pins_a_draft_2020_schema_validator() {
    let verify = job(SIMULATION, "verify-simulation", None);
    assert!(verify.contains("sys.version_info.minor"));
    assert!(verify.contains(")')\" = 3.10"));
    assert!(verify.contains("python3 -m venv \"$validator\""));
    assert!(verify.contains("--no-deps"));
    assert!(verify.contains("--only-binary=:all:"));
    assert!(verify.contains("--require-hashes"));
    assert!(verify.contains("schema-validator-requirements.txt"));
    assert!(verify.contains("Draft202012Validator"));
    assert!(verify.contains(">> \"$GITHUB_PATH\""));

    let requirements = SCHEMA_VALIDATOR_REQUIREMENTS
        .lines()
        .map(|line| {
            let (requirement, hash) = line
                .split_once(" --hash=sha256:")
                .expect("requirement hash");
            assert_eq!(hash.len(), 64);
            assert!(hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
            requirement
        })
        .collect::<Vec<_>>();
    assert_eq!(
        requirements,
        [
            "attrs==26.1.0",
            "jsonschema==4.26.0",
            "jsonschema-specifications==2025.9.1",
            "referencing==0.37.0",
            "rpds-py==0.30.0",
            "typing-extensions==4.16.0",
        ]
    );
}

#[test]
fn promotion_simulation_reports_only_the_work_it_executes() {
    assert!(SIMULATION.contains("python3 scripts/release/promotion-simulation.py"));
    assert!(!SIMULATION.lines().any(|line| line
        .trim_start()
        .starts_with("scripts/release/promotion-simulation.py")));
    assert!(SIMULATION.contains("Resolve the eleven original candidate artifact IDs"));
    assert!(SIMULATION.contains("verify-candidate-attestations.sh"));
    assert!(SIMULATION.contains("\"exact_candidate_bytes_exercised\": True"));
    assert!(SIMULATION.contains("\"policy_audit_exercised\": True"));
    assert!(SIMULATION.contains("\"promotion_authorization_exercised\": True"));
    assert!(SIMULATION.contains("\"promotion_workflow_exercised\": False"));
    assert!(SIMULATION.contains("\"github_publication_plan_exercised\": True"));
    assert!(SIMULATION.contains("\"receipt_recovery_exercised\": True"));
    assert!(SIMULATION.contains("\"receipt_workflow_exercised\": False"));
    assert!(SIMULATION.contains("\"cryptographic_tag_signature_exercised\": False"));
    assert!(SIMULATION.contains("\"oidc_attestations_exercised\": False"));
    assert!(!SIMULATION.contains("--execute"));
    assert!(PROMOTION_SIMULATION.contains("expired_at - timedelta(minutes=5)"));
    assert!(PROMOTION_SIMULATION.contains("policy audit expired before authorization"));
    assert!(!PROMOTION_SIMULATION
        .contains("expired_audit[\"expires_at\"] = expired_audit[\"emitted_at\"]"));
}

#[test]
fn promotion_simulation_uses_a_portable_unique_artifact_name() {
    assert!(SIMULATION.contains("name: rmux-promotion-simulation-${{ github.run_id }}"));
    assert!(!SIMULATION.contains("name: rmux-promotion-simulation-${{ inputs.simulation_id }}"));
}

#[test]
fn promotion_simulation_and_publisher_stay_below_the_release_file_budget() {
    assert!(PROMOTION_SIMULATION.lines().count() < 600);
    let publisher = include_str!("../scripts/release/publish-github-release.py");
    assert!(publisher.lines().count() < 600);
}

#[test]
fn candidate_attestation_gate_checks_symlinks_before_resolution() {
    let verifier = include_str!("../scripts/release/verify-candidate-attestations.sh");
    let symlink_check = verifier
        .find("if candidate.is_symlink():")
        .expect("symlink check");
    let resolution = verifier
        .find("path = candidate.resolve(strict=True)")
        .expect("candidate resolution");
    assert!(symlink_check < resolution);
}
