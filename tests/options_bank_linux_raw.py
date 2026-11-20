"""Observe the existing owned raw terminal application's actual input bytes."""
import time

OPTIONS = {'extended-keys', 'extended-keys-format', 'xterm-keys'}


def prepare(run):
    from options_bank_linux import save, pin
    names = set(run.contract['default_recipe']['names']) & OPTIONS
    if not names:
        return
    request = '1b5b3e346e1b5b3e343b326d'
    path = run.root / 'raw-input.json'
    save(path, dict(token=run.contract['default_recipe']['token'], request_hex=request))
    run.record['raw_configuration'] = pin(path)


def observe(run, command):
    from options_bank_linux import pin, load
    time.sleep(.25)
    artifacts, payload = [], ''
    for index in range(run.raw_mark, len(list(run.root.glob('input-*.json')))):
        path = run.root / f'input-{index}.json'
        artifacts.append(pin(path))
        payload += load(path)['hex']
    command.update(raw_input=payload, raw_artifacts=artifacts, raw_begin=run.raw_mark)


def verify(receipt, command, root, contract):
    from pathlib import Path
    from options_bank_linux import authenticate, require, load
    configuration = receipt['raw_configuration']
    authenticate(configuration)
    settings = load(configuration['path'])
    names = set(contract['default_recipe']['names']) & OPTIONS
    request = '1b5b3e346e1b5b3e343b326d'
    require(configuration['path'] == root + '/raw-input.json'
            and settings == dict(token=contract['default_recipe']['token'], request_hex=request), 'raw configuration')
    payload = ''
    for index, artifact in enumerate(command['raw_artifacts'], command['raw_begin']):
        authenticate(artifact)
        require(artifact['path'] == str(Path(root) / f'input-{index}.json'), 'raw input event target')
        payload += load(artifact['path'])['hex']
    require(payload == command['raw_input'], 'raw input event replay')


def plan(commands, entry):
    from options_bank_linux_recipe import Recipe
    name = entry['name']
    first, second = ('off', 'always') if name == 'extended-keys' else ('csi-u', 'xterm') if name == 'extended-keys-format' else ('off', 'on')
    r = Recipe(commands, entry, 'window' if name == 'xterm-keys' else 'server', first, second)
    r.mutations()
    if name == 'extended-keys-format':
        r.add('force-extended', ['set-option', '-s', 'extended-keys', 'always'])
    r.obligations['effect'] = []
    for role, value in [('effect', first), ('second-effect', second), ('restored-effect', first)]:
        r.obligations['effect'].append(r.set(role + '-value', value))
        if name == 'xterm-keys':
            r.obligations['effect'].append(r.read(role + '-read', value))
        r.add(role + '-application-request', ['send-keys', '-H', '-t', 'bank:0', 'ff'])
        r.add(role + '-stimulus', ['send-keys', '-t', 'bank:0',
                                  'C-Left' if name == 'xterm-keys' else 'C-a'], attach_client=True, raw_mark=True)
        checks = dict(raw_observation=True)
        if name == 'xterm-keys':
            checks['expected_raw'] = '1b5b313b3544'
        checks['raw_differ_from' if role == 'second-effect' and name != 'xterm-keys'
               else 'raw_equal_to'] = name + ':effect'
        if role == 'effect':
            checks.pop('raw_equal_to')
        r.obligations['effect'].append(r.add(role, ['capture-pane', '-p', '-t', 'bank:0'], **checks))
    return r.finish()
