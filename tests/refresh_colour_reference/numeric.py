"""CMY/CMYK and C-whitespace regression: real replies, oracle frozen first."""
import argparse
import json
from pathlib import Path
import shlex
import sys
import uuid
from support import Case, Client, identity, save, sha

ROOT = Path(__file__).resolve().parents[2]
ORACLE = '9cc80ae8fda9aa0843e64294645ded292dbe3ef318aa8cea4ed2738e760f8326'
COLOURS = [
    'cmy:0x1p-1/0/0', 'cmy:0/0x1p-1/0', 'cmy:0/0/0x1p-1',
    'cmyk:0/0/0/0x1p-1', 'cmyk:0x1p-1/0/0/0.5',
    'cmyk:0/0x1p-1/0/0.5', 'cmyk:0/0/0x1p-1/0.5',
    'cmy:0X.8P0/0x4p-3/+0x1.0p-1',
    'cmy:0/0.5/1', 'cmyk:0/0/0/0.5',
    'cmy:0.2/0.4/0.8', 'cmyk:0.2/0.4/0.8/0.1',
    'cmy:0/0/1e', 'cmy:0/0/1e+', 'cmy:0/0/1e-999',
    'cmy:0/0/-1e-999', 'cmy:0/0/0.5tail',
    'cmy:0x/0/0', 'cmy:0/0/0x', 'cmyk:0/0/0/0x',
    'cmy:0x1p1/0/0', 'cmyk:0/0/0/-0x1p-1',
    'cmy:0/0/NaN', 'cmy:0/0/1.01', 'cmyk:0/0/0/-0.01',
    '\v1,2,3', '1,\v2,3', '1,2,\v3',
    'cmy:\v0/0/0', 'cmy:0/\v0/0', 'cmy:0/0/\v0',
    'cmyk:0/0/0/\v0.5', 'gray\v50',
    'cmy:0/0/\u00a00.5', 'gray\u00a050',
    'default', '?', 'rgb:1/2/3',
]


def run(args):
    oracle = args.engine == 'oracle'
    if oracle and sha(args.binary) != ORACLE:
        raise ValueError('wrong oracle binary')
    if not oracle and not args.freeze:
        raise ValueError('candidate needs a frozen oracle')
    frozen = json.loads(Path(args.freeze).read_text()) if args.freeze else None
    if frozen and frozen['oracle'] != ORACLE:
        raise ValueError('wrong oracle authority in freeze')
    case = Case(args.engine, args.binary, 'numeric', str(Path(args.output).resolve()), 180)
    rows = []
    serial = 0
    try:
        worker = case.out/'worker'
        worker.mkdir()
        command = shlex.join([sys.executable, str(ROOT/'tests/refresh_colour_reference/worker.py'), str(worker)])
        case.q('new-session', '-d', '-s', 'p', command, timeout=10)
        case.handshake()
        case.until(lambda: (worker/'ready').exists(), 'worker ready', 5)
        case.producers.append(identity(int((worker/'ready').read_text())))
        case.r['producers'] = case.producers
        control = Client(case, 'p', control=True)
        target = case.q('list-clients', '-t', 'p', '-F', '#{client_name}')
        for slot in [10, 11]:
            for path in ['cli', 'control']:
                for ending in ['\x07', '\x1b\\']:
                    limit = 126 if ending == '\x07' else 125
                    inputs = [('0', colour, ending) for colour in COLOURS]
                    inputs += [('\v0', '#abcdef', ending),
                               ('0', ' '*(limit-3)+'red', ending),
                               ('0', ' '*(limit-2)+'red', ending),
                               ('0', 'cmy:0/0/0.5', '')]
                    for pane, colour, report_end in inputs:
                        for seed_slot in [10, 11]:
                            case.q('refresh-client', '-t', target, '-r', f'%0:\x1b]{seed_slot};#123456\x07')
                        argv = ['refresh-client', '-t', target, '-r', f'%{pane}:\x1b]{slot};{colour}{report_end}']
                        if path == 'cli':
                            case.q(*argv)
                        else:
                            marker = 'NUMERICACK'+uuid.uuid4().hex
                            control.p.stdin.write((shlex.join(argv)+'\ndisplay-message -p '+marker+'\n').encode())
                            control.p.stdin.flush()
                            case.until(lambda: marker.encode() in control.drains[0].raw, 'control barrier')
                            case.require(b'%error ' not in control.drains[0].raw, 'control success')
                        serial += 1
                        query = b'\x1b]10;?\x07\x1b]11;?\x1b\\'
                        save(worker/'request.tmp', {'id': serial, 'bytes': query.hex()})
                        (worker/'request.tmp').replace(worker/'request.json')
                        def received():
                            try:
                                reply = json.loads((worker/'reply.json').read_text())
                                return reply if reply['id'] == serial else None
                            except (FileNotFoundError, ValueError):
                                return None
                        actual = case.until(received, 'real pane reply', 2)['bytes']
                        row = dict(slot=slot, path=path, ending=ending, pane=pane, colour=colour,
                                   report_end=report_end, actual=actual)
                        rows.append(row)
                        if frozen:
                            case.require(row == frozen['rows'][len(rows)-1], 'numeric report exact replies',
                                         expected=frozen['rows'][len(rows)-1], actual=row)
        if frozen:
            case.require(len(rows) == len(frozen['rows']), 'complete frozen matrix')
        case.r['success'] = True
    finally:
        case.close()
        save(Path(args.output)/'observations.json', dict(oracle=ORACLE, rows=rows))
    print(json.dumps(dict(observations=len(rows), success=True)))


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--engine', choices=['oracle', 'candidate'], required=True)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--output', required=True)
    parser.add_argument('--freeze')
    run(parser.parse_args())
