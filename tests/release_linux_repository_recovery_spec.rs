#![cfg(unix)]

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const AUTHORIZE: &str =
    include_str!("../.github/actions/release-linux-repository-recovery-authorize/action.yml");
const BUILD: &str = include_str!("../.github/workflows/release-linux-repository-build.yml");
const MODEL: &str = include_str!("../scripts/release/linux_repository_recovery_model.py");
const PUBLISH: &str = include_str!("../.github/workflows/release-linux-repository-publish.yml");
const RESULT: &str = include_str!("../.github/actions/release-channel-result/action.yml");
const HISTORY: &str = include_str!("../scripts/retain-linux-package-history.py");

const SOURCE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const MAIN: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const PLAN_DIGEST: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const PAYLOAD_DIGEST: &str =
    "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const SIGNED_DIGEST: &str =
    "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn temp_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "rmux-linux-recovery-{label}-{}-{sequence}",
        std::process::id()
    ));
    if path.exists() {
        fs::remove_dir_all(&path).expect("remove stale Linux recovery fixture");
    }
    fs::create_dir_all(&path).expect("create Linux recovery fixture");
    path
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn python(source: &str) -> Output {
    Command::new("python3")
        .arg("-c")
        .arg(source)
        .current_dir(repo_root())
        .output()
        .expect("run Linux recovery Python fixture")
}

fn write_json(path: &Path, value: &Value) {
    let mut bytes = serde_json::to_vec_pretty(value).expect("serialize JSON fixture");
    bytes.push(b'\n');
    fs::write(path, bytes).expect("write JSON fixture");
}

fn run_helper(root: &Path, arguments: &[&str]) -> Output {
    Command::new("python3")
        .arg(repo_root().join("scripts/release/linux_repository_recovery.py"))
        .args(arguments)
        .env("GITHUB_REPOSITORY", "Helvesec/rmux")
        .env("GITHUB_REPOSITORY_ID", "1239918790")
        .env("GITHUB_RUN_ID", "900")
        .env("GITHUB_RUN_ATTEMPT", "1")
        .env("GITHUB_SHA", MAIN)
        .env("GITHUB_REF", "refs/heads/main")
        .env("GITHUB_REF_NAME", "main")
        .env("GITHUB_EVENT_NAME", "workflow_dispatch")
        .current_dir(root)
        .output()
        .expect("run Linux repository recovery helper")
}

#[test]
fn manual_recovery_is_same_run_revalidated_and_canonically_sealed() {
    let dispatch = BUILD
        .split("\n  workflow_dispatch:\n")
        .nth(1)
        .expect("manual signer dispatch")
        .split("\npermissions: {}\n")
        .next()
        .expect("manual signer inputs");
    for input in [
        "receipt_run_id",
        "plan_artifact_id",
        "plan_artifact_digest",
        "payload_artifact_id",
        "payload_artifact_digest",
        "expected_source_sha",
        "release_id",
        "release_ref",
    ] {
        assert!(dispatch.contains(&format!("      {input}: {{required: true")));
    }
    assert!(!dispatch.contains("repository_artifact_id"));
    assert!(!dispatch.contains("repository_artifact_digest"));
    assert!(!BUILD.contains("secrets: inherit"));
    assert!(!PUBLISH.contains("secrets: inherit"));

    assert_eq!(
        BUILD
            .matches("uses: ./.github/actions/release-linux-repository-recovery-authorize")
            .count(),
        1
    );
    let build_gate = BUILD
        .find("Authorize failed-receipt recovery before constructing signed bytes")
        .expect("pre-construction recovery gate");
    let construction = BUILD
        .find("Authenticate retained history and generate signed repositories")
        .expect("signed repository construction");
    assert!(build_gate < construction);
    assert!(BUILD.contains("if: github.run_id == inputs.receipt_run_id"));
    assert!(BUILD.contains("ref: ${{ inputs.expected_source_sha }}"));
    assert!(
        BUILD.contains("repository_artifact_id: ${{ needs.build.outputs.repository_artifact_id }}")
    );
    assert!(BUILD.contains(
        "repository_artifact_digest: ${{ needs.build.outputs.repository_artifact_digest }}"
    ));
    assert!(BUILD.contains("repository_artifact_run_id: ${{ github.run_id }}"));
    assert!(BUILD.contains("repository_artifact_source_sha: ${{ github.sha }}"));
    assert!(BUILD.contains("environment: release"));
    assert!(BUILD.contains("attestations: write"));
    assert!(BUILD.contains("id-token: write"));
    assert!(BUILD.contains(
        "RMUX_DOWNSTREAM_APP_PRIVATE_KEY: ${{ secrets.RMUX_DOWNSTREAM_APP_PRIVATE_KEY }}"
    ));

    assert_eq!(
        PUBLISH
            .matches("uses: ./.github/actions/release-linux-repository-recovery-authorize")
            .count(),
        1
    );
    let bundle = PUBLISH
        .find("linux_repository_recovery.py verify-bundle")
        .expect("same-run bundle proof");
    let remove_manifest = PUBLISH
        .find("Remove verified recovery-only metadata from public repository bytes")
        .expect("recovery metadata removal");
    let revalidate = PUBLISH
        .find("Revalidate failed-receipt authority immediately before mutation")
        .expect("pre-mutation revalidation");
    let writer = PUBLISH
        .find("scripts/release/publish-linux-repository.py")
        .expect("repository writer");
    let result = PUBLISH
        .find("Seal exact manual Linux recovery result evidence")
        .expect("recovery result sealing");
    assert!(
        bundle < remove_manifest
            && remove_manifest < revalidate
            && revalidate < writer
            && writer < result
    );
    assert!(PUBLISH.contains("environment: release-publication"));
    assert!(PUBLISH.contains("producer-workflow-id: ${{ steps.signer.outputs.workflow_id }}"));
    assert!(PUBLISH
        .contains("producer-workflow-path: .github/workflows/release-linux-repository-build.yml"));
    assert!(PUBLISH.contains("producer-head-sha: ${{ steps.route.outputs.artifact_source_sha }}"));
    assert!(PUBLISH.contains("producer-source-ref: refs/heads/main"));
    assert!(PUBLISH.contains(
        "attestation-signer-workflow-path: .github/workflows/release-linux-repository-publish.yml"
    ));
    assert!(PUBLISH.contains("if: ${{ !inputs.recovery_mode }}"));

    for required in [
        "linux_repository_recovery.py verify-signer-run",
        "linux_repository_recovery.py inspect-original",
        "--allow-completed-failed-run",
        "verify-receipt-attestation.py",
    ] {
        assert!(
            AUTHORIZE.contains(required),
            "authorization lost {required}"
        );
    }
    assert!(MODEL.contains("actions/artifacts?name="));
    assert!(MODEL.contains("reject_prior_recovery_result"));
    assert!(RESULT.contains("producer-head-sha:"));
    assert!(RESULT.contains("attestation-signer-workflow-path:"));
    assert!(HISTORY.contains("already contains current release"));
}

#[test]
fn receipt_failure_single_use_and_real_producer_are_fail_closed() {
    let output = python(
        r#"
import copy
import importlib.util
import pathlib
import sys

sys.path.insert(0, 'scripts/release')
from downstream_result import validate_producer, validate_result_artifact_source
from linux_repository_recovery_model import (
    reject_prior_linux_publication,
    reject_prior_recovery_result,
    validate_failed_receipt_run,
)

spec = importlib.util.spec_from_file_location(
    'retain_linux_package_history', 'scripts/retain-linux-package-history.py'
)
history = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = history
assert spec.loader is not None
spec.loader.exec_module(history)

source = 'a' * 40
main = 'b' * 40
receipt = {
    'id': 700,
    'workflow_id': 316435347,
    'path': '.github/workflows/release-receipt.yml',
    'event': 'workflow_dispatch',
    'run_attempt': 1,
    'head_sha': source,
    'head_branch': 'v1.2.3',
    'status': 'completed',
    'conclusion': 'failure',
    'repository': {'id': 1239918790},
    'head_repository': {'id': 1239918790},
}
validate_failed_receipt_run(receipt, run_id=700, source_sha=source, release_ref='v1.2.3')
for field, forged in (
    ('path', '.github/workflows/release-linux-repository-build.yml'),
    ('head_sha', main),
    ('head_branch', 'main'),
    ('status', 'in_progress'),
    ('conclusion', 'success'),
):
    changed = copy.deepcopy(receipt)
    changed[field] = forged
    try:
        validate_failed_receipt_run(changed, run_id=700, source_sha=source, release_ref='v1.2.3')
    except ValueError:
        pass
    else:
        raise SystemExit(f'forged failed receipt {field} was accepted')

names = (
    f'rmux-downstream-apt_rpm-result-{source}-17',
    f'rmux-downstream-apt_rpm-result-envelope-{source}-17',
    f'rmux-downstream-apt_rpm-result-reference-{source}-17',
)
reject_prior_recovery_result([], source_sha=source, release_id=17)
for index, name in enumerate(names, 1):
    try:
        reject_prior_recovery_result([{'id': index, 'name': name}], source_sha=source, release_id=17)
    except ValueError as error:
        if 'already exists' not in str(error):
            raise
    else:
        raise SystemExit(f'prior recovery marker was accepted: {name}')
reject_prior_linux_publication(
    [{'id': 9, 'name': f'rmux-downstream-apt_rpm-signed-{source}-17'}],
    source_sha=source,
    release_id=17,
)
try:
    reject_prior_linux_publication(
        [{'id': 10, 'name': names[0]}], source_sha=source, release_id=17
    )
except ValueError:
    pass
else:
    raise SystemExit('result evidence in failed receipt was accepted')

producer = {
    'run_id': 900,
    'run_attempt': 1,
    'workflow_id': 424242,
    'workflow_path': '.github/workflows/release-linux-repository-build.yml',
    'runner_group_id': 0,
    'runner_group_name': 'GitHub Actions',
    'runner_image': 'ubuntu-22.04',
}
validate_producer(producer, 'apt_rpm')
validate_result_artifact_source(
    main, channel='apt_rpm', release_source_sha=source, producer=producer
)
for channel in ('chocolatey', 'snap_candidate'):
    try:
        validate_producer(producer, channel)
    except ValueError:
        pass
    else:
        raise SystemExit(f'dynamic Linux producer escaped into {channel}')
normal = {**producer, 'workflow_id': 316435347,
          'workflow_path': '.github/workflows/release-linux-repository-publish.yml'}
try:
    validate_result_artifact_source(
        main, channel='apt_rpm', release_source_sha=source, producer=normal
    )
except ValueError:
    pass
else:
    raise SystemExit('normal result accepted a main-branch artifact source')

current = history.StableVersion.parse('1.2.3')
assert current is not None
package = history.AuthenticatedPackage(current, 'amd64', pathlib.Path('/tmp/rmux.deb'))
for manager in ('APT', 'RPM'):
    try:
        history.latest_predecessor([package], current, manager)
    except history.HistoryError as error:
        if f'published {manager} repository already contains current release 1.2.3' not in str(error):
            raise
    else:
        raise SystemExit(f'{manager} republish was accepted')
"#,
    );
    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn original_failed_receipt_authorization_round_trips_end_to_end() {
    let output = python(
        r#"
import argparse
import pathlib
import sys
import tempfile

sys.path.insert(0, 'scripts/release')
from downstream_channels import CHANNEL_POLICY, CHANNELS, file_hash, write_object
from downstream_plan import expected_channel_entries
from linux_repository_recovery import inspect_original

source = 'a' * 40
release = {
    'id': 17,
    'ref': 'v1.2.3',
    'intent_id': 'intent:release-17',
    'kind': 'stable',
    'tag_object_sha': 'f' * 40,
    'immutable': True,
}

def reference_artifact(artifact_id, name, digest):
    return {
        'artifact_id': artifact_id,
        'name': name,
        'archive_digest': digest,
        'size_in_bytes': 100,
    }

predicate = reference_artifact(
    703, f'rmux-publication-receipt-{source}-17', 'sha256:' + '3' * 64
)
envelope = reference_artifact(
    704, f'rmux-publication-receipt-envelope-{source}-17', 'sha256:' + '4' * 64
)
receipt_identity = {
    'run_id': 700,
    'run_attempt': 1,
    'workflow_id': 316435347,
    'workflow_path': '.github/workflows/release-receipt.yml',
}
attestation = {
    'attestation_id': 'receipt-attestation-17',
    'bundle_file': 'publication-receipt.sigstore.json',
    'bundle_sha256': '5' * 64,
}
verified_at = '2026-07-25T00:00:05Z'
embedded_receipt = {
    **receipt_identity,
    'predicate_bundle': predicate,
    'predicate_sha256': '6' * 64,
    'envelope_bundle': envelope,
    'envelope_sha256': '7' * 64,
    'attestation': attestation,
    'verified_at': verified_at,
}
reference = {
    'schema_version': 1,
    'status': 'downstream-authorized',
    'downstream_authority': True,
    'repository_id': 1239918790,
    'source_git_sha': source,
    'release': release,
    'receipt': receipt_identity,
    'predicate_bundle': predicate,
    'predicate_sha256': '6' * 64,
    'envelope_bundle': envelope,
    'envelope_sha256': '7' * 64,
    'attestation': attestation,
    'verified_at': verified_at,
}
plan = {
    'schema_version': 1,
    'status': 'downstream-authorized',
    'downstream_authority': True,
    'execution_authority': True,
    'execution_enabled': True,
    'repository_id': 1239918790,
    'source_git_sha': source,
    'release': release,
    'receipt': embedded_receipt,
    'channel_policy': {
        'path': '.github/release/channel-policy.json',
        'schema_version': 1,
        'sha256': file_hash(CHANNEL_POLICY),
    },
    'snap_candidate_opt_in': False,
    'created_at': '2026-07-25T00:00:06Z',
    'channel_count': len(CHANNELS),
    'channels': expected_channel_entries('stable', False, authority_active=True),
}
run = {
    'id': 700,
    'workflow_id': 316435347,
    'path': '.github/workflows/release-receipt.yml',
    'event': 'workflow_dispatch',
    'run_attempt': 1,
    'head_sha': source,
    'head_branch': 'v1.2.3',
    'status': 'completed',
    'conclusion': 'failure',
    'repository': {'id': 1239918790},
    'head_repository': {'id': 1239918790},
}

def api_artifact(artifact_id, name, digest):
    return {
        'id': artifact_id,
        'name': name,
        'digest': digest,
        'expired': False,
        'size_in_bytes': 100,
        'created_at': '2026-07-25T00:00:01Z',
        'updated_at': '2026-07-25T00:00:02Z',
        'expires_at': '2026-08-01T00:00:00Z',
        'workflow_run': {
            'id': 700,
            'repository_id': 1239918790,
            'head_repository_id': 1239918790,
            'head_sha': source,
        },
    }

with tempfile.TemporaryDirectory(dir=pathlib.Path.cwd()) as temporary:
    root = pathlib.Path(temporary)
    paths = {
        'plan': root / 'downstream-channel-plan.json',
        'reference': root / 'receipt-reference.json',
        'run': root / 'run.json',
        'artifacts': root / 'artifacts.json',
        'results': root / 'results.json',
        'output': root / 'github-output',
    }
    write_object(paths['plan'], plan)
    write_object(paths['reference'], reference)
    write_object(paths['run'], run)
    write_object(paths['artifacts'], {
        'total_count': 4,
        'artifacts': [
            api_artifact(701, f'rmux-downstream-plan-{source}-17', 'sha256:' + '1' * 64),
            api_artifact(702, f'rmux-downstream-apt_rpm-payload-{source}-17', 'sha256:' + '2' * 64),
            api_artifact(703, predicate['name'], predicate['archive_digest']),
            api_artifact(704, envelope['name'], envelope['archive_digest']),
        ],
    })
    write_object(paths['results'], {'total_count': 0, 'artifacts': []})
    inspect_original(argparse.Namespace(
        source_sha=source,
        release_ref='v1.2.3',
        release_id=17,
        receipt_run_id=700,
        plan_artifact_id=701,
        plan_artifact_digest='sha256:' + '1' * 64,
        payload_artifact_id=702,
        payload_artifact_digest='sha256:' + '2' * 64,
        plan=paths['plan'],
        receipt_reference=paths['reference'],
        run_json=paths['run'],
        artifacts_json=paths['artifacts'],
        result_artifacts_json=paths['results'],
        github_output=paths['output'],
    ))
    values = dict(
        line.split('=', 1)
        for line in paths['output'].read_text(encoding='utf-8').splitlines()
    )
    if values['receipt_artifact_id'] != '703':
        raise SystemExit('predicate artifact ID was not recovered')
    if values['receipt_envelope_artifact_id'] != '704':
        raise SystemExit('envelope artifact ID was not recovered')
"#,
    );
    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn public_writer_rejects_unverified_recovery_metadata() {
    let output = python(
        r#"
import importlib.util
import hashlib
import pathlib
import sys
import tempfile

sys.path.insert(0, 'scripts/release')
spec = importlib.util.spec_from_file_location(
    'publish_linux_repository', 'scripts/release/publish-linux-repository.py'
)
publisher = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(publisher)

with tempfile.TemporaryDirectory(dir=pathlib.Path.cwd()) as temporary:
    root = pathlib.Path(temporary)
    (root / 'PACKAGE_REPOSITORY_BASE').write_text('a' * 40 + '\n', encoding='utf-8')
    required = {
        'index.html',
        '_headers',
        'debian/dists/stable/InRelease',
        'debian/dists/stable/Release',
        'debian/dists/stable/Release.gpg',
        'debian/rmux.asc',
        'rpm/repodata/repomd.xml',
        'rpm/repodata/repomd.xml.asc',
        'rpm/RPM-GPG-KEY-rmux',
        'rpm/RPM-GPG-KEY-rmux-repository',
        'rpm/rmux.repo',
    }
    for name in required:
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(('fixture:' + name).encode('utf-8'))
    checksums = ''.join(
        f'{hashlib.sha256((root / name).read_bytes()).hexdigest()}  {name}\n'
        for name in sorted(name for name in required if name.startswith(('debian/', 'rpm/')))
    )
    (root / 'SHA256SUMS').write_text(checksums, encoding='utf-8')
    manifest = root / 'LINUX_REPOSITORY_RECOVERY.json'
    manifest.write_text('{}\n', encoding='utf-8')
    try:
        publisher.repository_files(root)
    except ValueError as error:
        if 'unexpected path' not in str(error):
            raise
    else:
        raise SystemExit('normal publisher accepted recovery-only metadata')
    manifest.unlink()
    if set(publisher.repository_files(root)) != required:
        raise SystemExit('verified metadata removal did not restore exact public bytes')
"#,
    );
    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn signer_artifact_manifest_and_workflow_id_are_exactly_bound() {
    let root = temp_dir("signer-binding");
    let run_path = root.join("run.json");
    let artifact_path = root.join("artifact.json");
    let proof_path = root.join("proof.json");
    let output_path = root.join("github-output");
    let run = json!({
        "id": 900,
        "workflow_id": 424242,
        "path": ".github/workflows/release-linux-repository-build.yml",
        "event": "workflow_dispatch",
        "run_attempt": 1,
        "head_sha": MAIN,
        "head_branch": "main",
        "status": "in_progress",
        "conclusion": null,
        "repository": {"id": 1239918790},
        "head_repository": {"id": 1239918790}
    });
    let artifact = json!({
        "id": 901,
        "name": format!("rmux-downstream-apt_rpm-signed-{SOURCE}-17"),
        "digest": SIGNED_DIGEST,
        "expired": false,
        "size_in_bytes": 4096,
        "created_at": "2026-07-25T00:00:00Z",
        "updated_at": "2026-07-25T00:00:01Z",
        "expires_at": "2026-08-01T00:00:00Z",
        "workflow_run": {
            "id": 900,
            "repository_id": 1239918790,
            "head_repository_id": 1239918790,
            "head_sha": MAIN
        }
    });
    write_json(&run_path, &run);
    write_json(&artifact_path, &artifact);

    let run_arg = run_path.to_string_lossy();
    let artifact_arg = artifact_path.to_string_lossy();
    let proof_arg = proof_path.to_string_lossy();
    let output_arg = output_path.to_string_lossy();
    let valid = run_helper(
        &root,
        &[
            "verify-signer-artifact",
            "--run-id",
            "900",
            "--protected-main-sha",
            MAIN,
            "--artifact-id",
            "901",
            "--artifact-digest",
            SIGNED_DIGEST,
            "--receipt-run-id",
            "700",
            "--plan-artifact-id",
            "701",
            "--plan-artifact-digest",
            PLAN_DIGEST,
            "--payload-artifact-id",
            "702",
            "--payload-artifact-digest",
            PAYLOAD_DIGEST,
            "--source-sha",
            SOURCE,
            "--release-id",
            "17",
            "--release-ref",
            "v1.2.3",
            "--run-json",
            &run_arg,
            "--artifact-json",
            &artifact_arg,
            "--proof",
            &proof_arg,
            "--github-output",
            &output_arg,
        ],
    );
    assert!(valid.status.success(), "{}", stderr(&valid));
    let proof: Value =
        serde_json::from_slice(&fs::read(&proof_path).expect("read proof")).expect("parse proof");
    assert_eq!(proof["producer"]["workflow_id"], 424242);
    assert_eq!(
        proof["producer"]["workflow_path"],
        ".github/workflows/release-linux-repository-build.yml"
    );
    assert_eq!(proof["artifact"]["artifact_id"], 901);
    assert_eq!(
        fs::read_to_string(&output_path).expect("read output"),
        "workflow_id=424242\n"
    );

    let mut forged_run = run.clone();
    forged_run["path"] = json!(".github/workflows/release-receipt.yml");
    write_json(&run_path, &forged_run);
    let bad_proof = root.join("bad-proof.json");
    let bad_proof_arg = bad_proof.to_string_lossy();
    let rejected = run_helper(
        &root,
        &[
            "verify-signer-artifact",
            "--run-id",
            "900",
            "--protected-main-sha",
            MAIN,
            "--artifact-id",
            "901",
            "--artifact-digest",
            SIGNED_DIGEST,
            "--receipt-run-id",
            "700",
            "--plan-artifact-id",
            "701",
            "--plan-artifact-digest",
            PLAN_DIGEST,
            "--payload-artifact-id",
            "702",
            "--payload-artifact-digest",
            PAYLOAD_DIGEST,
            "--source-sha",
            SOURCE,
            "--release-id",
            "17",
            "--release-ref",
            "v1.2.3",
            "--run-json",
            &run_arg,
            "--artifact-json",
            &artifact_arg,
            "--proof",
            &bad_proof_arg,
            "--github-output",
            &output_arg,
        ],
    );
    assert!(!rejected.status.success());
    assert!(stderr(&rejected).contains("signer run path mismatch"));
    write_json(&run_path, &run);

    let repository = root.join("repository");
    fs::create_dir(&repository).expect("create signed repository fixture");
    let base = repository.join("PACKAGE_REPOSITORY_BASE");
    fs::write(&base, format!("{MAIN}\n")).expect("write repository base");
    let manifest = repository.join("LINUX_REPOSITORY_RECOVERY.json");
    let base_arg = base.to_string_lossy();
    let manifest_arg = manifest.to_string_lossy();
    let created = run_helper(
        &root,
        &[
            "create-manifest",
            "--run-id",
            "900",
            "--protected-main-sha",
            MAIN,
            "--receipt-run-id",
            "700",
            "--plan-artifact-id",
            "701",
            "--plan-artifact-digest",
            PLAN_DIGEST,
            "--payload-artifact-id",
            "702",
            "--payload-artifact-digest",
            PAYLOAD_DIGEST,
            "--source-sha",
            SOURCE,
            "--release-id",
            "17",
            "--release-ref",
            "v1.2.3",
            "--repository-base",
            &base_arg,
            "--output",
            &manifest_arg,
        ],
    );
    assert!(created.status.success(), "{}", stderr(&created));
    let repository_arg = repository.to_string_lossy();
    let verified = run_helper(
        &root,
        &[
            "verify-bundle",
            "--repository-dir",
            &repository_arg,
            "--proof",
            &proof_arg,
            "--artifact-id",
            "901",
        ],
    );
    assert!(verified.status.success(), "{}", stderr(&verified));

    let mut changed: Value = serde_json::from_slice(&fs::read(&manifest).expect("read manifest"))
        .expect("parse manifest");
    changed["source_git_sha"] = json!(MAIN);
    write_json(&manifest, &changed);
    let rejected_bundle = run_helper(
        &root,
        &[
            "verify-bundle",
            "--repository-dir",
            &repository_arg,
            "--proof",
            &proof_arg,
            "--artifact-id",
            "901",
        ],
    );
    assert!(!rejected_bundle.status.success());
    assert!(stderr(&rejected_bundle).contains("manifest differs"));
    fs::remove_dir_all(root).expect("remove Linux recovery fixture");
}
