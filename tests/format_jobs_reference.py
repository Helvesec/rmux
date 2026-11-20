#!/usr/bin/env python3
"""Freeze Mac tmux 3.7b observations, then replay on a dedicated Linux candidate."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
import traceback

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / 'format_jobs_reference'))
from support import Case, save, sha
from scenarios import SCENARIOS
from product import PRODUCT_SCENARIOS

PIN = '9cc80ae8fda9aa0843e64294645ded292dbe3ef318aa8cea4ed2738e760f8326'

def digest():
    files = [Path(__file__), *sorted((ROOT / 'format_jobs_reference').glob('*.py'))]
    return {str(file.relative_to(ROOT)): sha(file) for file in files}

def main():
    if sys.argv[1:2] == ['bounded-corpus']:
        from routes import corpus_main
        return corpus_main(sys.argv[2:])
    if sys.argv[1:2] == ['bounded-reader']:
        from bounded import reader_main
        return reader_main(sys.argv[2:])
    if sys.argv[1:2] == ['bounded-linux']:
        from bounded import main as bounded_main
        return bounded_main(sys.argv[2:])
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--engine', choices=['oracle', 'candidate'], required=True)
    p.add_argument('--binary', required=True)
    p.add_argument('--out', type=Path, required=True)
    p.add_argument('--socket-root', type=Path, required=True)
    p.add_argument('--oracle', type=Path)
    p.add_argument('--case', choices=list(SCENARIOS) + list(PRODUCT_SCENARIOS), action='append')
    args = p.parse_args()
    if args.out.exists():
        p.error('output directory must be fresh')
    if args.engine == 'oracle':
        assert sys.platform == 'darwin' and sha(args.binary) == PIN
    else:
        assert args.oracle, 'candidate replay requires frozen oracle'
        oracle = json.loads(args.oracle.read_text())
        assert oracle['passed'] and oracle['harness'] == digest()
    os.environ['FORMAT_JOBS_SOCKET_ROOT'] = str(args.socket_root.resolve())
    result = dict(engine=args.engine, binary=str(Path(args.binary).resolve()),
                  harness=digest(), cases=[], passed=True)
    scenarios = dict(SCENARIOS)
    if args.engine == 'candidate':
        scenarios.update(PRODUCT_SCENARIOS)
    for name in args.case or scenarios:
        c = Case(args.engine, args.binary, name, args.out, 55)
        try:
            scenarios[name](c)
            c.r['passed'] = True
        except Exception as exc:
            c.r.update(passed=False, error=str(exc), traceback=traceback.format_exc())
            result['passed'] = False
        finally:
            c.close()
        integrity = c.r['binary_sha256_before'] == c.r['binary_sha256_after']
        if c.server:
            integrity = integrity and c.r['server_sha256_before'] == c.r['server_sha256_after']
            integrity = integrity and not Path(c.socket).exists()
        integrity = integrity and all(item.get('owned_process_gone', True) for item in c.r['cleanup'])
        c.r['integrity_and_cleanup'] = bool(integrity)
        c.r['passed'] = c.r['passed'] and integrity
        result['passed'] = result['passed'] and c.r['passed']
        save(c.out / 'receipt.json', c.r)
        result['cases'].append(dict(name=name, passed=c.r['passed'],
                                    assertions=len(c.r['assertions']), error=c.r.get('error')))
        print(json.dumps(result['cases'][-1]), flush=True)
    result['passed'] = result['passed'] and result['harness'] == digest()
    save(args.out / 'result.json', result)
    return 0 if result['passed'] else 1

if __name__ == '__main__':
    sys.exit(main())
