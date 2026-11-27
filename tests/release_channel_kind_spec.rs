#![cfg(unix)]

//! One spec per release kind: every channel the receipt routes runs the real
//! `channel-execution` decision, then the worker's own non-executable steps
//! from those real outputs, and the sealed state must satisfy the final
//! channel summary. A drift between a produced and an expected state is red.

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
        .expect("run release channel kind fixture")
}

fn assert_kind(kind: &str) {
    let output = python(&format!("{FIXTURE}\nrun_kind({kind:?})\n"));
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(&format!("kind {kind}: ok")),
        "kind {kind} did not report its verdict"
    );
}

const FIXTURE: &str = r#"
import argparse
import importlib.util
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, 'scripts/release')

from downstream_channels import CHANNELS, CHANNEL_POLICY
from downstream_plan import expected_channel_entries
from downstream_summary import _validate_planned_state

RECEIPT_PATH = '.github/workflows/release-receipt.yml'
RELEASE_REFS = {'rc': 'v1.2.3-rc.1', 'stable': 'v1.2.3'}
EXPRESSION = re.compile(r'\$\{\{\s*(.*?)\s*\}\}')


def read(path):
    return pathlib.Path(path).read_text(encoding='utf-8')


def sections(text, indent):
    marker = re.compile(rf'^{" " * indent}([A-Za-z0-9_-]+):[^\n]*$', re.M)
    found = list(marker.finditer(text))
    return {
        match.group(1): text[match.start():found[index + 1].start() if index + 1 < len(found) else len(text)]
        for index, match in enumerate(found)
    }


def kind_allows(condition, kind):
    if condition is None:
        return True
    condition = condition.strip()
    if condition.startswith('${{') and condition.endswith('}}'):
        condition = condition[3:-2]
    for term in (part.strip() for part in condition.split('&&')):
        if term in {'!cancelled()', '!failure()'}:
            continue
        selected = re.fullmatch(r"inputs\.release_kind == '([a-z]+)'", term)
        if selected is None:
            raise ValueError(f'receipt job condition is not interpretable: {term}')
        if selected.group(1) != kind:
            return False
    return True


def routed_channels(kind):
    jobs = sections(read(RECEIPT_PATH).split('\njobs:\n', 1)[1], 2)
    routes = []
    for job, body in jobs.items():
        condition = re.search(r'^    if: (.*)$', body, re.M)
        targets = re.findall(r'^    uses: \./(\.github/workflows/\S+)$', body, re.M)
        targets += [
            f'{action}/action.yml'
            for action in re.findall(r'^      - uses: \./(\.github/actions/\S+)$', body, re.M)
        ]
        for worker in targets:
            if 'channel-execution.py' not in read(worker):
                continue
            selected = re.search(r'^\s+channel: ([a-z_]+)$', body, re.M)
            fixed = re.search(r'channel-execution\.py[^\n]*\n?[^\n]*--channel ([a-z_]+)', read(worker))
            channel = selected.group(1) if selected else fixed.group(1) if fixed else None
            if channel not in CHANNELS:
                raise ValueError(f'receipt job {job} routes no exact channel')
            if kind_allows(condition.group(1) if condition else None, kind):
                routes.append((job, worker, channel))
    return routes


def block_scalar(lines, style):
    indent = min(len(line) - len(line.lstrip()) for line in lines if line.strip())
    body = [line[indent:] for line in lines]
    if style == '|':
        return '\n'.join(body).rstrip('\n') + '\n'
    if style == '>-':
        return ' '.join(line.strip() for line in body if line.strip())
    raise ValueError(f'unsupported block scalar {style}')


def parse_steps(text):
    head = re.search(r'^( +)steps:\n( +)- ', text, re.M)
    if head is None:
        raise ValueError('worker has no step list')
    item = head.group(2)
    lines = text[head.end() - 2 - len(item):].splitlines()
    steps, current = [], None
    for line in lines:
        if line.startswith(item + '- '):
            current = [' ' * (len(item) + 2) + line[len(item) + 2:]]
            steps.append(current)
        elif current is not None and (not line.strip() or line.startswith(' ' * (len(item) + 2))):
            current.append(line)
        elif current is not None:
            break
    return [parse_step(step, len(item) + 2) for step in steps]


def parse_step(lines, indent):
    step, index = {}, 0
    key_line = re.compile(rf'^{" " * indent}([a-z-]+):(?: (.*))?$')
    while index < len(lines):
        found = key_line.match(lines[index])
        if found is None:
            raise ValueError(f'unparsed step line: {lines[index]!r}')
        key, value = found.group(1), (found.group(2) or '').strip()
        nested = []
        index += 1
        while index < len(lines) and (not lines[index].strip() or lines[index].startswith(' ' * (indent + 1))):
            nested.append(lines[index])
            index += 1
        if value in {'|', '>-'}:
            step[key] = block_scalar(nested, value)
        elif key in {'env', 'with'} and not value:
            step[key] = dict(
                (name.strip(), item.strip())
                for name, item in (line.split(':', 1) for line in nested if line.strip())
            )
        else:
            step[key] = value
    return step


def resolve(text, outputs, inputs):
    def value(match):
        expression = match.group(1)
        step = re.fullmatch(r'steps\.([a-z_-]+)\.outputs\.([a-z_-]+)', expression)
        if step:
            return outputs.get(step.group(1), {}).get(step.group(2), '')
        named = re.fullmatch(r'inputs\.([a-z_-]+)', expression)
        if named and named.group(1) in inputs:
            return inputs[named.group(1)]
        raise ValueError(f'worker expression is not interpretable: {expression}')
    return EXPRESSION.sub(value, text)


def read_outputs(path):
    result = {}
    for line in path.read_text(encoding='utf-8-sig').splitlines():
        if '<<' in line.split('=', 1)[0]:
            raise ValueError('multiline step outputs are not interpretable')
        name, item = line.split('=', 1)
        result[name] = item
    return result


def run_decision(root, channel, entry, kind):
    spec = importlib.util.spec_from_file_location('channel_execution', 'scripts/release/channel-execution.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    # Only the signed request envelope check is replaced; the decision-to-state
    # mapping and the target evidence validation stay the real code.
    module.validate_request = lambda value: value
    request = {
        'channel': channel,
        'plan_entry': entry,
        'execution_authority': entry['execution_enabled'],
        'release': {'ref': RELEASE_REFS[kind]},
    }
    request_path = root / 'request.json'
    request_path.write_text(json.dumps(request), encoding='utf-8')
    github_output = root / 'decision-output'
    github_output.write_text('', encoding='utf-8')
    module.execute(argparse.Namespace(
        request=request_path,
        channel=channel,
        target_evidence=root / 'target-evidence.json',
        github_output=github_output,
    ))
    return read_outputs(github_output)


def run_shell(root, step, outputs, inputs):
    script = resolve(step['run'], outputs, inputs)
    env = {key: resolve(item, outputs, inputs) for key, item in step.get('env', {}).items()}
    github_output = root / f"output-{step.get('id', 'anonymous')}"
    github_output.write_text('', encoding='utf-8')
    environment = {'PATH': os.environ['PATH'], 'HOME': os.environ.get('HOME', '/'), **env,
                   'GITHUB_OUTPUT': str(github_output)}
    if step['shell'] == 'bash':
        command = ['bash', '--noprofile', '--norc', '-eo', 'pipefail', '-c', script]
    elif step['shell'] == 'pwsh':
        if shutil.which('pwsh') is None:
            return None
        command = ['pwsh', '-NoProfile', '-NonInteractive', '-Command',
                   "$ErrorActionPreference = 'stop'\n" + script
                   + "\nif (Test-Path -LiteralPath variable:\\LASTEXITCODE) { exit $LASTEXITCODE }"]
    else:
        raise ValueError(f"unsupported worker shell {step['shell']}")
    completed = subprocess.run(command, env=environment, capture_output=True, text=True)
    if completed.returncode != 0:
        return False
    return read_outputs(github_output)


def run_worker(root, worker, channel, entry, kind):
    steps = parse_steps(read(worker))
    decision_index = next(index for index, step in enumerate(steps) if step.get('id') == 'decision')
    decision = steps[decision_index]
    if 'scripts/release/channel-execution.py' not in decision['run']:
        raise ValueError(f'{worker} decision does not run channel-execution')
    if f'--channel {channel}' not in decision['run'] and '--channel "${{ inputs.channel }}"' not in decision['run']:
        raise ValueError(f'{worker} decision names another channel')
    outputs = {'decision': run_decision(root, channel, entry, kind)}
    inputs = {'channel': channel}
    executes = outputs['decision']['execute'] == 'true'
    notes = []
    for step in steps[decision_index + 1:]:
        label = step.get('name', step.get('id', step.get('uses')))
        condition = step.get('if')
        if condition is not None:
            if condition != "steps.decision.outputs.execute == 'true'":
                raise ValueError(f'{worker} step condition is not interpretable: {condition}')
            if not executes:
                continue
        if executes:
            return 'executes', None, None, notes
        if step.get('uses') == './.github/actions/release-channel-result':
            sealed = step['with']
            return ('sealed', resolve(sealed['state'], outputs, inputs),
                    resolve(sealed['mutation-started'], outputs, inputs), notes)
        if 'uses' in step:
            raise ValueError(f'{worker} runs {label} on a non-executable path')
        produced = run_shell(root, step, outputs, inputs)
        if produced is False:
            return 'refused', label, None, notes
        if produced is None:
            notes.append(f'{channel}: {step["shell"]} step {label!r} NOT EXECUTED (no interpreter)')
            return 'not-executed', label, None, notes
        if 'id' in step:
            outputs[step['id']] = produced
    raise ValueError(f'{worker} never seals a result')


def run_channels(kind, snap_opt_in):
    entries = {
        entry['name']: entry
        for entry in expected_channel_entries(kind, snap_opt_in, authority_active=True)
    }
    routes = routed_channels(kind)
    if sorted(channel for _, _, channel in routes) != sorted(CHANNELS):
        raise SystemExit(f'{kind}: receipt does not route each channel exactly once: {routes}')
    failures, notes = [], []
    for job, worker, channel in routes:
        entry = entries[channel]
        with tempfile.TemporaryDirectory(dir=pathlib.Path.cwd().parent) as root:
            outcome, state, mutation, worker_notes = run_worker(pathlib.Path(root), worker, channel, entry, kind)
        notes += worker_notes
        where = f'{kind} opt_in={snap_opt_in} {job} ({worker}) {channel} decision={entry["execution_decision"]}'
        if outcome == 'not-executed':
            continue
        if entry['execution_decision'] == 'enabled':
            if outcome != 'executes':
                failures.append(f'{where}: executable request did not reach its writer ({outcome} {state})')
            else:
                print(f'{where}: reaches its writer')
            continue
        if outcome != 'sealed':
            failures.append(f'{where}: worker refused its own policy outcome at step {state!r}')
            continue
        if mutation != 'false':
            failures.append(f'{where}: non-executable result claims a mutation')
        try:
            _validate_planned_state({'state': state}, entry)
        except ValueError as error:
            failures.append(f'{where}: sealed state {state!r} fails the final summary: {error}')
        else:
            print(f'{where}: sealed {state}')
    return failures, notes


def probe_rmux_io_refusal():
    worker = '.github/workflows/release-rmux-io-channel.yml'
    gate = next(step for step in parse_steps(read(worker)) if step.get('name') == 'Refuse automated rmux.io mutation')
    forged = (
        {'execute': 'true', 'state': ''},
        {'execute': 'true', 'state': 'blocked'},
        {'execute': 'false', 'state': 'prepared'},
        {'execute': 'false', 'state': 'public-live'},
        {'execute': 'false', 'state': ''},
    )
    for decision in forged:
        with tempfile.TemporaryDirectory(dir=pathlib.Path.cwd().parent) as root:
            if run_shell(pathlib.Path(root), gate, {'decision': decision}, {'channel': 'rmux_io'}) is not False:
                raise SystemExit(f'rmux.io gate accepted a mutating outcome: {decision}')
    print('rmux.io gate refuses every executable or mutating outcome')


def run_shadow():
    receipt = read(RECEIPT_PATH)
    kinds = re.search(r'^      release_kind:\n(?:        .*\n)*?        options: \[(.*)\]$', receipt, re.M)
    if kinds is None or [item.strip() for item in kinds.group(1).split(',')] != ['rc', 'stable']:
        raise SystemExit('shadow: receipt release_kind is not exactly rc or stable')
    for snap_opt_in in (False, True):
        try:
            expected_channel_entries('shadow', snap_opt_in, authority_active=True)
        except ValueError:
            continue
        raise SystemExit('shadow: a downstream channel plan exists for a shadow release')
    policy = json.loads(CHANNEL_POLICY.read_text(encoding='utf-8'))['release_kinds']['shadow']
    if any(value is not False for value in policy.values()):
        raise SystemExit('shadow: a channel is not denied for a shadow release')
    print('shadow: receipt refuses the kind, no plan exists, every channel is denied')


def run_kind(kind):
    if kind == 'shadow':
        run_shadow()
        print('kind shadow: ok')
        return
    probe_rmux_io_refusal()
    failures, notes = [], []
    for snap_opt_in in (False, True):
        found, worker_notes = run_channels(kind, snap_opt_in)
        failures += found
        notes += worker_notes
    for note in sorted(set(notes)):
        print(note)
    if failures:
        raise SystemExit('\n'.join(failures))
    print(f'kind {kind}: ok')
"#;

#[test]
fn shadow_release_reaches_no_downstream_channel() {
    assert_kind("shadow");
}

#[test]
fn rc_release_seals_every_channel_through_its_worker() {
    assert_kind("rc");
}

#[test]
fn stable_release_seals_every_channel_through_its_worker() {
    assert_kind("stable");
}
