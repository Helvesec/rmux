#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn python(source: &str) -> Output {
    Command::new("python3")
        .arg("-c")
        .arg(source)
        .current_dir(repo_root())
        .output()
        .expect("run downstream result evidence fixture")
}

fn assert_fixture(source: &str) {
    let output = python(source);
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

const FIXTURE: &str = r#"
import argparse
import copy
import importlib.util
import json
import os
import pathlib
import tempfile

import sys
sys.path.insert(0, 'scripts/release')

import downstream_channels
from downstream_channels import (
    canonical_file_hash,
    canonical_hash,
    file_hash,
    target_for_channel,
    write_object,
)
from downstream_result_reference import create_reference

SOURCE = '1' * 40
RELEASE = {
    'id': 7,
    'ref': 'v1.2.3',
    'intent_id': 'intent:release-7',
    'kind': 'stable',
    'tag_object_sha': '2' * 40,
    'immutable': True,
}

def artifact(artifact_id, name, digest='3'):
    return {
        'artifact_id': artifact_id,
        'name': name,
        'archive_digest': 'sha256:' + digest * 64,
        'size_in_bytes': 100,
    }

RECEIPT = {
    'run_id': 41,
    'run_attempt': 1,
    'workflow_id': 316435347,
    'workflow_path': '.github/workflows/release-receipt.yml',
    'predicate_bundle': artifact(42, f'rmux-publication-receipt-{SOURCE}-7'),
    'predicate_sha256': '4' * 64,
    'envelope_bundle': artifact(43, f'rmux-publication-receipt-envelope-{SOURCE}-7'),
    'envelope_sha256': '5' * 64,
    'attestation': {
        'attestation_id': 'receipt-attestation-7',
        'bundle_file': 'publication-receipt.sigstore.json',
        'bundle_sha256': '6' * 64,
    },
    'verified_at': '2026-07-20T00:00:00Z',
}

def write_fixture(root):
    root = pathlib.Path(root)
    target = target_for_channel('homebrew_tap')
    payload = {
        'schema_version': 1,
        'predicate_type': 'https://rmux.io/attestations/release-downstream-channel-payload/v1',
        'status': 'disarmed-non-authoritative',
        'downstream_authority': False,
        'execution_authority': False,
        'repository_id': 1239918790,
        'source_git_sha': SOURCE,
        'release': RELEASE,
        'channel': 'homebrew_tap',
        'producer': {
            'run_id': 41,
            'run_attempt': 1,
            'workflow_id': 316435347,
            'workflow_path': '.github/workflows/release-receipt.yml',
            'job_workflow_path': '.github/workflows/release-downstream.yml',
            'runner_group_id': 0,
            'runner_group_name': 'GitHub Actions',
            'runner_image': 'ubuntu-22.04',
        },
        'source_evidence': {
            'receipt_predicate_sha256': RECEIPT['predicate_sha256'],
            'candidate_manifest_sha256': 'a' * 64,
            'candidate_manifest_artifact_digest': 'sha256:' + 'b' * 64,
            'candidate_manifest_expires_at': '2026-07-21T00:00:00Z',
            'release_asset_set_sha256': 'c' * 64,
            'sha256sums_sha256': 'd' * 64,
        },
        'artifact': {
            **artifact(32, f'rmux-downstream-homebrew_tap-payload-{SOURCE}-7'),
            'created_at': '2026-07-20T00:00:01Z',
            'updated_at': '2026-07-20T00:00:02Z',
            'expires_at': '2026-07-21T00:00:00Z',
        },
        'created_at': '2026-07-20T00:00:03Z',
        'file_count': 1,
        'files': [{
            'name': 'rmux.rb',
            'role': 'homebrew-formula',
            'size': 1,
            'sha256': '7' * 64,
        }],
        'retention_expires_at': '2026-07-21T00:00:00Z',
    }
    payload_digest = canonical_hash(payload['files'])
    payload['payload_set_sha256'] = payload_digest
    key_material = {
        'receipt_predicate_sha256': RECEIPT['predicate_sha256'],
        'channel': 'homebrew_tap',
        'release_ref': RELEASE['ref'],
        'payload_set_sha256': payload_digest,
        'target': target,
    }
    request = {
        'schema_version': 1,
        'status': 'disarmed-non-authoritative',
        'downstream_authority': False,
        'execution_authority': False,
        'execution_enabled': False,
        'repository_id': 1239918790,
        'source_git_sha': SOURCE,
        'release': RELEASE,
        'receipt': RECEIPT,
        'plan_sha256': '8' * 64,
        'channel': 'homebrew_tap',
        'operation': 'initial',
        'retry_depth': 0,
        'idempotency_key': 'rmux-downstream-v1:' + canonical_hash(key_material),
        'retry_of_request_sha256': None,
        'payload_artifact': payload,
        'payload_set_sha256': payload_digest,
        'pre_site_summary_sha256': None,
        'target': target,
        'previous_result': None,
        'rebuild_native': False,
        'requested_at': '2026-07-20T00:00:05Z',
        'expires_at': '2026-07-20T01:00:00Z',
    }
    request_path = root / 'downstream-channel-request.json'
    write_object(request_path, request)
    evidence = {
        'schema_version': 1,
        'channel': 'homebrew_tap',
        'target_kind': 'github-repository',
        'repository_id': target['repository_id'],
        'external_id': None,
        'url': 'https://github.com/Helvesec/homebrew-rmux/releases',
        'version': '1.2.3',
        'commit_sha': None,
        'public_live': False,
        'observed_at': '2026-07-20T00:00:20Z',
    }
    target_path = root / 'downstream-channel-target-evidence.json'
    write_object(target_path, evidence)
    producer = {
        'run_id': 51,
        'run_attempt': 1,
        'workflow_id': 52,
        'workflow_path': '.github/workflows/release-downstream.yml',
        'runner_group_id': 0,
        'runner_group_name': 'GitHub Actions',
        'runner_image': 'ubuntu-22.04',
    }
    predicate = {
        'schema_version': 1,
        'predicate_type': 'https://rmux.io/attestations/release-downstream-channel-result/v1',
        'status': 'disarmed-non-authoritative',
        'downstream_authority': False,
        'execution_authority': False,
        'repository_id': 1239918790,
        'source_git_sha': SOURCE,
        'release': RELEASE,
        'receipt': RECEIPT,
        'request_sha256': file_hash(request_path),
        'payload_set_sha256': payload_digest,
        'idempotency_key': request['idempotency_key'],
        'channel': 'homebrew_tap',
        'target': target,
        'producer': producer,
        'subject': {
            'name': 'downstream-channel-target-evidence.json',
            'sha256': canonical_file_hash(evidence),
        },
        'state': 'prepared',
        'started_at': '2026-07-20T00:00:10Z',
        'mutation_started': False,
        'remote_request_id': None,
        'target_evidence': evidence,
        'observed_at': evidence['observed_at'],
    }
    predicate_path = root / 'downstream-channel-result-predicate.json'
    write_object(predicate_path, predicate)
    predicate_name = f'rmux-downstream-homebrew_tap-result-{SOURCE}-7'
    predicate_artifact = artifact(53, predicate_name)
    envelope = {
        'schema_version': 1,
        'envelope_type': 'https://rmux.io/envelopes/release-downstream-channel-result/v1',
        'status': 'disarmed-non-authoritative',
        'downstream_authority': False,
        'execution_authority': False,
        'repository_id': 1239918790,
        'source_git_sha': SOURCE,
        'release_ref': RELEASE['ref'],
        'channel': 'homebrew_tap',
        'request_sha256': predicate['request_sha256'],
        'predicate_sha256': file_hash(predicate_path),
        'attestation': {
            'attestation_id': 'result-attestation-7',
            'bundle_file': 'downstream-channel-result.sigstore.json',
            'bundle_sha256': '9' * 64,
        },
        'result_bundle': predicate_artifact,
        'created_at': '2026-07-20T00:00:30Z',
    }
    envelope_path = root / 'downstream-channel-result-envelope.json'
    write_object(envelope_path, envelope)
    predicate_meta = {
        'artifact_id': 53,
        'name': predicate_name,
        'digest': predicate_artifact['archive_digest'],
        'size_in_bytes': predicate_artifact['size_in_bytes'],
        'run_id': producer['run_id'],
        'source_git_sha': SOURCE,
    }
    envelope_meta = {
        'artifact_id': 54,
        'name': f'rmux-downstream-homebrew_tap-result-envelope-{SOURCE}-7',
        'digest': 'sha256:' + 'a' * 64,
        'size_in_bytes': 100,
        'run_id': producer['run_id'],
        'source_git_sha': SOURCE,
    }
    predicate_meta_path = root / 'predicate-artifact.json'
    envelope_meta_path = root / 'envelope-artifact.json'
    write_object(predicate_meta_path, predicate_meta)
    write_object(envelope_meta_path, envelope_meta)
    return {
        'request': request_path,
        'target': target_path,
        'predicate': predicate_path,
        'envelope': envelope_path,
        'predicate_meta': predicate_meta_path,
        'envelope_meta': envelope_meta_path,
        'predicate_value': predicate,
    }

def create(paths):
    return create_reference(
        request_path=paths['request'],
        predicate_path=paths['predicate'],
        envelope_path=paths['envelope'],
        predicate_artifact_path=paths['predicate_meta'],
        envelope_artifact_path=paths['envelope_meta'],
        verified_at='2026-07-20T00:00:40Z',
    )

# Repository publication remains intentionally blocked. These tests isolate
# result evidence while retaining the production payload validator.
_channel_contracts = downstream_channels.contract_channels()
_channel_contracts['homebrew_tap'] = {
    **_channel_contracts['homebrew_tap'],
    'blockers': [],
}
downstream_channels.contract_channels = lambda: _channel_contracts
"#;

#[test]
fn exact_reference_binds_artifacts_and_rejects_forged_metadata() {
    assert_fixture(&format!(
        "{FIXTURE}\n{}",
        r#"
with tempfile.TemporaryDirectory(dir=pathlib.Path.cwd()) as root:
    paths = write_fixture(root)
    reference = create(paths)
    if reference['predicate_bundle']['artifact_id'] != 53:
        raise SystemExit('predicate artifact ID was not bound')
    if reference['receipt'] != RECEIPT or reference['plan_sha256'] != '8' * 64:
        raise SystemExit('result reference did not bind its receipt and plan')
    metadata = json.loads(paths['envelope_meta'].read_text(encoding='utf-8'))
    metadata['run_id'] = 999
    write_object(paths['envelope_meta'], metadata)
    try:
        create(paths)
    except ValueError as error:
        if 'artifact API identity changed' not in str(error):
            raise
    else:
        raise SystemExit('forged result artifact metadata was accepted')
"#
    ));
}

#[test]
fn exact_reference_rejects_changed_envelope_and_symlinked_request() {
    assert_fixture(&format!(
        "{FIXTURE}\n{}",
        r#"
with tempfile.TemporaryDirectory(dir=pathlib.Path.cwd()) as root:
    paths = write_fixture(root)
    envelope = json.loads(paths['envelope'].read_text(encoding='utf-8'))
    envelope['request_sha256'] = '0' * 64
    write_object(paths['envelope'], envelope)
    try:
        create(paths)
    except ValueError as error:
        if 'changed exact field request_sha256' not in str(error):
            raise
    else:
        raise SystemExit('changed result envelope was accepted')

    paths = write_fixture(root)
    linked = pathlib.Path(root) / 'request-link.json'
    linked.symlink_to(paths['request'])
    paths['request'] = linked
    try:
        create(paths)
    except ValueError as error:
        if 'cannot be a symlink' not in str(error):
            raise
    else:
        raise SystemExit('symlinked request was accepted')
"#
    ));
}

#[test]
fn sigstore_verifier_binds_exact_statement_signer_and_timestamp() {
    assert_fixture(&format!(
        "{FIXTURE}\n{}",
        r#"
with tempfile.TemporaryDirectory(dir=pathlib.Path.cwd()) as root:
    root = pathlib.Path(root)
    paths = write_fixture(root)
    bundle = root / 'downstream-channel-result.sigstore.json'
    bundle.write_text('{}\n', encoding='utf-8')
    envelope = json.loads(paths['envelope'].read_text(encoding='utf-8'))
    envelope['attestation']['bundle_sha256'] = file_hash(bundle)
    write_object(paths['envelope'], envelope)
    output_path = root / 'verification.json'
    arguments_path = root / 'arguments.txt'
    gh = root / 'gh'
    gh.write_text(
        '#!/bin/sh\nprintf \'%s\\n\' "$@" > "$RMUX_ARGS_LOG"\ncat "$RMUX_FAKE_OUTPUT"\n',
        encoding='utf-8',
    )
    gh.chmod(0o700)
    statement = {
        'subject': [{
            'name': 'downstream-channel-target-evidence.json',
            'digest': {'sha256': file_hash(paths['target'])},
        }],
        'predicateType': 'https://rmux.io/attestations/release-downstream-channel-result/v1',
        'predicate': paths['predicate_value'],
    }

    def write_verification(*, timestamps=True, digest=None):
        current = copy.deepcopy(statement)
        if digest is not None:
            current['subject'][0]['digest']['sha256'] = digest
        write_object(output_path, [{
            'verificationResult': {
                'signature': {'certificate': {'subject': 'trusted'}},
                'verifiedTimestamps': ([{'type': 'transparency-log'}] if timestamps else []),
                'statement': current,
            },
        }])

    spec = importlib.util.spec_from_file_location(
        'verify_channel_result_attestation',
        'scripts/release/verify-channel-result-attestation.py',
    )
    verifier = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(verifier)
    args = argparse.Namespace(
        gh=gh,
        request=paths['request'],
        target_evidence=paths['target'],
        bundle=bundle,
        predicate=paths['predicate'],
        envelope=paths['envelope'],
        source_sha=SOURCE,
        release_ref=RELEASE['ref'],
        channel='homebrew_tap',
    )
    os.environ['RMUX_FAKE_OUTPUT'] = str(output_path)
    os.environ['RMUX_ARGS_LOG'] = str(arguments_path)
    write_verification()
    verifier.verify(args)
    arguments = arguments_path.read_text(encoding='utf-8').splitlines()
    for expected in (
        '--deny-self-hosted-runners',
        'Helvesec/rmux/.github/workflows/release-downstream.yml',
        '--signer-digest',
        '--source-digest',
        'refs/tags/v1.2.3',
    ):
        if expected not in arguments:
            raise SystemExit(f'missing attestation policy argument: {expected}')

    write_verification(digest='0' * 64)
    try:
        verifier.verify(args)
    except ValueError as error:
        if 'differs from exact local bytes' not in str(error):
            raise
    else:
        raise SystemExit('forged Sigstore statement was accepted')

    write_verification(timestamps=False)
    try:
        verifier.verify(args)
    except ValueError as error:
        if 'lacks signed verification evidence' not in str(error):
            raise
    else:
        raise SystemExit('untimestamped Sigstore statement was accepted')
"#
    ));
}

#[test]
fn result_reference_schema_separates_receipt_and_result_attestations() {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../.github/release/schemas/downstream-channel-result-reference.schema.json"
    ))
    .expect("result reference schema");
    assert_eq!(
        schema["properties"]["attestation"]["$ref"],
        "#/$defs/result_attestation"
    );
    assert_eq!(
        schema["$defs"]["receipt"]["properties"]["attestation"]["$ref"],
        "#/$defs/receipt_attestation"
    );
    assert_eq!(
        schema["$defs"]["receipt_attestation"]["properties"]["bundle_file"]["const"],
        "publication-receipt.sigstore.json"
    );
    assert_eq!(
        schema["$defs"]["result_attestation"]["properties"]["bundle_file"]["const"],
        "downstream-channel-result.sigstore.json"
    );
}
