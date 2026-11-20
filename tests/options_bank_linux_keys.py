"""Key table and timer recipes consumed by an attached client's bindings."""

OPTIONS = {'prefix', 'prefix2', 'key-table', 'user-keys', 'repeat-time',
           'initial-repeat-time', 'prefix-timeout', 'escape-time', 'assume-paste-time',
           'mouse', 'focus-follows-mouse'}


def plan(commands, entry):
    from options_bank_linux_recipe import Recipe
    name = entry['name']
    if name in ('escape-time', 'assume-paste-time', 'mouse', 'focus-follows-mouse'):
        return input_plan(commands, entry)
    scope = 'server' if name in ('user-keys', 'prefix-timeout') else 'session'
    first, second = {'prefix': ('C-a', 'C-b'), 'prefix2': ('C-a', 'C-c'),
                     'key-table': ('first', 'second'), 'user-keys': ('\x1b[901~', '\x1b[902~'),
                     'repeat-time': ('0', '1000'), 'initial-repeat-time': ('50', '1000'),
                     'prefix-timeout': ('0', '50')}[name]
    r = Recipe(commands, entry, scope, first, second)
    r.mutations()
    r.add('bind-root', ['bind-key', '-T', 'root', 'x', 'rename-window', '-t', 'bank:0', 'ROOT'])
    r.add('bind-prefix', ['bind-key', '-r', '-T', 'prefix', 'x', 'rename-window', '-t', 'bank:0', 'PREFIX'])
    if name == 'key-table':
        for table in ('first', 'second'):
            r.add('bind-' + table, ['bind-key', '-T', table, 'x', 'rename-window', '-t', 'bank:0', table.upper()])
    if name == 'user-keys':
        r.add('bind-user', ['bind-key', '-T', 'root', 'User0', 'rename-window', '-t', 'bank:0', 'USER'])
    if name == 'prefix2':
        r.add('disable-primary', ['set-option', '-g', 'prefix', 'None'])
    if name in ('repeat-time', 'initial-repeat-time', 'prefix-timeout'):
        r.add('stable-prefix', ['set-option', '-g', 'prefix', 'C-b'])
        if name != 'repeat-time':
            r.add('stable-repeat', ['set-option', '-g', 'repeat-time', '0' if name == 'prefix-timeout' else '1000'])
    r.obligations['effect'] = []
    for role, value in [('effect', first), ('second-effect', second), ('restored-effect', first)]:
        r.obligations['effect'] += [r.set(role + '-value', value),
                                   r.add(role + '-reset-name', ['rename-window', '-t', 'bank:0', 'bank'])]
        if name in ('prefix', 'prefix2'):
            frames = [dict(hex='0178', after_ms=150)]
            expected = 'PREFIX' if value == first else 'ROOT'
        elif name == 'key-table':
            frames, expected = [dict(hex='78', after_ms=150)], value.upper()
        elif name == 'user-keys':
            frames, expected = [dict(hex='1b5b3930317e', after_ms=150)], 'USER' if value == first else 'bank'
        elif name == 'prefix-timeout':
            frames = [dict(hex='02', after_ms=200), dict(hex='78', after_ms=150)]
            expected = 'PREFIX' if value == first else 'ROOT'
        else:
            frames = [dict(hex='0278', after_ms=200), dict(hex='78', after_ms=150)]
            expected = 'ROOT' if value == first else 'PREFIX'
        r.obligations['effect'].append(r.add(role, ['display-message', '-p', '-t', 'bank:0', '#{window_name}'],
                                            expected + '\n', client_input=frames))
    r.add('restore-name', ['rename-window', '-t', 'bank:0', 'bank'])
    return r.finish()


def input_plan(commands, entry):
    from options_bank_linux_recipe import Recipe
    name = entry['name']
    first, second = ('0', '1000') if name in ('escape-time', 'assume-paste-time') else ('off', 'on')
    r = Recipe(commands, entry, 'server' if name == 'escape-time' else 'session', first, second)
    r.mutations()
    if name == 'escape-time':
        for key, text in [('Escape', 'ESCAPE'), ('Up', 'ARROW')]:
            r.add('bind-' + key, ['bind-key', '-T', 'root', key, 'rename-window', '-t', 'bank:0', text])
    elif name == 'assume-paste-time':
        # tmux uses its timing heuristic only without bracketed-paste capability.
        r.add('plain-paste-terminal', ['set-option', '-s', 'terminal-overrides', 'xterm*:Enbp@'])
        r.add('detach-capabilities', ['detach-client', '-s', 'bank'])
        r.add('reload-capabilities', ['display-message', '-p', ''], reattach='plain-paste')
        r.add('prefix', ['set-option', '-g', 'prefix', 'C-b'])
        r.add('bind', ['bind-key', '-T', 'prefix', 'x', 'rename-window', '-t', 'bank:0', 'PREFIX'])
    elif name == 'mouse':
        r.add('bind-mouse', ['bind-key', '-T', 'root', 'WheelUpPane', 'rename-window', '-t', 'bank:0', 'MOUSE'])
    else:
        r.add('mouse-on', ['set-option', '-g', 'mouse', 'on'])
        r.add('split-consumer', ['split-window', '-h', '-d', '-t', 'bank:0', '/bin/sleep', '239'],
              consumer_argv=['/bin/sleep', '239'], consumer_label=name + '-split')
    r.obligations['effect'] = []
    for role, value in [('effect', first), ('second-effect', second), ('restored-effect', first)]:
        r.obligations['effect'].append(r.set(role + '-value', value))
        r.add(role + '-reset-name', ['rename-window', '-t', 'bank:0', 'bank'])
        if name == 'escape-time':
            # Outstanding terminal capability queries impose a 500 ms floor.
            frames = [dict(hex='1b', after_ms=650), dict(hex='5b41', after_ms=500)]
            expected, field = ('ESCAPE' if value == first else 'ARROW'), '#{window_name}'
        elif name == 'assume-paste-time':
            frames = [dict(hex='61', after_ms=5), dict(hex='62', after_ms=5), dict(hex='0278', after_ms=150)]
            expected, field = ('PREFIX' if value == first else 'bank'), '#{window_name}'
        elif name == 'mouse':
            frames = [dict(hex='1b5b3c36343b31303b354d', after_ms=150)]
            expected, field = ('bank' if value == first else 'MOUSE'), '#{window_name}'
        else:
            r.add(role + '-active', ['select-pane', '-t', 'bank:0.0'])
            frames = [dict(hex='1b5b3c33353b353b354d', after_ms=50), dict(hex='1b5b3c33353b36303b354d', after_ms=150)]
            expected, field = ('0' if value == first else '1'), '#{pane_index}'
        r.obligations['effect'].append(r.add(role, ['display-message', '-p', '-t', 'bank:0', field],
                                            expected + '\n', client_input=frames))
    if name == 'focus-follows-mouse':
        r.add('close-consumer', ['kill-pane', '-t', 'bank:0.1'])
    r.add('restore-name', ['rename-window', '-t', 'bank:0', 'bank'])
    return r.finish()
