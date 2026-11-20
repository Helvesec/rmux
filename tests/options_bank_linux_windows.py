"""Window and session options consumed by creation and destruction commands."""

OPTIONS = {'renumber-windows', 'default-size', 'remain-on-exit'}


def plan(commands, entry):
    from options_bank_linux_recipe import Recipe
    name = entry['name']
    first, second = {'renumber-windows': ('off', 'on'),
                     'default-size': ('91x27', '89x26'), 'remain-on-exit': ('on', 'failed')}[name]
    r = Recipe(commands, entry, 'window' if name == 'remain-on-exit' else 'session', first, second)
    r.mutations()
    if name == 'renumber-windows':
        r.add('create-consumer', ['new-window', '-d', '-t', 'bank:2', '-n', 'first', '/bin/sleep', '240'])
        r.add('second-create', ['new-window', '-d', '-t', 'bank:4', '-n', 'second', '/bin/sleep', '235'],
              consumer_argv=['/bin/sleep', '235'], consumer_label=name + '-second')
        r.add('remove-first', ['kill-window', '-t', 'bank:2'])
        r.obligations['effect'] = [r.add('effect', ['list-windows', '-t', 'bank', '-F', '#{window_name}|#{window_index}'],
                                       'bank|0\nsecond|4\n')]
        r.set('enable-renumber', second)
        r.add('trigger-create', ['new-window', '-d', '-t', 'bank:6', '-n', 'trigger', '/bin/sleep', '234'],
              consumer_argv=['/bin/sleep', '234'], consumer_label=name + '-trigger')
        r.add('trigger-close', ['kill-window', '-t', 'bank:6'])
        r.obligations['effect'] += [r.add('renumber-effect', ['list-windows', '-t', 'bank', '-F', '#{window_name}|#{window_index}'],
                                         'bank|0\nsecond|1\n')]
        r.add('close-second', ['kill-window', '-t', 'bank:1'])
    elif name == 'default-size':
        r.obligations['effect'] = []
        for role, value, expected in [('effect', first, '91|27\n'), ('second-effect', second, '89|26\n')]:
            r.set(role + '-size', value)
            r.obligations['effect'] += [r.add(role + '-create', ['new-session', '-d', '-s', 'sized', '-n', 'sized', '/bin/sleep', '235'],
                                            consumer_argv=['/bin/sleep', '235'], consumer_label=name + '-' + role),
                                       r.add(role, ['display-message', '-p', '-t', 'sized', '#{window_width}|#{window_height}'], expected),
                                       r.add(role + '-close', ['kill-session', '-t', 'sized'])]
    else:
        r.obligations['effect'] = []
        for role, value, expected in [('effect', first, 'bank|0\neffect|1\n'), ('second-effect', second, 'bank|0\n')]:
            r.set(role + '-remain', value)
            r.obligations['effect'] += [r.add(role + '-create', ['new-window', '-d', '-t', 'bank:', '-n', 'effect', '/bin/sleep', '235'],
                                            consumer_argv=['/bin/sleep', '235'], consumer_label=name + '-' + role),
                                       r.add(role + '-exit', ['respawn-pane', '-k', '-t', 'bank:1', '/bin/sh', '-c', 'exit 0']),
                                       r.add(role, ['list-windows', '-t', 'bank', '-F', '#{window_name}|#{window_index}'], expected,
                                             wait_pane_exit="bank:1", wait_pane_kept=value == 'on')]
            if value == 'on':
                r.obligations['effect'] += [r.add('dead-pane', ['display-message', '-p', '-t', 'bank:1', '#{pane_dead}|#{pane_dead_status}'], '1|0\n')]
                r.add(role + '-close', ['kill-window', '-t', 'bank:1'])
    return r.finish()


def await_pane_exit(run, row):
    """Wait for the terminal exit notification before the final observation."""
    target = row['wait_pane_exit']
    def ended():
        value, index = run.command(row['role'] + '-exit-probe',
            ['list-panes', '-t', target, '-F', '#{pane_dead}'], check=False)
        probe = run.record['commands'][index]
        return pane_exit_observed(row, probe)
    run.wait(ended)


def pane_exit_observed(row, probe):
    from options_bank_linux import require
    require(type(row['wait_pane_kept']) is bool, 'pane exit retention binding')
    return (probe['exit_code'] == 0 and probe['stdout'] == '1\n'
            if row['wait_pane_kept'] else probe['exit_code'] != 0)


def planned_commands(commands, plan, prefix):
    """Replay only the declared waits; keep every probe in the native journal."""
    import hashlib
    from options_bank_linux import require
    selected, indices, cursor = [], [], 2
    for row in plan:
        if 'wait_pane_exit' in row:
            probes = []
            while cursor < len(commands) - 1 and commands[cursor]['role'] == row['role'] + '-exit-probe':
                probe = commands[cursor]
                require(probe['argv'] == prefix + ['list-panes', '-t', row['wait_pane_exit'], '-F', '#{pane_dead}']
                        and type(probe['exit_code']) is int, 'pane exit probe binding')
                for channel in ('stdout', 'stderr'):
                    require(probe[channel + '_sha256'] == hashlib.sha256(probe[channel].encode()).hexdigest(),
                            'pane exit probe output hash')
                require(not probes or not pane_exit_observed(row, probes[-1]),
                        'pane exit probe after readiness')
                probes.append(probe)
                cursor += 1
            require(probes and pane_exit_observed(row, probes[-1]),
                    'pane exit wait not observed')
        require(cursor < len(commands) - 1 and commands[cursor]['role'] == row['role'],
                'planned command population/order')
        selected.append(commands[cursor])
        indices.append(cursor)
        cursor += 1
    require(cursor == len(commands) - 1, 'unplanned fixture command')
    return selected, indices
