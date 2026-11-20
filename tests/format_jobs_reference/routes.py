"""Independent five-route corpus on the existing bounded Linux runner."""
import base64
import hashlib
import json
import os
from pathlib import Path
import shlex

from bounded import expand, pin, read_pinned, require, qualified_argv

ROUTES = ('startup', 'cli', 'source-file', 'queue', 'binding')


def load_corpus(ref):
    corpus = read_pinned(ref)
    require(corpus.get('schema') == 1 and corpus.get('routes') == list(ROUTES),
            'complete independent five-route corpus required')
    cases = corpus.get('cases', [])
    ids = [case.get('id') for case in cases]
    require(ids and len(ids) == len(set(ids)) and
            all(isinstance(x, str) and x and all(c.isalnum() or c == '-' for c in x)
                for x in ids), 'unique independent corpus cases')
    require(all(isinstance(case.get('format'), str) and case['format'] for case in cases),
            'independent expression required')
    return corpus


def text(argv):
    return shlex.join(argv)


def action(case, target, route):
    name = '@replay-' + case['id']
    done = '@done-' + case['id']
    return [
        ['set-option', '-F', '-t', target, name, case['format']],
        ['set-option', '-t', target, done, route + '-' + case['id']],
    ]


def route_steps(corpus):
    target = corpus['target']
    steps = []
    for route in ROUTES:
        if route == 'binding':
            steps.append({'kind': 'attach', 'session': corpus['session']})
        for case in corpus['cases']:
            steps.append(dict(kind='route', id=route + '-' + case['id'], route=route,
                              target=target, case=case, argv=action(case, target, route)[0],
                              success={'exit': 0, 'stderr': ''}))
    for error in corpus['errors']:
        require(error['route'] in ('source-file', 'queue') and
                error['type'] in ('runtime', 'parse'), 'bounded source/queue error case')
        steps.append(dict(kind='route', id=error['route'] + '-' + error['type'] + '-error',
                          route=error['route'], target=target, error=error['type'],
                          success={'exit': 0, 'stderr': ''}))
    return steps


def startup(corpus):
    value = corpus['startup']
    for case in corpus['cases']:
        value += '\n'.join(text(argv) for argv in action(case, corpus['target'], 'startup')) + '\n'
    return value


def validate_contract(contract):
    corpus = load_corpus(contract['independent_corpus'])
    require(contract['startup'] == startup(corpus) and
            contract['steps'] == route_steps(corpus), 'exact independent corpus route population')
    expected = read_pinned(contract['independent_expected'])
    require(expected.get('schema') == 1 and
            expected.get('corpus_sha256') == contract['independent_corpus']['sha256'] and
            expected.get('tmux', {}).get('version') == 'tmux 3.7b' and
            expected['tmux'].get('sha256') == contract['binaries']['oracle']['sha256'] and
            [row['id'] for row in expected['observations']] ==
            [step['id'] for step in contract['steps'] if step['kind'] == 'route'],
            'complete frozen independent oracle required')
    return corpus


def validate_values(contract, steps, values):
    expected = read_pinned(contract['independent_expected'])
    actual = [dict(id=step['id'], values=[
              [category, code, base64.b64encode(out).decode(), base64.b64encode(err).decode()]
              for category, code, out, err in value])
              for step, (value, _) in zip(steps, values)]
    require(len(actual) == len(expected['observations']), 'missing independent oracle observation')
    failures = [row['id'] for row, frozen in zip(actual, expected['observations']) if row != frozen]
    require(not failures, 'frozen tmux replay differs: ' + ', '.join(failures))


def entry_argv(step, root):
    """One serialization for both execution and receipt validation."""
    if 'error' in step:
        before, after = '@before-' + step['id'], '@after-' + step['id']
        commands = [['set-option', '-t', step['target'], before, 'entered']]
        if step['error'] == 'runtime':
            commands.append(['set-option', '-t', step['target'], 'invalid-option043', 'bad'])
        else:
            commands.append(['invalid-command043'])
        commands.append(['set-option', '-t', step['target'], after, 'continued'])
    else:
        commands = action(step['case'], step['target'], step['route'])
    route = step['route']
    if route == 'startup':
        return [], None
    if route == 'cli':
        return commands, None
    if route == 'source-file':
        path = root / (step['id'] + '.conf')
        return [['source-file', str(path)]], '\n'.join(text(c) for c in commands) + '\n'
    command = '; '.join(text(c) for c in commands)
    argv = ['if-shell', '-F', '-t', step['target'], '1', command]
    if route == 'binding':
        argv = ['bind-key', '-n', 'x', *argv]
    return [argv], None


def effect_argv(step):
    if 'error' in step:
        return [['show-options', '-qv', '-t', step['target'], '@' + prefix + step['id']]
                for prefix in ('before-', 'after-')]
    name = '@replay-' + step['case']['id']
    # Presence is distinct from an empty expansion. The direct formatter is a
    # neighboring witness, while the stored value proves the requested route.
    return [
        ['show-options', '-qv', '-t', step['target'], name],
        ['show-options', '-q', '-t', step['target'], name],
        ['display-message', '-p', '-t', step['target'], step['case']['format']],
        ['show-options', '-qv', '-t', step['target'], '@done-' + step['case']['id']],
    ]


def execute(run, step):
    route = step['route']
    if route != 'startup':
        if 'error' in step:
            names = ['@before-' + step['id'], '@after-' + step['id']]
        else:
            names = ['@replay-' + step['case']['id'], '@done-' + step['case']['id']]
        for name in names:
            run.q(['set-option', '-u', '-t', step['target'], name])
    entries, contents = entry_argv(step, run.root)
    observation = dict(id=step['id'], kind='route', route=route, entries=[], effects=[])
    if contents is not None:
        path = run.root / (step['id'] + '.conf')
        require(not path.exists(), 'fresh source route fixture')
        path.write_text(contents)
        observation['file'] = pin(path)
    for argv in entries:
        observation['entries'].append(len(run.r['commands']))
        run.q(argv, step['id'] + '-entry', check=False)
    if route == 'binding':
        require(len(run.clients) == 1, 'one qualified binding client')
        client = run.clients[0]
        require(os.write(client.master, b'x') == 1, 'binding key delivery')
        run.r.setdefault('key_inputs', []).append(dict(id=step['id'], pid=client.p.pid, data_hex='78'))
        marker = effect_argv(step)[-1]
        expected = (step['id'] + '\n').encode()
        run.wait(lambda: run.q(marker, step['id'] + '-boundary') == expected,
                 'binding evaluation boundary')
    for argv in effect_argv(step):
        observation['effects'].append(len(run.r['commands']))
        run.q(argv, step['id'], check=False)
    observation['command'] = observation['effects'][0]
    run.r['observations'].append(observation)


def observation_value(plan, contract, phase, receipt, step, obs, last):
    root = Path(plan['roots'][phase])
    prefix = [contract['binaries'][phase]['path'], '-N', '-S', str(root / 'socket')]
    require(set(obs) == {'id', 'kind', 'route', 'entries', 'effects', 'command'} |
            ({'file'} if step['route'] == 'source-file' else set()) and
            obs['kind'] == 'route' and obs['route'] == step['route'], 'route observation identity')
    entries, contents = entry_argv(step, root)
    if contents is not None:
        path = root / (step['id'] + '.conf')
        require(obs['file'] == dict(path=str(path), bytes=len(contents.encode()),
                sha256=hashlib.sha256(contents.encode()).hexdigest()),
                'source replay bytes/argv binding')
    effects = effect_argv(step)
    require(len(obs['entries']) == len(entries) and len(obs['effects']) == len(effects) and
            obs['command'] == obs['effects'][0], 'complete route entry/effect population')
    if step['route'] == 'binding':
        keys = [x for x in receipt.get('key_inputs', []) if x['id'] == step['id']]
        require(len(keys) == 1 and keys[0]['data_hex'] == '78' and
                keys[0]['pid'] == receipt['processes']['attached-client']['birth']['pid'],
                'owned binding activation required')
    values = []
    for category, indices, argvs in [('entry', obs['entries'], entries),
                                    ('effect', obs['effects'], effects)]:
        for index, argv in zip(indices, argvs):
            require(type(index) is int and last < index < len(receipt['commands']),
                    'ordered route command indices')
            last = index
            command = receipt['commands'][index]
            require(command.get('argv') == prefix + expand(argv, root) and
                    command.get('label') == step['id'] + ('-entry' if category == 'entry' else ''),
                    'route command/target identity')
            require(command.get('launch_argv') == qualified_argv(contract, phase,
                    command['argv'], check_context=False) and
                    command.get('gone_after_wait') is True and
                    type(command.get('waited_pid')) is int and
                    type(command.get('parent_pid')) is int, 'route launch and direct wait required')
            require(type(command.get('exit')) is int and not command.get('timeout'),
                    'completed route command required')
            raw = [base64.b64decode(command[name], validate=True) for name in ('stdout', 'stderr')]
            if category == 'effect':
                require(command['exit'] == 0 and not raw[1], 'route effect observation failed')
            elif 'error' not in step:
                require(command['exit'] == 0 and not raw[1], 'route evaluation failed')
            else:
                require(command['exit'] != 0 and any(raw), 'source/queue error must be observable')
            # Only the authenticated phase root is canonicalized; all original
            # bytes remain in the receipt. No errors or output are discarded.
            values.append((category, command['exit'],
                           *(data.replace(str(root).encode(), b'@ROOT@') for data in raw)))
    if 'error' not in step:
        done = receipt['commands'][obs['effects'][-1]]['stdout']
        require(base64.b64decode(done, validate=True) == (step['id'] + '\n').encode(),
                'actual evaluation completion marker')
        present = receipt['commands'][obs['effects'][1]]['stdout']
        require(base64.b64decode(present, validate=True), 'evaluation option must exist even when empty')
    return values, last


def corpus_main(argv):
    """Produce a bounded contract; existing reader and Linux entrypoints replay it."""
    import argparse
    from support import save
    from format_jobs_reference import digest
    parser = argparse.ArgumentParser(description=corpus_main.__doc__)
    parser.add_argument('corpus', type=Path)
    parser.add_argument('source', type=Path)
    parser.add_argument('head')
    parser.add_argument('oracle', type=Path)
    parser.add_argument('candidate', type=Path)
    parser.add_argument('out', type=Path)
    args = parser.parse_args(argv)
    require(not args.out.exists(), 'fresh bounded corpus contract')
    ref = pin(args.corpus)
    corpus = load_corpus(ref)
    harness_root = Path(__file__).parent.parent
    harness = {name: pin(harness_root / name) for name in digest()}
    contract = dict(schema=1, independent_corpus=ref,
                    independent_expected=pin(args.corpus.with_name('formats_independent_routes_expected.json')),
                    source=dict(path=str(args.source.resolve()), head=args.head),
                    startup=startup(corpus), startup_helpers=['main'], steps=route_steps(corpus),
                    binaries=dict(oracle=pin(args.oracle.resolve()), candidate=pin(args.candidate.resolve())),
                    shell=pin('/usr/bin/bash'), python=pin('/usr/bin/python3'), harness=harness,
                    limits=dict(seconds=300, helpers_per_phase=2, commands_per_phase=2000,
                                bytes_per_phase=2097152),
                    environment=dict(PATH='/usr/local/bin:/usr/bin:/bin', TERM='xterm-256color',
                                     LC_ALL='C.UTF-8', HOME='@ROOT@/home', XDG_CONFIG_HOME='@ROOT@/xdg',
                                     RMUX_DISABLE_TMUX_FALLBACK='1'))
    validate_contract(contract)
    if os.environ.get('RMUX_IDENTITY_HEAD'):
        require(os.environ['RMUX_IDENTITY_HEAD'] == args.head and
                Path(os.environ['RMUX_IDENTITY_SOURCE']).resolve() == args.source.resolve(),
                'corpus source differs from qualified candidate context')
        contract['candidate_identity'] = dict(source=str(args.source.resolve()), head=args.head,
            target=os.environ['RMUX_IDENTITY_TARGET'],
            script=pin(args.source / 'scripts/candidate_identity.py'), python=pin('/usr/bin/python3'))
    save(args.out, contract)
    print(json.dumps(pin(args.out)))
    return 0
