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
        .expect("run downstream summary fixture")
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
import copy
import importlib.util
import pathlib
import sys
import tempfile

sys.path.insert(0, 'scripts/release')

from downstream_channels import (
    CHANNELS,
    CHANNEL_POLICY,
    file_hash,
    target_for_channel,
    write_object,
)
from downstream_plan import expected_channel_entries
from downstream_summary import PRE_SITE_CHANNELS, create_summary

SOURCE = 'a' * 40
RELEASE = {
    'id': 17,
    'ref': 'v1.2.3',
    'intent_id': 'intent:release-17',
    'kind': 'stable',
    'tag_object_sha': 'b' * 40,
    'immutable': True,
}

def artifact(artifact_id, name, digest='c'):
    return {
        'artifact_id': artifact_id,
        'name': name,
        'archive_digest': 'sha256:' + digest * 64,
        'size_in_bytes': 100,
    }

RECEIPT = {
    'run_id': 20,
    'run_attempt': 1,
    'workflow_id': 316435347,
    'workflow_path': '.github/workflows/release-receipt.yml',
    'predicate_bundle': artifact(21, f'rmux-publication-receipt-{SOURCE}-17'),
    'predicate_sha256': 'd' * 64,
    'envelope_bundle': artifact(22, f'rmux-publication-receipt-envelope-{SOURCE}-17'),
    'envelope_sha256': 'e' * 64,
    'attestation': {
        'attestation_id': 'receipt-attestation-17',
        'bundle_file': 'publication-receipt.sigstore.json',
        'bundle_sha256': 'f' * 64,
    },
    'verified_at': '2026-07-20T00:00:00Z',
}

HOSTS = {
    'chocolatey': 'https://community.chocolatey.org/packages/rmux/1.2.3',
    'crates_io': 'https://crates.io/crates/rmux/1.2.3',
    'rmux_io': 'https://rmux.io/releases',
    'snap_candidate': 'https://snapcraft.io/rmux',
    'snap_stable': 'https://snapcraft.io/rmux',
}

def write_plan(root):
    plan = {
        'schema_version': 1,
        'status': 'disarmed-non-authoritative',
        'downstream_authority': False,
        'execution_authority': False,
        'execution_enabled': False,
        'repository_id': 1239918790,
        'source_git_sha': SOURCE,
        'release': RELEASE,
        'receipt': RECEIPT,
        'channel_policy': {
            'path': '.github/release/channel-policy.json',
            'schema_version': 1,
            'sha256': file_hash(CHANNEL_POLICY),
        },
        'snap_candidate_opt_in': False,
        'created_at': '2026-07-20T00:00:05Z',
        'channel_count': len(CHANNELS),
        'channels': expected_channel_entries('stable', False),
    }
    path = pathlib.Path(root) / 'downstream-channel-plan.json'
    write_object(path, plan)
    return path, plan

def target_url(channel, target):
    if channel in HOSTS:
        return HOSTS[channel]
    return f"https://github.com/{target['repository_full_name']}/releases"

def write_reference(root, channel, plan_path, plan, offset):
    target = target_for_channel(channel)
    planned = next(item for item in plan['channels'] if item['name'] == channel)
    state = {
        'denied': 'denied-by-policy',
        'blocked': 'blocked',
        'disarmed': 'prepared',
    }[planned['execution_decision']]
    evidence = {
        'schema_version': 1,
        'channel': channel,
        'target_kind': target['target_kind'],
        'repository_id': target['repository_id'],
        'external_id': None,
        'url': target_url(channel, target),
        'version': '1.2.3',
        'commit_sha': None,
        'public_live': False,
        'observed_at': '2026-07-20T00:00:20Z',
    }
    run_id = 100 + offset
    reference = {
        'schema_version': 1,
        'status': 'disarmed-non-authoritative',
        'downstream_authority': False,
        'execution_authority': False,
        'repository_id': 1239918790,
        'source_git_sha': SOURCE,
        'release': RELEASE,
        'receipt': RECEIPT,
        'plan_sha256': file_hash(plan_path),
        'channel': channel,
        'producer': {
            'run_id': run_id,
            'run_attempt': 1,
            'workflow_id': 200,
            'workflow_path': '.github/workflows/release-downstream.yml',
            'runner_group_id': 0,
            'runner_group_name': 'GitHub Actions',
            'runner_image': ('windows-latest' if channel == 'chocolatey' else 'ubuntu-22.04'),
        },
        'request_sha256': format(offset + 1, '064x'),
        'payload_set_sha256': format(offset + 21, '064x'),
        'idempotency_key': 'rmux-downstream-v1:' + format(offset + 41, '064x'),
        'state': state,
        'mutation_started': False,
        'remote_request_id': None,
        'public_live': False,
        'target_evidence': evidence,
        'predicate_bundle': artifact(
            300 + offset,
            f'rmux-downstream-{channel}-result-{SOURCE}-17',
        ),
        'predicate_sha256': format(offset + 61, '064x'),
        'envelope_bundle': artifact(
            400 + offset,
            f'rmux-downstream-{channel}-result-envelope-{SOURCE}-17',
        ),
        'envelope_sha256': format(offset + 81, '064x'),
        'attestation': {
            'attestation_id': f'attestation-{channel}',
            'bundle_file': 'downstream-channel-result.sigstore.json',
            'bundle_sha256': format(offset + 101, '064x'),
        },
        'verified_at': '2026-07-20T00:00:30Z',
    }
    path = pathlib.Path(root) / f'{channel}-result-reference.json'
    write_object(path, reference)
    return path

def build(root):
    root = pathlib.Path(root)
    plan_path, plan = write_plan(root)
    references = {
        channel: write_reference(root, channel, plan_path, plan, index)
        for index, channel in enumerate(CHANNELS)
    }
    pre = create_summary(
        plan_path=plan_path,
        phase='pre-site',
        result_paths={channel: references[channel] for channel in PRE_SITE_CHANNELS},
        pre_site_summary_path=None,
        created_at='2026-07-20T00:00:40Z',
    )
    pre_path = root / 'pre-site-channel-summary.json'
    write_object(pre_path, pre)
    final = create_summary(
        plan_path=plan_path,
        phase='final',
        result_paths={'rmux_io': references['rmux_io']},
        pre_site_summary_path=pre_path,
        created_at='2026-07-20T00:00:50Z',
    )
    return plan_path, plan, references, pre_path, pre, final
"#;

#[test]
fn ten_results_feed_site_then_exact_final_summary() {
    assert_fixture(&format!(
        "{FIXTURE}\n{}",
        r#"
with tempfile.TemporaryDirectory(dir=pathlib.Path.cwd()) as root:
    _, _, _, pre_path, pre, final = build(root)
    if pre['phase'] != 'pre-site' or pre['result_count'] != 10:
        raise SystemExit('pre-site result cardinality changed')
    if final['phase'] != 'final' or final['result_count'] != 11:
        raise SystemExit('final result cardinality changed')
    if final['pre_site_summary_sha256'] != file_hash(pre_path):
        raise SystemExit('final summary did not bind exact pre-site bytes')
    if [item['channel'] for item in final['results']] != list(CHANNELS):
        raise SystemExit('final summary channel order changed')
    if final['downstream_authority'] or final['rmux_io_authority']:
        raise SystemExit('summary acquired publication authority')
"#
    ));
}

#[test]
fn aggregation_rejects_missing_results_wrong_receipt_and_wrong_state() {
    assert_fixture(&format!(
        "{FIXTURE}\n{}",
        r#"
with tempfile.TemporaryDirectory(dir=pathlib.Path.cwd()) as root:
    plan_path, plan = write_plan(root)
    references = {
        channel: write_reference(root, channel, plan_path, plan, index)
        for index, channel in enumerate(CHANNELS)
    }
    incomplete = {channel: references[channel] for channel in PRE_SITE_CHANNELS[:-1]}
    try:
        create_summary(
            plan_path=plan_path,
            phase='pre-site',
            result_paths=incomplete,
            pre_site_summary_path=None,
            created_at='2026-07-20T00:00:40Z',
        )
    except ValueError as error:
        if 'reference set changed' not in str(error):
            raise
    else:
        raise SystemExit('incomplete result set was accepted')

    forged = copy.deepcopy(RECEIPT)
    forged['predicate_sha256'] = '0' * 64
    reference = pathlib.Path(references['apt_rpm'])
    value = __import__('json').loads(reference.read_text(encoding='utf-8'))
    value['receipt'] = forged
    write_object(reference, value)
    try:
        create_summary(
            plan_path=plan_path,
            phase='pre-site',
            result_paths={channel: references[channel] for channel in PRE_SITE_CHANNELS},
            pre_site_summary_path=None,
            created_at='2026-07-20T00:00:40Z',
        )
    except ValueError as error:
        if 'changed summary field receipt' not in str(error):
            raise
    else:
        raise SystemExit('result from a different receipt was accepted')

    references['apt_rpm'] = write_reference(root, 'apt_rpm', plan_path, plan, 0)
    value = __import__('json').loads(reference.read_text(encoding='utf-8'))
    value['state'] = 'prepared'
    write_object(reference, value)
    try:
        create_summary(
            plan_path=plan_path,
            phase='pre-site',
            result_paths={channel: references[channel] for channel in PRE_SITE_CHANNELS},
            pre_site_summary_path=None,
            created_at='2026-07-20T00:00:40Z',
        )
    except ValueError as error:
        if 'differs from its exact plan' not in str(error):
            raise
    else:
        raise SystemExit('result state contradicting the plan was accepted')
"#
    ));
}

#[test]
fn final_phase_rejects_changed_pre_site_summary_and_request_binds_digest() {
    assert_fixture(&format!(
        "{FIXTURE}\n{}",
        r#"
with tempfile.TemporaryDirectory(dir=pathlib.Path.cwd()) as root:
    plan_path, plan, references, pre_path, pre, _ = build(root)
    changed = copy.deepcopy(pre)
    changed['advertised_channels'] = ['apt_rpm']
    write_object(pre_path, changed)
    try:
        create_summary(
            plan_path=plan_path,
            phase='final',
            result_paths={'rmux_io': references['rmux_io']},
            pre_site_summary_path=pre_path,
            created_at='2026-07-20T00:00:50Z',
        )
    except ValueError as error:
        if 'advertised channels differ' not in str(error):
            raise
    else:
        raise SystemExit('changed pre-site summary was accepted')

    changed = copy.deepcopy(pre)
    apt = next(item for item in changed['results'] if item['channel'] == 'apt_rpm')
    apt['reference']['state'] = 'prepared'
    write_object(pre_path, changed)
    try:
        channel_request = None
        spec = importlib.util.spec_from_file_location(
            'channel_request', 'scripts/release/channel-request.py'
        )
        channel_request = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(channel_request)
        channel_request.pre_site_summary_digest(
            pre_path,
            channel='rmux_io',
            plan=plan,
            plan_path=plan_path,
            requested_at='2026-07-20T00:00:45Z',
        )
    except ValueError as error:
        if 'differs from its exact plan' not in str(error):
            raise
    else:
        raise SystemExit('pre-site summary contradicting the plan was accepted')

    write_object(pre_path, pre)
    digest = channel_request.pre_site_summary_digest(
        pre_path,
        channel='rmux_io',
        plan=plan,
        plan_path=plan_path,
        requested_at='2026-07-20T00:00:45Z',
    )
    if digest != file_hash(pre_path):
        raise SystemExit('rmux.io request did not bind the exact pre-site digest')
    try:
        channel_request.pre_site_summary_digest(
            pre_path,
            channel='apt_rpm',
            plan=plan,
            plan_path=plan_path,
            requested_at='2026-07-20T00:00:45Z',
        )
    except ValueError as error:
        if 'only rmux_io' not in str(error):
            raise
    else:
        raise SystemExit('non-site channel consumed a pre-site summary')
"#
    ));
}
