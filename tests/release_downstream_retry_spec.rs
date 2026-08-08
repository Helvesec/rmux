#[path = "support/python3.rs"]
mod python3;

use std::fs;
use std::path::PathBuf;
use std::process::Output;
use std::time::{SystemTime, UNIX_EPOCH};

const ACTIVATION: &str = include_str!("../.github/release/release-activation.json");
const CHOCOLATEY: &str = include_str!("../.github/workflows/release-chocolatey-retry.yml");
const CHOCOLATEY_STATUS: &str = include_str!("../scripts/release/chocolatey-package-status.py");
const CHOCOLATEY_WRITER: &str = include_str!("../scripts/release/publish-chocolatey-package.ps1");
const DISPATCH: &str = include_str!("../.github/workflows/release-channel-retry.yml");
const PREPARE: &str = include_str!("../.github/actions/release-channel-retry-prepare/action.yml");
const RETRY_HELPER: &str = include_str!("../scripts/release/prepare-channel-retry.py");
const SNAP: &str = include_str!("../.github/workflows/release-snap-retry.yml");
const SNAP_WRITER: &str =
    include_str!("../.github/actions/release-snap-candidate-write/action.yml");

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rmux-chocolatey-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create Chocolatey fixture directory");
    path
}

fn chocolatey_entry(version: &str, approved: &str, published: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<entry xmlns="http://www.w3.org/2005/Atom"
       xmlns:d="http://schemas.microsoft.com/ado/2007/08/dataservices">
  <content>
    <d:Version>{version}</d:Version>
    <d:IsApproved>{approved}</d:IsApproved>
    <d:Published>{published}</d:Published>
  </content>
</entry>
"#
    )
}

fn chocolatey_status(document: &PathBuf, expected_version: &str) -> Output {
    python3::command()
        .args(["scripts/release/chocolatey-package-status.py", "--document"])
        .arg(document)
        .args(["--expected-version", expected_version])
        .current_dir(repo_root())
        .output()
        .expect("run Chocolatey metadata classifier")
}

fn assert_reusable_only(workflow: &str) {
    assert!(workflow.contains("on:\n  workflow_call:"));
    assert_eq!(workflow.matches("permissions: {}").count(), 1);
    for forbidden in [
        "\n  workflow_dispatch:",
        "\n  repository_dispatch:",
        "\n  push:",
        "\n  schedule:",
        "runs-on: self-hosted",
        "- self-hosted",
        "larger-runner",
    ] {
        assert!(!workflow.contains(forbidden), "retry gained {forbidden}");
    }
}

fn workflow_calls(text: &str, name: &str) -> usize {
    text.lines()
        .map(str::trim)
        .filter(|line| {
            line.strip_prefix("uses:")
                .map(str::trim)
                .is_some_and(|target| target == format!("./.github/workflows/{name}"))
        })
        .count()
}

fn job_block<'a>(workflow: &'a str, job: &str, next_job: Option<&str>) -> &'a str {
    let start_marker = format!("\n  {job}:\n");
    let start = workflow.find(&start_marker).expect("retry job boundary");
    let end = next_job
        .map(|name| {
            workflow[start + start_marker.len()..]
                .find(&format!("\n  {name}:\n"))
                .map(|offset| start + start_marker.len() + offset)
                .expect("next retry job boundary")
        })
        .unwrap_or(workflow.len());
    &workflow[start..end]
}

#[test]
fn retry_entry_point_is_dispatch_only_and_ledger_gated() {
    assert!(DISPATCH.contains("on:\n  workflow_dispatch:"));
    assert!(!DISPATCH.contains("\n  workflow_call:"));
    assert!(!DISPATCH.contains("\n  push:"));
    assert_eq!(DISPATCH.matches("permissions: {}").count(), 1);
    assert_eq!(workflow_calls(DISPATCH, "release-chocolatey-retry.yml"), 1);
    assert_eq!(workflow_calls(DISPATCH, "release-snap-retry.yml"), 1);
    assert!(!DISPATCH.contains("secrets: inherit"));
    assert!(DISPATCH.contains("inputs.channel == 'chocolatey'"));
    assert!(DISPATCH.contains("inputs.channel == 'snap_candidate'"));

    let activation: serde_json::Value =
        serde_json::from_str(ACTIVATION).expect("activation ledger");
    assert_eq!(activation["status"], "active");
    assert_eq!(activation["capabilities"]["downstream_channels"], true);

    let contract: serde_json::Value = serde_json::from_str(include_str!(
        "../.github/release/downstream-channel-contract.json"
    ))
    .expect("downstream contract");
    let retryable: Vec<_> = contract["channels"]
        .as_array()
        .expect("channels")
        .iter()
        .filter(|channel| channel["retryable"] == true)
        .map(|channel| channel["name"].as_str().expect("channel name"))
        .collect();
    assert_eq!(retryable, ["chocolatey", "snap_candidate"]);

    let workflows = repo_root().join(".github/workflows");
    for entry in fs::read_dir(workflows).expect("list workflows") {
        let path = entry.expect("workflow entry").path();
        let text = fs::read_to_string(&path).expect("read workflow");
        let calls = workflow_calls(&text, "release-chocolatey-retry.yml")
            + workflow_calls(&text, "release-snap-retry.yml");
        if path.ends_with("release-channel-retry.yml") {
            assert_eq!(calls, 2);
        } else {
            assert_eq!(calls, 0, "unexpected retry caller {}", path.display());
        }
    }
}

#[test]
fn retry_wrappers_use_one_common_exact_evidence_preparer() {
    for (workflow, channel) in [(CHOCOLATEY, "chocolatey"), (SNAP, "snap_candidate")] {
        assert_reusable_only(workflow);
        assert_eq!(
            workflow
                .matches("uses: ./.github/actions/release-channel-retry-prepare")
                .count(),
            1
        );
        assert!(workflow.contains(&format!("channel: {channel}")));
        assert!(workflow.contains("environment: release"));
        assert!(workflow.contains("assert-release-capability.py downstream_channels"));
        assert!(workflow.contains("prepare-channel-retry.py validate-identity-inputs --from-env"));
        assert!(workflow.contains("prepare-channel-retry.py verify-prepared --from-env"));
        assert!(
            workflow
                .find("prepare-channel-retry.py validate-identity-inputs --from-env")
                .expect("retry input validator")
                < workflow
                    .find("actions-artifact.py verify")
                    .expect("first retry input consumer")
        );
        assert!(workflow.contains("uses: ./.github/actions/release-channel-result"));
        assert!(!workflow.contains("if: ${{ false }}"));
        assert!(!workflow.contains("cargo build"));
        assert!(!workflow.contains("cargo package"));
        assert!(!workflow.contains("snapcore/action-build"));
    }

    assert!(CHOCOLATEY.contains("producer-workflow-id: \"316439352\""));
    assert!(CHOCOLATEY
        .contains("producer-workflow-path: .github/workflows/release-chocolatey-retry.yml"));
    assert!(SNAP.contains("producer-workflow-id: \"316439354\""));
    assert!(SNAP.contains("producer-workflow-path: .github/workflows/release-snap-retry.yml"));
    assert!(SNAP.contains("uses: ./.github/actions/release-snap-candidate-write"));
    assert!(SNAP_WRITER.contains("release: latest/candidate"));
    assert!(!SNAP_WRITER.contains("latest/stable"));
}

#[test]
fn prepare_loads_its_validator_from_the_immutable_workflow_revision() {
    for (name, workflow) in [("Chocolatey", CHOCOLATEY), ("Snap", SNAP)] {
        let prepare = job_block(workflow, "prepare", Some("retry"));
        let trusted_checkout = prepare
            .find("ref: ${{ github.workflow_sha }}")
            .unwrap_or_else(|| panic!("{name} prepare checkout is not workflow-anchored"));
        let local_validator = prepare
            .find("uses: ./.github/actions/release-channel-retry-prepare")
            .unwrap_or_else(|| panic!("{name} prepare validator action is missing"));

        assert!(
            trusted_checkout < local_validator,
            "{name} validator must be loaded after the immutable workflow checkout"
        );
        assert!(
            !prepare.contains("ref: ${{ inputs.expected_source_sha }}"),
            "{name} prepare must not checkout an input-selected revision"
        );

        let retry = job_block(workflow, "retry", None);
        let validated_source_checkout = retry
            .find("ref: ${{ inputs.expected_source_sha }}")
            .unwrap_or_else(|| panic!("{name} retry source checkout is missing"));
        assert!(
            retry
                .find("needs: prepare")
                .is_some_and(|boundary| boundary < validated_source_checkout),
            "{name} source checkout must remain behind the prepare boundary"
        );
    }
}

#[test]
fn common_retry_preparer_binds_receipt_result_attestation_and_original_bytes() {
    assert_eq!(PREPARE.matches("actions-artifact.py verify").count(), 5);
    assert_eq!(PREPARE.matches("actions/download-artifact@").count(), 5);
    assert_eq!(
        PREPARE
            .matches("--expected-workflow-path .github/workflows/release-receipt.yml")
            .count(),
        2
    );
    assert!(!PREPARE.contains("release-promote.yml"));
    assert!(PREPARE.contains("verify-receipt-attestation.py"));
    assert!(PREPARE.contains("verify-channel-result-attestation.py"));
    assert!(PREPARE.contains("install-gh-2.93.0.sh"));
    assert!(PREPARE.contains("--include-retention"));
    assert!(PREPARE.contains("artifact-ids: ${{ steps.payload.outputs.artifact_id }}"));
    assert!(PREPARE.contains("prepare-channel-retry.py validate-prepare-inputs --from-env"));
    assert!(PREPARE.contains("prepare-channel-retry.py prepare --from-env"));
    assert!(
        PREPARE
            .find("prepare-channel-retry.py validate-prepare-inputs --from-env")
            .expect("prepare input validator")
            < PREPARE
                .find("[[ \"$RMUX_CHANNEL\" =~")
                .expect("first prepare input consumer")
    );
    assert!(PREPARE.contains("test \"$RMUX_RECEIPT_RUN_ID\" = \"$RMUX_PRIOR_RESULT_RUN_ID\""));
    assert!(PREPARE
        .contains("test \"$RMUX_PRIOR_RESULT_RUN_WORKFLOW_ID\" = \"$RMUX_RECEIPT_WORKFLOW_ID\""));
    assert!(PREPARE.contains(
        "test \"$RMUX_PRIOR_RESULT_PRODUCER_WORKFLOW_ID\" = \"$RMUX_RECEIPT_WORKFLOW_ID\""
    ));
    assert!(!PREPARE.contains("cargo build"));
    assert!(!PREPARE.contains("cargo package"));
}

#[test]
fn retry_helper_enforces_single_depth_no_mutation_and_exact_file_sets() {
    for invariant in [
        "validate_retryable_previous(result)",
        "validate_retryable_previous(previous)",
        "operation=\"retry\"",
        "retry payload bytes differ",
        "prepared retry bundle",
        "original-request.json",
        "previous-result.json",
        "payload-artifact-live.json",
        "exact retry payload has expired",
        "expected_request(",
    ] {
        assert!(
            RETRY_HELPER.contains(invariant),
            "retry helper lost {invariant}"
        );
    }
    assert!(!RETRY_HELPER.contains("subprocess"));
    assert!(!RETRY_HELPER.contains("cargo"));
}

#[test]
fn retry_helper_imports_the_shared_request_model() {
    let status = python3::command()
        .args(["scripts/release/prepare-channel-retry.py", "--help"])
        .status()
        .expect("run Python import probe");
    assert!(
        status.success(),
        "retry helper must be importable at runtime"
    );
}

#[test]
fn retries_share_channel_concurrency_and_only_use_store_mutations() {
    assert!(CHOCOLATEY.contains("group: rmux-channel-chocolatey-${{ inputs.release_ref }}"));
    assert!(SNAP.contains("group: rmux-channel-snap-candidate-${{ inputs.release_ref }}"));
    assert!(CHOCOLATEY.contains("publish-chocolatey-package.ps1"));
    assert!(CHOCOLATEY_WRITER.contains("$state = \"failed-transient\""));
    assert!(CHOCOLATEY_WRITER.contains("$state = \"failed-terminal\""));
    assert!(CHOCOLATEY_WRITER.contains("$state = \"pending-moderation\""));
    assert!(CHOCOLATEY_WRITER.contains("ChocolateyPublicBytesMismatchException"));
    assert!(CHOCOLATEY_WRITER.contains("chocolatey-package-status.py"));
    assert!(CHOCOLATEY_WRITER.contains("Packages(Id='rmux',Version='$version')"));
    assert!(CHOCOLATEY_STATUS.contains("IsApproved"));
    assert!(
        CHOCOLATEY_WRITER
            .find("Invoke-WebRequest -Uri $metadataUrl")
            .expect("metadata lookup")
            < CHOCOLATEY_WRITER
                .find("choco push $package")
                .expect("package push")
    );
    assert!(
        SNAP_WRITER.contains("snapcore/action-publish@214b86e5ca036ead1668c79afb81e550e6c54d40")
    );
    assert_eq!(SNAP_WRITER.matches("snapcore/action-publish@").count(), 2);
    assert!(SNAP_WRITER.contains("state=failed-transient"));
    assert!(SNAP_WRITER.contains("state=failed-terminal"));
    for workflow in [CHOCOLATEY, SNAP] {
        assert!(workflow.contains("cancel-in-progress: false"));
        assert!(workflow.contains("persist-credentials: false"));
        assert!(workflow.contains("attestations: write"));
        assert!(workflow.contains("id-token: write"));
    }
}

#[test]
fn chocolatey_metadata_distinguishes_pending_from_public_packages() {
    let root = temp_dir("metadata");
    let pending = root.join("pending.xml");
    let public = root.join("public.xml");
    fs::write(
        &pending,
        chocolatey_entry("0.10.0", "false", "1900-01-01T00:00:00"),
    )
    .expect("write pending Chocolatey fixture");
    fs::write(
        &public,
        chocolatey_entry("0.10.0", "true", "2026-08-08T12:34:56.789"),
    )
    .expect("write public Chocolatey fixture");

    let pending_output = chocolatey_status(&pending, "0.10.0");
    assert!(pending_output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&pending_output.stdout).trim(),
        "pending"
    );
    let public_output = chocolatey_status(&public, "0.10.0");
    assert!(public_output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&public_output.stdout).trim(),
        "public"
    );

    for (name, document, expected_version) in [
        (
            "mismatched-version",
            chocolatey_entry("0.9.1", "true", "2026-08-08T12:34:56Z"),
            "0.10.0",
        ),
        (
            "approved-without-publication",
            chocolatey_entry("0.10.0", "true", "1900-01-01T00:00:00"),
            "0.10.0",
        ),
        (
            "unapproved-with-publication",
            chocolatey_entry("0.10.0", "false", "2026-08-08T12:34:56Z"),
            "0.10.0",
        ),
        (
            "duplicate-publication",
            chocolatey_entry("0.10.0", "true", "2026-08-08T12:34:56Z").replace(
                "</content>",
                "<d:Published>2026-08-08T12:34:56Z</d:Published></content>",
            ),
            "0.10.0",
        ),
        (
            "external-entity",
            "<!DOCTYPE entry [<!ENTITY version SYSTEM 'file:///etc/passwd'>]><entry />".to_owned(),
            "0.10.0",
        ),
    ] {
        let path = root.join(format!("{name}.xml"));
        fs::write(&path, document).expect("write rejected Chocolatey fixture");
        assert!(
            !chocolatey_status(&path, expected_version).status.success(),
            "invalid Chocolatey metadata accepted: {name}"
        );
    }

    fs::remove_dir_all(root).expect("remove Chocolatey fixture directory");
}

#[test]
fn retry_result_producer_requires_the_exact_workflow_id_path_pair() {
    let script = r#"
import sys
sys.path.insert(0, 'scripts/release')
from downstream_result import validate_producer

producer = {
    'run_id': 9,
    'run_attempt': 1,
    'workflow_id': 316439352,
    'workflow_path': '.github/workflows/release-chocolatey-retry.yml',
    'runner_group_id': 0,
    'runner_group_name': 'GitHub Actions',
    'runner_image': 'windows-latest',
}
validate_producer(producer, 'chocolatey')
for field, value in (
    ('workflow_id', 316435347),
    ('workflow_path', '.github/workflows/release-snap-retry.yml'),
):
    forged = dict(producer)
    forged[field] = value
    try:
        validate_producer(forged, 'chocolatey')
    except ValueError:
        continue
    raise SystemExit(f'forged retry producer accepted: {field}')
"#;
    let output = python3::command()
        .arg("-c")
        .arg(script)
        .current_dir(repo_root())
        .output()
        .expect("run retry producer fixture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
