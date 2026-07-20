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
        .expect("run adversarial Python fixture")
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

const PRELUDE: &str = r#"
import hashlib
import json
import pathlib
import sys
import tempfile
sys.path.insert(0, 'scripts/release')

SOURCE = 'a' * 40
RELEASE = {
    'id': 1,
    'ref': 'v1.2.3',
    'intent_id': 'intent:12',
    'kind': 'stable',
    'tag_object_sha': 'b' * 40,
    'immutable': True,
}

def artifact(name):
    return {
        'artifact_id': 1,
        'name': name,
        'archive_digest': 'sha256:' + 'c' * 64,
        'size_in_bytes': 1,
    }

RECEIPT = {
    'run_id': 1,
    'run_attempt': 1,
    'workflow_id': 1,
    'workflow_path': '.github/workflows/release-receipt.yml',
    'predicate_bundle': artifact(f'rmux-publication-receipt-{SOURCE}-1'),
    'predicate_sha256': 'd' * 64,
    'envelope_bundle': artifact(f'rmux-publication-receipt-envelope-{SOURCE}-1'),
    'envelope_sha256': 'e' * 64,
    'attestation': {
        'attestation_id': 'attestation-1',
        'bundle_file': 'publication-receipt.sigstore.json',
        'bundle_sha256': 'f' * 64,
    },
    'verified_at': '2026-07-19T00:00:00Z',
}
"#;

#[test]
fn snap_candidate_opt_in_is_preserved_for_stable_retry_plans() {
    assert_fixture(&format!(
        "{PRELUDE}\n{}",
        r#"
from downstream_plan import expected_channel_entries
for opted_in in (False, True):
    entries = expected_channel_entries('stable', opted_in)
    snap = next(entry for entry in entries if entry['name'] == 'snap_candidate')
    if snap['explicit_opt_in'] is not opted_in:
        raise SystemExit(f'Snap candidate opt-in was not preserved: {opted_in!r}')
    expected = 'blocked' if opted_in else 'denied'
    if snap['execution_decision'] != expected:
        raise SystemExit(f'Snap candidate decision differs: {snap!r}')
"#
    ));
}

#[test]
fn duplicate_json_names_fail_closed() {
    assert_fixture(&format!(
        "{PRELUDE}\n{}",
        r#"
import importlib.util
import downstream_contract
from downstream_channels import read_object
spec = importlib.util.spec_from_file_location('validate_contracts', 'scripts/release/validate-contracts.py')
validate_contracts = importlib.util.module_from_spec(spec)
spec.loader.exec_module(validate_contracts)
with tempfile.TemporaryDirectory(dir=pathlib.Path.cwd()) as root:
    path = pathlib.Path(root) / 'duplicate.json'
    path.write_text('{"authority":false,"authority":true}\n', encoding='utf-8')
    for reader in (
        lambda: read_object(path, 'duplicate evidence'),
        lambda: downstream_contract._read(path),
        lambda: validate_contracts.load(path),
    ):
        try:
            reader()
        except ValueError:
            pass
        else:
            raise SystemExit('duplicate JSON object name was accepted')
"#
    ));
}

#[test]
fn mutated_plan_entries_fail_closed() {
    assert_fixture(&format!(
        "{PRELUDE}\n{}",
        r#"
from downstream_channels import CHANNELS, CHANNEL_POLICY, file_hash
from downstream_plan import validate_plan
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
    'created_at': '2026-07-19T00:00:00Z',
    'channel_count': 11,
    'channels': [
        {
            'name': name,
            'policy_decision': 'allow',
            'execution_decision': 'disarmed',
            'payload_ready': True,
            'blockers': [],
        }
        for name in CHANNELS
    ],
}
try:
    validate_plan(plan)
except ValueError:
    pass
else:
    raise SystemExit('mutated plan entries were accepted')
"#
    ));
}

#[test]
fn forged_result_predicates_fail_closed() {
    assert_fixture(&format!(
        "{PRELUDE}\n{}",
        r#"
import importlib.util
from downstream_channels import canonical_file_hash, target_for_channel
spec = importlib.util.spec_from_file_location('channel_result', 'scripts/release/channel-result.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
target = target_for_channel('homebrew_tap')
evidence = {
    'schema_version': 1,
    'channel': 'homebrew_tap',
    'target_kind': 'github-repository',
    'repository_id': target['repository_id'],
    'external_id': None,
    'url': 'https://evil.example/release',
    'version': '1.2.3',
    'commit_sha': None,
    'public_live': False,
    'observed_at': '2026-07-19T00:00:01Z',
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
    'request_sha256': '1' * 64,
    'payload_set_sha256': '2' * 64,
    'idempotency_key': 'rmux-downstream-v1:' + '3' * 64,
    'channel': 'homebrew_tap',
    'target': target,
    'producer': {
        'run_id': 1,
        'run_attempt': 1,
        'workflow_id': 1,
        'workflow_path': '.github/workflows/release-evil.yml',
        'runner_group_id': 0,
        'runner_group_name': 'GitHub Actions',
        'runner_image': 'ubuntu-22.04',
    },
    'subject': {
        'name': 'downstream-channel-target-evidence.json',
        'sha256': canonical_file_hash(evidence),
    },
    'state': 'failed-transient',
    'started_at': '2026-07-19T00:00:00Z',
    'mutation_started': False,
    'remote_request_id': None,
    'target_evidence': evidence,
    'observed_at': '2026-07-19T00:00:01Z',
}
try:
    module.validate_predicate(predicate, None, None)
except ValueError:
    pass
else:
    raise SystemExit('forged result predicate was accepted')
"#
    ));
}

#[test]
fn forged_summary_results_fail_closed() {
    assert_fixture(&format!(
        "{PRELUDE}\n{}",
        r#"
import argparse
import importlib.util
from downstream_channels import CHANNEL_POLICY, file_hash
from downstream_plan import expected_channel_entries
spec = importlib.util.spec_from_file_location('channel_summary', 'scripts/release/channel-summary.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
with tempfile.TemporaryDirectory() as root:
    root = pathlib.Path(root)
    plan_path = root / 'plan.json'
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
        'created_at': '2026-07-19T00:00:00Z',
        'channel_count': 11,
        'channels': expected_channel_entries('stable', False),
    }
    plan_path.write_text(json.dumps(plan, indent=2, sort_keys=True) + '\n', encoding='utf-8')
    args = argparse.Namespace(
        plan=plan_path,
        result_predicate=['apt_rpm=/tmp/forged-predicate.json'],
        result_envelope=['apt_rpm=/tmp/forged-envelope.json'],
        created_at='2026-07-19T00:00:01Z',
    )
    try:
        module.expected_summary(args)
    except ValueError as error:
        if 'result aggregation is blocked' not in str(error):
            raise
    else:
        raise SystemExit('forged summary result was accepted')
"#
    ));
}

#[test]
fn retry_state_requires_no_prior_mutation() {
    assert_fixture(&format!(
        "{PRELUDE}\n{}",
        r#"
from downstream_result import validate_retryable_previous
for state in ('prepared', 'failed-transient'):
    validate_retryable_previous({
        'state': state,
        'mutation_started': False,
        'remote_request_id': None,
    })
for state, mutation_started, remote_request_id in (
    ('submitted', True, 'remote-1'),
    ('pending-moderation', True, 'remote-1'),
    ('public-live', True, 'remote-1'),
    ('no-op-exact', False, 'remote-1'),
    ('failed-terminal', False, None),
    ('failed-transient', True, 'remote-1'),
):
    try:
        validate_retryable_previous({
            'state': state,
            'mutation_started': mutation_started,
            'remote_request_id': remote_request_id,
        })
    except ValueError:
        pass
    else:
        raise SystemExit(f'unsafe retry state accepted: {state}')
"#
    ));
}

#[test]
fn remote_and_target_identities_must_match() {
    assert_fixture(&format!(
        "{PRELUDE}\n{}",
        r#"
from downstream_result import validate_remote_identity
validate_remote_identity({'external_id': 'submission-A'}, 'submission-A')
for external_id, remote_id in (
    ('submission-B', 'submission-A'),
    (None, 'submission-A'),
    ('submission-A', None),
):
    try:
        validate_remote_identity({'external_id': external_id}, remote_id)
    except ValueError:
        pass
    else:
        raise SystemExit('mismatched remote identity was accepted')
"#
    ));
}
