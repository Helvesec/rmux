"""Freeze tmux's actual PTY replies, then compare the candidate byte for byte."""
import argparse
import json
import os
from pathlib import Path
import shlex
import sys
import subprocess
import time
import uuid
from support import Case, Client, identity, save, sha
from file_exchange import publish_json, read_json

ROOT = Path(__file__).resolve().parents[2]
ORACLE_SHA = '9cc80ae8fda9aa0843e64294645ded292dbe3ef318aa8cea4ed2738e760f8326'
QUERY = b'\x1b]10;?\x07\x1b]11;?\x1b\\\x1b]12;?\x07'


def run(args):
    if args.engine == 'candidate' and not args.freeze:
        raise ValueError('candidate execution requires a frozen oracle')
    if args.engine == 'oracle' and sha(args.binary) != ORACLE_SHA:
        raise ValueError('oracle executable does not match the pinned tmux 3.7b authority')
    reference = json.loads(Path(args.freeze).read_text()) if args.freeze else None
    if reference and reference['oracle_binary_sha256'] != ORACLE_SHA:
        raise ValueError('reference does not bind the pinned oracle')
    c = Case(args.engine, args.binary, 'reports', str(Path(args.output).resolve()), 180)
    serial, results = 0, []
    def worker_command(directory):
        argv = [sys.executable, str(ROOT/'tests/refresh_colour_reference/worker.py'), str(directory)]
        return subprocess.list2cmdline(argv) if os.name == 'nt' else shlex.join(argv)
    clients = []

    def observe(label, value, product=None):
        row = {'label': label, 'actual': value, 'expected': value if product is None else product}
        if product is not None:
            row['classification'] = 'existing-unknown-contract' if label == 'unknown' else 'product-divergence'
        results.append(row)
        if reference is not None:
            expected = reference['results'][len(results) - 1]
            c.require(expected['label'] == label and value == expected['expected'], label,
                      expected=expected['expected'], actual=value)

    def emit(pane='p', payload=QUERY):
        nonlocal serial
        serial += 1
        d = c.out / pane
        publish_json(d / 'request.json', {'id': serial, 'bytes': payload.hex()})
        def got():
            try:
                r = read_json(d / 'reply.json')
                return r if r['id'] == serial else None
            except (FileNotFoundError, ValueError):
                return None
        return c.until(got, 'real pane replies', 2)['bytes']

    def control(command, client):
        marker = 'RACK' + uuid.uuid4().hex
        client.p.stdin.write((command + "\ndisplay-message -p '" + marker + "'\n").encode())
        client.p.stdin.flush()
        c.until(lambda: marker.encode() in client.drains[0].raw, 'queued command barrier')
        c.require(b'%error ' not in client.drains[0].raw, 'successful control framing')

    def report(value, path='cli', extra=()):
        command = ['refresh-client', '-t', target, '-r', value, *extra]
        if path == 'cli': c.q(*command)
        elif path == 'alias': c.q('refresh', '-t'+target, '-r'+value)
        elif path == 'argv-queue': c.q('display-message','-p','before',';',*command,';','display-message','-p','after')
        elif path == 'control': control(shlex.join(command), a)
        elif path == 'source':
            config = c.out / 'report.conf'
            config.write_text(shlex.join(command)+'\n')
            c.q('source-file',str(config))
        elif path == 'control-source':
            config = c.out / 'report.conf'
            config.write_text(shlex.join(command)+'\n')
            control(shlex.join(['source-file',str(config)]),a)
        elif path == 'binding':
            c.q('bind-key','-n','F8',*command)
            c.q('send-keys','-K','-c',target,'F8')
        else: raise AssertionError(path)

    try:
        for name in ['p','q']:
            d = c.out / name
            d.mkdir()
            c.q('new-session','-d','-s',name,worker_command(d),timeout=10)
            if c.server is None: c.handshake()
            c.until(lambda:(d/'ready').exists(),'worker ready')
            c.producers.append(identity(int((d/'ready').read_text())))
        c.handshake()
        a = Client(c,'p',control=True); b = Client(c,'q',control=True)
        target = c.q('list-clients','-F','#{client_name}','-t','p')
        observe('unknown',emit(),product='')
        report('%0:\x1b]10;#113355\x07'); report('%0:\x1b]11;#224466\x07')
        report('%1:\x1b]10;#abcdef\x07'); report('%1:\x1b]11;#fedcba\x07')
        observe('seed',emit()); observe('other-seed',emit('q'))
        specs = ['rgb:12/34/ab','rgb:1234/5678/abcd','#123456','#123456789abc','red','LightBlue','light blue',' BLUE ','grey50','gray100','rgb:1/2/3','rgb:123/456/789','RGB:12/34/56','default','colour42','?','','1,2,3','256,257,258','-1,2,3','1,2,3tail','cmy:0/0.5/1','cmyk:0/0/0/0.5','rgbi:1/0/0','rgb: 1/ 2/ 3','rgb:01/02/3x','rgb:01/02/03tail']
        for index, spec in enumerate(specs):
            report('%0:\x1b]10;'+spec+'\x07')
            observe('grammar-'+str(index),emit())
        for label, value in [
            ('bg-st','%0:\x1b]11;#012345\x1b\\'),('cursor-noop','%0:\x1b]12;red\x07'),
            ('palette-noop','%0:\x1b]4;3;red\x07'),('plain-noop','%0:rgb:12/34/56'),
            ('partial-noop','%0:\x1b]10;green'),('first-only','%0:\x1b]10;blue\x07\x1b]11;red\x07'),
            ('pane-suffix','%0junk:\x1b]10;red\x07'),('pane-sign','%+0:\x1b]10;blue\x07'),
            ('pane-space','% 0:\x1b]10;green\x07'),('missing-pane','%900000:\x1b]10;red\x07'),
            ('bel-126','%0:\x1b]10;'+(' '*123)+'red\x07'),('bel-127-noop','%0:\x1b]10;'+(' '*123)+'blue\x07'),
            ('st-125','%0:\x1b]10;'+(' '*122)+'red\x1b\\'),('st-126-noop','%0:\x1b]10;'+(' '*122)+'blue\x1b\\'),
            ('large-noop','%0:\x1b]10;'+('x'*1024)+'\x07'),
        ]:
            report(value); observe(label,emit())
        observe('other-pane-isolated',emit('q'))
        target_b = c.q('list-clients','-F','#{client_name}','-t','q')
        control(shlex.join(['refresh-client','-t',target_b,'-r','%0:\x1b]10;#b1c2d3\x07']),b)
        observe('other-client-last-writer',emit())
        observe('other-client-own-pane-unchanged',emit('q'))
        for index, path in enumerate(['cli','alias','argv-queue','control','source','control-source']):
            report('%0:\x1b]10;#1234'+str(index)*2+'\x07',path)
            observe('path-'+path,emit())
        report('%0:\x1b]11;#a1b2c3\x07',extra=['-A','%1:off','-B','r:%0:#{pane_id}','-C','81x25','-f','no-output'])
        observe('combined-A-B-C-flags',emit())
        # Restore neighboring delivery flags, keeping the colour reports.
        c.q('refresh-client','-t',target,'-A','%1:on','-f','!no-output')
        observe('flags-restored',emit())
        app = b'\x1b]10;rgb:1010/2020/3030\x07\x1b]11;rgb:4040/5050/6060\x07\x1b]12;rgb:7070/8080/9090\x07'
        observe('app-precedence',emit(payload=app+QUERY))
        observe('app-reset',emit(payload=b'\x1b]110;\x07\x1b]111;\x07\x1b]112;\x07'+QUERY))
        # Complete a fragmented OSC query with the report in force at completion.
        observe('query-fragment-prefix',emit(payload=b'\x1b]10;'))
        report('%0:\x1b]10;#f1e2d3\x07','control')
        observe('query-fragment-finish',emit(payload=b'?\x1b\\'))
        observe('query-dispatch-at-esc',emit(payload=b'\x1b]10;?\x1b'))
        report('%0:\x1b]10;#102938\x07')
        observe('query-st-slash',emit(payload=b'\\'))
        c.q('switch-client','-c',target,'-t','q'); observe('session-switch',emit())
        c.q('detach-client','-t',target); a.close(); c.clients.remove(a)
        observe('reporter-detached',emit())
        target_b=c.q('list-clients','-F','#{client_name}','-t','q')
        c.q('detach-client','-t',target_b); b.close(); c.clients.remove(b)
        observe('all-detached',emit())
        d=Client(c,'q',control=True)
        observe('replacement-reactivates',emit())
        (c.out/'p'/'ready').unlink()
        c.q('respawn-pane','-k','-t','%0',worker_command(c.out/'p'))
        c.until(lambda:(c.out/'p'/'ready').exists(),'respawn worker ready')
        c.producers.append(identity(int((c.out/'p'/'ready').read_text())))
        observe('same-pane-respawn',emit())
        (c.out/'p'/'ready').unlink()
        c.q('respawn-window','-k','-t','p',worker_command(c.out/'p'))
        c.until(lambda:(c.out/'p'/'ready').exists(),'window respawn worker ready')
        c.producers.append(identity(int((c.out/'p'/'ready').read_text())))
        observe('same-pane-window-respawn',emit())
        c.q('kill-session','-t','p')
        e = c.out/'newp'; e.mkdir()
        c.q('new-session','-d','-s','p',worker_command(e))
        c.until(lambda:(e/'ready').exists(),'replacement pane ready')
        c.producers.append(identity(int((e/'ready').read_text())))
        target=c.q('list-clients','-F','#{client_name}','-t','q')
        observe('retired-pane-new-unknown',emit('newp'),product='')
        report('%0:\x1b]10;red\x07')
        observe('retired-id-no-alias',emit('newp'),product='')
        report('%2:\x1b]10;#abcdef\x07'); report('%2:\x1b]11;#13579b\x07')
        observe('new-pane-addressable',emit('newp'))
        if args.attached:
            plain=Client(c,'p')
            c.wait_clients([d,plain])
            target=plain.tty
            report('%2:\x1b]10;#213243\x07')
            observe('attached-target',emit('newp'))
            report('%2:\x1b]10;#435465\x07','binding')
            observe('binding-path',emit('newp'))
            c.q('detach-client','-t',target); plain.close(); c.clients.remove(plain)

        c.handshake()
        c.require(len(results) == (len(reference['results']) if reference else len(results)), 'complete matrix')
        c.r['success'] = True
    finally:
        c.close()
        save(Path(args.output)/'freeze.json',{'engine':args.engine,'binary_sha256':sha(args.binary),'oracle_binary_sha256':ORACLE_SHA,'source_authority':'e802909de06012a4df6209d55e86487c56223163','attached':args.attached,'results':results})
    print(json.dumps({'cases':len(results),'assertions':len(c.r['assertions']),'success':True}))


if __name__ == '__main__':
    p=argparse.ArgumentParser()
    p.add_argument('--engine',choices=['oracle','candidate'],required=True)
    p.add_argument('--binary',required=True)
    p.add_argument('--output',required=True)
    p.add_argument('--freeze')
    p.add_argument('--attached',action='store_true')
    run(p.parse_args())
