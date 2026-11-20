#!/usr/bin/env python3
"""Bounded macOS L regressions. Run oracle first; candidate requires its freeze.

--build-receipt records a clean source SHA and both binaries built together.
--reference is the original qualified oracle freeze; its literal assertions and
receipts are checked before either engine runs. All output directories are new.
"""
import argparse
import importlib
import json
import os
from pathlib import Path
import resource
import subprocess
import traceback

from support import Case, Skip, identity, save, sha, stamp

ORACLE_SHA = "9cc80ae8fda9aa0843e64294645ded292dbe3ef318aa8cea4ed2738e760f8326"
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--engine', choices=('oracle', 'candidate'), required=True)
    p.add_argument('--binary', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--reference', type=Path, required=True)
    p.add_argument('--freeze', type=Path)
    p.add_argument('--build-receipt', type=Path, required=True)
    p.add_argument('--cases', nargs='+', default=['reentry', 'creation', 'activity'])
    args = p.parse_args()
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    args.output.mkdir(parents=True, exist_ok=False)
    binary = str(args.binary.resolve())
    source = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
    status = subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT, text=True)
    build = json.loads(args.build_receipt.read_text())
    assert build['sha'] == source and build['returncode'] == 0 and status == ''
    assert all(sha(path) == digest for path, digest in build['binaries'].items())
    reference = json.loads(args.reference.read_text())
    assert reference['qualified'] and reference['oracle_binary_sha256'] == ORACLE_SHA
    assert all(sha(path) == digest for path, digest in reference['receipt_hashes'].items())
    hashes = {f.name: sha(f) for f in HERE.glob('*.py')}
    modules = {name: importlib.import_module(name) for name in args.cases}
    expected = {name: module.EXPECTED[name] for name, module in modules.items()}
    for name in ('reentry', 'creation', 'activity'):
        if name in expected:
            assert expected[name] == reference['expected'][name], 'original assertions changed'
    if args.engine == 'oracle':
        assert sha(binary) == ORACLE_SHA
        assert subprocess.check_output([binary, '-V'], text=True).strip() == 'tmux 3.7b'
    else:
        assert sha(binary) == build['binaries'][binary]
        assert args.freeze
        frozen = json.loads(args.freeze.read_text())
        assert frozen['qualified'] and frozen['expected'] == expected
        assert frozen['harness_hashes'] == hashes
        assert frozen['reference_sha256'] == sha(args.reference)
        assert all(sha(path) == digest for path, digest in frozen['receipt_hashes'].items())
    meta = dict(source_sha=source, start=stamp(), engine=args.engine,
                binary=binary, harness_hashes=hashes, expected=expected,
                reference_sha256=sha(args.reference), build_receipt_sha256=sha(args.build_receipt))
    save(args.output / 'run.json', meta)
    results, receipts = {}, {}
    for name, module in modules.items():
        c = Case(args.engine, binary, name, args.output, getattr(module, 'DEADLINE', 20))
        c.r.update(source_sha=source, independent_expected=expected[name], qualified=False)
        try:
            getattr(module, name)(c)
            c.r.update(status='passed', reason='all independent assertions satisfied')
        except Skip as exc:
            c.r.update(status='skipped', reason=str(exc))
        except Exception as exc:
            c.r.update(status='failed', reason=str(exc))
            (c.out / 'exception.txt').write_text(traceback.format_exc())
        finally:
            c.r['server_before_cleanup'] = identity(c.server['pid']) if c.server else None
            c.close()
        results[name] = {k: c.r[k] for k in ('status', 'qualified', 'reason')}
        receipts[str((c.out / 'receipt.json').resolve())] = sha(c.out / 'receipt.json')
        print(json.dumps(dict(case=name, **results[name])), flush=True)
    assert all(sha(path) == digest for path, digest in build['binaries'].items())
    assert subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT, text=True) == status
    meta.update(finish=stamp(), cases=results)
    save(args.output / 'run.json', meta)
    if args.engine == 'oracle' and all(r['status'] == 'passed' and r['qualified'] for r in results.values()):
        save(args.output / 'oracle-freeze.json', dict(qualified=True, expected=expected,
             harness_hashes=hashes, receipt_hashes=receipts, reference_sha256=sha(args.reference)))
    if any(r['status'] == 'skipped' for r in results.values()):
        return 77
    return int(any(r['status'] != 'passed' or not r['qualified'] for r in results.values()))


if __name__ == '__main__':
    raise SystemExit(main())
