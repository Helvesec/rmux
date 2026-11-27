#![cfg(unix)]

use std::process::Command;

#[test]
fn retry_boundary_fixtures_preserve_identity_while_writer_is_active() {
    let output = Command::new("python3")
        .args([
            "-B",
            "-c",
            r####"
import importlib.util
import json
import os
import pathlib
import re
import subprocess
import threading
from unittest.mock import patch

checkout = pathlib.Path.cwd().resolve()
spec = importlib.util.spec_from_file_location(
    'guard', checkout / 'scripts/candidate_identity.py')
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)
head = guard.git(checkout, 'rev-parse', 'HEAD')
guard.source_identity(checkout, head)
source = (checkout / 'tests/release_retry_input_boundary_spec.rs').read_text()
population = (
    'retry_public_validator_preserves_r4_yaml_expression_boundaries',
    'retry_public_validator_bounds_quotes_without_narrowing_multiline_yaml',
)
failures = []
for name in population:
    body = source.split('fn ' + name + '() {', 1)[1]
    snippet = re.search(r'r###"(.*?)"###', body, re.S).group(1)
    written, resume = threading.Event(), threading.Event()
    record, errors = {}, []
    original = pathlib.Path.write_bytes

    def write(path, data):
        result = original(path, data)
        if not written.is_set():
            record.update(writer=name, pid=os.getpid(),
                          thread=threading.get_native_id(), path=str(path.resolve()))
            written.set()
            if not resume.wait(30):
                raise TimeoutError('identity observation did not release fixture writer')
        return result

    def run():
        try:
            exec(compile(snippet, name, 'exec'), {})
        except BaseException as error:
            errors.append(repr(error))
        finally:
            written.set()

    with patch.object(pathlib.Path, 'write_bytes', write):
        writer = threading.Thread(target=run)
        writer.start()
        try:
            assert written.wait(30) and record, (name, errors)
            record['status'] = guard.git(
                checkout, 'status', '--porcelain', '--untracked-files=all')
            try:
                guard.source_identity(checkout, head)
                record['identity'] = 'PASS'
            except guard.IdentityError as error:
                record['identity'] = str(error)
                failures.append(dict(record))
        finally:
            resume.set()
            writer.join(30)
        assert not writer.is_alive() and not errors, (name, errors)
    guard.source_identity(checkout, head)
    record['after_cleanup'] = guard.git(checkout, 'status', '--porcelain')
    print(json.dumps(record, sort_keys=True), flush=True)
assert not failures, json.dumps(failures, sort_keys=True)
"####,
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("observe retry fixtures during their writes");
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn release_fixtures_keep_checkout_clean_during_creation_and_after_cleanup() {
    let output = Command::new("python3")
        .args([
            "-B",
            "-c",
            r#"
import pathlib
import re
import subprocess
import tempfile

checkout = pathlib.Path.cwd().resolve()
population = {
    'release_downstream_payload_spec.rs': 4,
    'release_downstream_adversarial_spec.rs': 2,
    'release_downstream_summary_spec.rs': 3,
    'release_downstream_result_evidence_spec.rs': 4,
    'release_downstream_writer_spec.rs': 4,
    'release_linux_repository_recovery_spec.rs': 3,
}

def status():
    return subprocess.run(
        ['git', 'status', '--porcelain=v1', '--untracked-files=all'],
        check=True, capture_output=True, text=True,
    ).stdout

baseline = status()
failures = []
checked = 0
for filename, expected in population.items():
    source = (checkout / 'tests' / filename).read_text(encoding='utf-8')
    constructors = re.findall(
        r'with (tempfile\.TemporaryDirectory\([^\n]*?\)) as \w+:', source,
    )
    assert len(constructors) == expected, (filename, len(constructors), expected)
    for index, constructor in enumerate(constructors):
        # Evaluate the constructor used by the fixture, while its directory exists.
        with eval(constructor) as directory:
            root = pathlib.Path(directory).resolve()
            (root / 'payload-probe.json').write_text('{}\n', encoding='utf-8')
            during = status()
            if root.is_relative_to(checkout) or during != baseline:
                failures.append(f'{filename}[{index}] during creation: {during!r}')
        after = status()
        if after != baseline:
            failures.append(f'{filename}[{index}] after cleanup: {after!r}')
        checked += 1

assert not failures, '\n'.join(failures)
assert checked == sum(population.values())
print(f'{checked} fixture directories outside checkout; status unchanged during/after')
"#,
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run release fixture isolation regression");
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
