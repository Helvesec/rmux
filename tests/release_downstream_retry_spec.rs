use std::fs;
use std::path::PathBuf;

const DOWNSTREAM: &str = include_str!("../.github/workflows/release-downstream.yml");
const CHOCOLATEY: &str = include_str!("../.github/workflows/release-chocolatey-retry.yml");
const SNAP: &str = include_str!("../.github/workflows/release-snap-retry.yml");

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

fn assert_reusable_only(workflow: &str) {
    assert!(workflow.contains("on:\n  workflow_call:"));
    assert_eq!(workflow.matches("permissions: {}").count(), 1);
    for forbidden in [
        "\n  workflow_dispatch:",
        "\n  repository_dispatch:",
        "\n  push:",
        "\n  schedule:",
    ] {
        assert!(!workflow.contains(forbidden));
    }
    assert!(!workflow.contains("contents: write"));
    assert!(!workflow.contains("id-token: write"));
    assert!(!workflow.contains("attestations: write"));
    assert!(!workflow.contains("secrets:"));
    assert!(!workflow.contains("environment:"));
    assert!(!workflow.contains("runs-on: self-hosted"));
    assert!(!workflow.contains("- self-hosted"));
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
fn retry_workflows_are_uncalled_with_read_only_prepare_and_false_writer() {
    for (workflow, writer_id) in [
        (CHOCOLATEY, "retry-chocolatey"),
        (SNAP, "retry-snap-candidate"),
    ] {
        assert_reusable_only(workflow);
        assert_eq!(workflow.matches("if: ${{ false }}").count(), 1);
        let prepare = job(workflow, "prepare-retry", Some(writer_id));
        let writer = job(workflow, writer_id, None);
        assert!(!prepare.contains("if: ${{ false }}"));
        assert!(prepare.contains("test \"$GITHUB_REPOSITORY\" = \"Helvesec/rmux\""));
        assert!(prepare.contains("test \"$GITHUB_REPOSITORY_ID\" = \"1239918790\""));
        assert!(writer.contains("if: ${{ false }}"));
        assert!(prepare.contains("GITHUB_RUN_ATTEMPT"));
        assert!(prepare.contains("actions: read"));
        assert!(prepare.contains("contents: read"));
        assert!(writer.contains("assert-release-capability.py downstream_channels"));
        assert!(workflow.contains("cancel-in-progress: false"));
    }

    let workflows = repo_root().join(".github/workflows");
    for entry in fs::read_dir(workflows).expect("list workflows") {
        let path = entry.expect("workflow entry").path();
        let text = fs::read_to_string(&path).expect("read workflow");
        for workflow_name in ["release-chocolatey-retry.yml", "release-snap-retry.yml"] {
            assert!(!text
                .lines()
                .any(|line| calls_disarmed_workflow(line, workflow_name)));
        }
    }
}

#[test]
fn retry_caller_guard_rejects_relative_and_absolute_targets() {
    for workflow_name in ["release-chocolatey-retry.yml", "release-snap-retry.yml"] {
        for target in [
            format!("    uses: ./.github/workflows/{workflow_name}"),
            format!(
                "    uses: Helvesec/rmux/.github/workflows/{workflow_name}@0123456789012345678901234567890123456789"
            ),
        ] {
            assert!(calls_disarmed_workflow(&target, workflow_name));
        }
    }
}

#[test]
fn retries_bind_exact_receipt_result_origin_and_closed_bundle() {
    for (workflow, writer_id, channel) in [
        (CHOCOLATEY, "retry-chocolatey", "chocolatey"),
        (SNAP, "retry-snap-candidate", "snap_candidate"),
    ] {
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
            "prior_result_run_id:",
            "prior_result_run_workflow_id:",
            "prior_result_producer_workflow_id:",
            "prior_result_producer_workflow_path:",
            "prior_result_artifact_id:",
            "prior_result_artifact_digest:",
            "prior_result_predicate_sha256:",
            "prior_result_envelope_artifact_id:",
            "prior_result_envelope_artifact_digest:",
            "prior_result_envelope_sha256:",
            "request_idempotency_key:",
        ] {
            assert!(workflow.contains(input), "missing retry input {input}");
        }
        let prepare = job(workflow, "prepare-retry", Some(writer_id));
        assert_eq!(prepare.matches("actions-artifact.py verify").count(), 5);
        assert_eq!(prepare.matches("artifact-ids:").count(), 4);
        assert_eq!(
            prepare
                .matches("--expected-workflow-path .github/workflows/release-receipt.yml")
                .count(),
            2
        );
        assert_eq!(
            prepare
                .matches("--expected-workflow-path .github/workflows/release-promote.yml")
                .count(),
            2
        );
        for artifact_name in [
            "rmux-publication-receipt-$RMUX_EXPECTED_SOURCE_SHA-$RMUX_RELEASE_ID".to_owned(),
            "rmux-publication-receipt-envelope-$RMUX_EXPECTED_SOURCE_SHA-$RMUX_RELEASE_ID"
                .to_owned(),
        ] {
            assert!(artifact_verification(prepare, &artifact_name)
                .contains("--expected-workflow-path .github/workflows/release-receipt.yml"));
        }
        for artifact_name in [
            format!(
                "rmux-downstream-{channel}-result-$RMUX_EXPECTED_SOURCE_SHA-$RMUX_RELEASE_ID"
            ),
            format!(
                "rmux-downstream-{channel}-result-envelope-$RMUX_EXPECTED_SOURCE_SHA-$RMUX_RELEASE_ID"
            ),
        ] {
            assert!(artifact_verification(prepare, &artifact_name)
                .contains("--expected-workflow-path .github/workflows/release-promote.yml"));
        }
        assert!(prepare
            .contains("test \"$RMUX_RECEIPT_RUN_WORKFLOW_ID\" = \"$RMUX_RECEIPT_WORKFLOW_ID\""));
        assert_eq!(
            prepare
                .matches("--expected-event workflow_dispatch")
                .count(),
            4
        );
        assert_eq!(prepare.matches("--max-attempts 1").count(), 5);
        assert!(!prepare.contains("pattern:"));
        assert_eq!(prepare.matches("merge-multiple: true").count(), 4);
        for exact_file in [
            "channel-payload.json",
            "downstream-channel-plan.json",
            "downstream-channel-request.json",
            "downstream-channel-result-predicate.json",
            "downstream-channel-result.sigstore.json",
            "downstream-channel-target-evidence.json",
            "release-state.json",
            "receipt-reference.json",
            "downstream-channel-result-envelope.json",
        ] {
            assert!(prepare.contains(exact_file), "bundle lost {exact_file}");
        }
        assert!(prepare.contains("names != wanted"));
        assert!(prepare.contains("contains a symlink"));
        assert!(prepare.contains("channel-policy.py verify-plan"));
        assert!(prepare.contains("channel-request.py verify"));
        assert!(prepare.contains("channel-request.py create"));
        assert!(prepare.contains("channel-result.py verify-predicate"));
        assert!(prepare.contains("result_started_at=\"$(jq -er .started_at"));
        assert!(prepare.contains("--started-at \"$result_started_at\""));
        assert!(prepare.contains("channel-result.py verify-envelope"));
        assert_eq!(
            prepare
                .matches("--request \"$bundle/downstream-channel-request.json\"")
                .count(),
            2
        );
        assert!(prepare.contains("install-gh-2.93.0.sh"));
        assert!(prepare.contains("verify-receipt-attestation.py"));
        assert!(prepare.contains("attestation verify"));
        assert!(prepare.contains("GH_TOKEN: ${{ github.token }}"));
        assert!(prepare.contains("--deny-self-hosted-runners"));
        assert!(prepare.contains("result attestation cardinality differs"));
        assert!(prepare.contains("result attestation lacks signed verification evidence"));
        assert!(prepare.contains("verifiedTimestamps"));
        assert!(prepare.contains("signature.get(\"certificate\")"));
        assert!(prepare.contains("signed result subject differs"));
        assert!(!prepare.contains("predicate[\"subject\"]"));
        assert!(prepare.contains("hashlib.sha256(target_path.read_bytes()).hexdigest()"));
        assert!(prepare.contains("^rmux-downstream-v1:[0-9a-f]{64}$"));
        assert!(prepare.contains("expected_receipt = {"));
        assert!(prepare.contains("\"predicate_bundle\": reference[\"predicate_bundle\"]"));
        assert!(prepare.contains("\"envelope_bundle\": reference[\"envelope_bundle\"]"));
        assert!(prepare.contains("\"predicate_artifact_id\""));
        assert!(prepare.contains("\"predicate_artifact_digest\""));
        assert!(prepare.contains("\"envelope_artifact_id\""));
        assert!(prepare.contains("\"envelope_artifact_digest\""));
        assert!(prepare.contains("rebuild_native"));
        assert!(prepare.contains("exact payload has expired"));
        assert!(prepare.contains("Revalidate the exact payload artifact is still available"));
        assert!(prepare.contains(".artifact.artifact_id"));
        assert!(prepare.contains(".artifact.archive_digest"));
        assert!(prepare.contains("--include-retention"));
        assert!(prepare.contains("artifact[\"created_at\"]"));
        assert!(prepare.contains("artifact[\"updated_at\"]"));
        assert!(prepare.contains("artifact[\"expires_at\"]"));
        assert!(prepare.contains("live payload artifact identity differs"));
        assert!(prepare.contains("request[\"retry_depth\"] != 0"));
        assert!(prepare.contains("result[\"state\"] not in {\"prepared\", \"failed-transient\"}"));
        assert!(prepare.contains("result[\"mutation_started\"] is not False"));
        assert!(prepare.contains("result[\"remote_request_id\"] is not None"));
        assert!(prepare.contains("result[\"started_at\"]"));
        assert!(prepare.contains("request[\"expires_at\"]"));
        assert!(prepare.contains("prior result is not safe for one exact retry"));
        assert!(prepare.contains("prior result started outside the original request TTL"));
        for previous_field in [
            "\"request_sha256\": result[\"request_sha256\"]",
            "\"state\": result[\"state\"]",
            "\"mutation_started\": result[\"mutation_started\"]",
            "\"remote_request_id\": result[\"remote_request_id\"]",
        ] {
            assert!(prepare.contains(previous_field));
        }
        assert_eq!(
            prepare
                .matches(".github/workflows/release-downstream.yml")
                .count(),
            1
        );
    }
    assert!(!CHOCOLATEY.contains(".github/workflows/release-chocolatey-retry.yml"));
    assert!(!SNAP.contains(".github/workflows/release-snap-retry.yml"));
    assert!(CHOCOLATEY.contains("snap_candidate_opt_in=\"$(jq -er"));
    assert!(CHOCOLATEY.contains("plan_args+=(--snap-candidate-opt-in)"));
    assert!(CHOCOLATEY.contains("if [[ \"$snap_candidate_opt_in\" == true ]]"));
}

#[test]
fn retries_share_channel_concurrency_and_never_rebuild() {
    assert!(DOWNSTREAM.contains("group: rmux-channel-chocolatey-${{ inputs.release_ref }}"));
    assert!(CHOCOLATEY.contains("group: rmux-channel-chocolatey-${{ inputs.release_ref }}"));
    assert!(
        DOWNSTREAM.contains("group: rmux-channel-${{ matrix.channel }}-${{ inputs.release_ref }}")
    );
    assert!(SNAP.contains("group: rmux-channel-snap-candidate-${{ inputs.release_ref }}"));

    for (workflow, forbidden) in [
        (
            CHOCOLATEY,
            [
                "cargo build",
                "cargo package",
                "choco pack",
                "choco push",
                "generate-chocolatey-package.sh",
                "latest/stable",
            ],
        ),
        (
            SNAP,
            [
                "cargo build",
                "cargo package",
                "snapcraft",
                "snapcore/action-build",
                "snapcore/action-publish",
                "latest/stable",
            ],
        ),
    ] {
        for primitive in forbidden {
            assert!(
                !workflow.contains(primitive),
                "retry gained forbidden rebuild or mutation primitive {primitive}"
            );
        }
    }
}

#[test]
fn snap_retry_remains_candidate_only_and_writer_disabled() {
    assert!(SNAP.contains("rmux-channel-snap-candidate-"));
    assert!(SNAP.contains("the downstream writer is disabled"));
    assert!(!SNAP.contains("snap-stable"));

    let policy: serde_json::Value =
        serde_json::from_str(include_str!("../.github/release/channel-policy.json"))
            .expect("channel policy");
    assert_eq!(
        policy["release_kinds"]["rc"]["snap_candidate"],
        "explicit_opt_in"
    );
    assert_eq!(
        policy["release_kinds"]["stable"]["snap_candidate"],
        "explicit_opt_in"
    );
    assert_eq!(policy["release_kinds"]["rc"]["snap_stable"], false);
    assert_eq!(policy["release_kinds"]["stable"]["snap_stable"], false);
}
