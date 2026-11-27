"""Scrollbar mutations measured through formats and the owned pane's native PTY."""
import json
import time

SCROLLBAR_OPTIONS = ('pane-scrollbars', 'pane-scrollbars-position', 'pane-scrollbars-style')
OPTIONS = (*SCROLLBAR_OPTIONS, 'pane-border-status')
CONFIGURATIONS = dict(zip(OPTIONS, ('on', 'left', 'width=2,pad=1', 'top')))
TEMPLATE = ('#{pane_left}|#{pane_top}|#{pane_width}|#{pane_height}|'
            '#{pane_right}|#{pane_bottom}|#{pane_at_left}|#{pane_at_right}|'
            '#{pane_at_top}|#{pane_at_bottom}|#{pane_x}|#{pane_y}')


def prepare(run):
    from options_bank_linux import pin
    (run.root / 'geometry-enabled').write_text(run.contract['default_recipe']['token'])
    configurations = {}
    for name in run.contract['default_recipe']['names']:
        if name in OPTIONS:
            value = CONFIGURATIONS[name]
            path = run.root / (name + '.conf')
            path.write_text('set-option -w -t bank:bank ' + name + ' ' + value + '\n')
            configurations[name] = pin(path)
    run.record['geometry_configurations'] = configurations


def helper_sample(root, token):
    import os
    request = root / 'geometry-request'
    if request.exists():
        index = int(request.read_text())
        destination = root / f'geometry-{index}.json'
        if not destination.exists():
            temporary = root / 'geometry-next.json'
            temporary.write_text(json.dumps({'token': token, 'size': list(os.get_terminal_size(0))}))
            temporary.replace(destination)


def sample(run):
    from options_bank_linux import load, pin, require
    # Both products defer the kernel resize until after command completion.
    time.sleep(.3)
    index = len(run.record.setdefault('geometry_samples', []))
    request = run.root / 'geometry-request-next'
    request.write_text(str(index))
    request.replace(run.root / 'geometry-request')
    path = run.root / f'geometry-{index}.json'
    run.wait(path.exists)
    observed = load(path)
    require(observed['token'] == run.contract['default_recipe']['token'], 'geometry helper token')
    run.record['geometry_samples'].append(pin(path))
    return observed['size']


def plan(commands, entry):
    name = entry['name']
    begin = len(commands)
    target = ['-t', 'bank:bank']
    obligations = {}

    def add(role, argv, expected=None, error=False, size=None):
        index = len(commands)
        row = {'role': name + ':' + role, 'argv': argv, 'error': error}
        if expected is not None:
            row['expected_stdout'] = expected
        if size is not None:
            row['expected_pty_size'] = size
        commands.append(row)
        return index

    def read(role, flags, value=None):
        return add(role, ['show-options', flags, *(target if flags != '-gwv' else []), name], value)

    def change(role, value, flags='-w', error=False):
        return add(role, ['set-option', flags, *target, name, value], error=error)

    def effect(role, geometry, size):
        indices = [add(role, ['display-message', '-p', *target, TEMPLATE], geometry + '\n', size=size)]
        indices.append(add(role + '-list', ['list-panes', *target, '-F', TEMPLATE], geometry + '\n'))
        indices.append(add(role + '-loop', ['display-message', '-p', *target, '#{P:' + TEMPLATE + '}'], geometry + '\n'))
        return indices

    baseline = '0|0|79|24|78|23|1|0|1|1|0|0'
    full = '0|0|80|24|79|23|1|1|1|1|0|0'
    left = '1|0|79|24|79|23|0|1|1|1|1|0'
    narrow = '0|0|77|24|76|23|1|0|1|1|0|0'
    value, other, expected, expected_size, second, second_size = {
        'pane-scrollbars': ('on', 'off', baseline, [79, 24], full, [80, 24]),
        'pane-scrollbars-position': ('left', 'right', left, [79, 24], baseline, [79, 24]),
        'pane-scrollbars-style': ('width=2,pad=1', 'width=1,pad=0', narrow, [77, 24], baseline, [79, 24]),
        'pane-border-status': ('top', 'bottom', '0|1|79|23|78|23|1|0|1|1|0|1', [79, 23],
                               '0|0|79|23|78|22|1|0|1|1|0|0', [79, 23]),
    }[name]
    obligations['default'] = [read('default', '-gwv')]
    for option, setting in zip(SCROLLBAR_OPTIONS, ('on', 'right', 'width=1,pad=0')):
        if option != name:
            add('setup-' + option, ['set-option', '-w', *target, option, setting])
    add('create-consumer', ['new-window', '-d', '-t', 'bank:', '-n', 'effect', '/bin/sleep', '240'])
    obligations['replace'] = [change('replace', value), read('value', '-wv', value + '\n')]
    obligations['effect'] = effect('effect', expected, expected_size)
    obligations['scope'] = [change('global', other, '-gw'), read('global-value', '-gwv', other + '\n'),
                            read('local-value', '-wv', value + '\n'),
                            add('invalid-target', ['set-option', '-w', '-t', 'geometry-missing', name, value], error=True)]
    obligations['effect'] += effect('local-over-global', expected, expected_size)
    if name == 'pane-scrollbars-style':
        obligations['scope'] += [change('pane', other, '-p'), read('pane-value', '-pv', other + '\n')]
        obligations['effect'] += effect('pane-effect', second, second_size)
        add('clear-pane', ['set-option', '-pu', *target, name])
        obligations['effect'] += effect('pane-unset-effect', expected, expected_size)
    obligations['only-if-unset'] = [change('only-existing', other, '-wo', error=True), read('only-value', '-wv', value + '\n')]
    obligations['quiet'] = [change('only-existing-quiet', other, '-wqo'), read('quiet-value', '-wv', value + '\n')]
    obligations['unset'] = [add('unset', ['set-option', '-wu', *target, name]), read('inherited', '-wAv', other + '\n')]
    obligations['effect'] += effect('unset-effect', second, second_size)
    obligations['format'] = [add('format-input', ['set-option', '-g', '@geometry-value', value]),
                             change('format', '#{@geometry-value}', '-wF'), read('format-value', '-wv', value + '\n')]
    obligations['effect'] += effect('format-effect', expected, expected_size)
    sibling = add('sibling', ['set-window-option', *target, name, other])
    obligations['effect'] += effect('sibling-effect', second, second_size)
    obligations['configuration'] = [sibling, add('source', ['source-file', name + '.conf']), read('source-value', '-wv', value + '\n')]
    obligations['effect'] += effect('source-effect', expected, expected_size)
    for border in ('top', 'bottom'):
        add('border-' + border, ['set-option', '-w', *target, 'pane-border-status', border])
        fields = expected.split('|')
        fields[3] = '23'
        if border == 'top':
            fields[1] = fields[11] = '1'
        else:
            fields[1] = fields[11] = '0'
            fields[5] = '22'
        obligations['effect'] += effect('border-' + border + '-effect', '|'.join(fields), [expected_size[0], 23])
        add('border-off-' + border, ['set-option', '-w', *target, 'pane-border-status', 'off'])
    if name == 'pane-scrollbars':
        change('modal', 'modal')
        obligations['effect'] += effect('modal-normal', full, [80, 24])
        add('copy', ['copy-mode', *target])
        obligations['effect'] += effect('modal-copy', baseline, [79, 24])
        add('cancel', ['send-keys', '-X', *target, 'cancel'])
        obligations['effect'] += effect('modal-cancel', full, [80, 24])
    add('close-consumer', ['kill-window', '-t', 'bank:effect'])
    for option in OPTIONS:
        add('restore-local-' + option, ['set-option', '-wu', *target, option])
        add('restore-global-' + option, ['set-option', '-gwu', option])
    return {'id': entry['id'], 'indices': list(range(begin, len(commands))),
            'obligations': obligations, 'status': 'REQUIRED'}


def verify_samples(receipt, contract):
    from options_bank_linux import authenticate, load, require
    observed = [command['pty_size'] for command in receipt['commands'] if 'pty_size' in command]
    artifacts = receipt['geometry_samples']
    require(len(observed) == len(artifacts) and observed, 'native geometry sample population')
    for size, artifact in zip(observed, artifacts):
        authenticate(artifact)
        sample = load(artifact['path'])
        require(sample == {'token': contract['default_recipe']['token'], 'size': size}, 'native geometry sample binding')
    for name, artifact in receipt['geometry_configurations'].items():
        authenticate(artifact)
        value = CONFIGURATIONS[name]
        from pathlib import Path
        require(Path(artifact['path']).read_text() == 'set-option -w -t bank:bank ' + name + ' ' + value + '\n', 'geometry configuration input')
