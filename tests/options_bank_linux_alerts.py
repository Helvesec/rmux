"""Alert mutations consumed by window flags and the attached terminal bell."""

OPTIONS = {'monitor-bell', 'monitor-activity', 'monitor-silence', 'bell-action',
           'activity-action', 'silence-action', 'visual-bell', 'visual-activity', 'visual-silence'}


def plan(commands, entry):
    from options_bank_linux_recipe import Recipe
    name = entry['name']
    kind = 'bell' if 'bell' in name else 'activity' if 'activity' in name else 'silence'
    monitor, visual = name.startswith('monitor-'), name.startswith('visual-')
    first, second = ('0', '1') if name == 'monitor-silence' else ('off', 'on') if monitor or visual else ('none', 'any')
    r = Recipe(commands, entry, 'window' if monitor else 'session', first, second)
    r.mutations()
    for option, value in [('monitor-' + kind, '1' if kind == 'silence' else 'on'),
                          (kind + '-action', 'any'), ('visual-' + kind, 'off')]:
        if option != name:
            r.add('setup-' + option, ['set-option', '-gw' if option.startswith('monitor-') else '-g', option, value])
    r.add('create-consumer', ['new-window', '-d', '-t', 'bank:1', '-n', 'effect', '/bin/sleep', '240'])
    r.obligations['effect'] = []
    for role, value in [('effect', first), ('second-effect', second), ('restored-effect', first)]:
        r.obligations['effect'].append(r.set(role + '-value', value))
        r.add(role + '-clear-alert', ['select-window', '-t', 'bank:1'])
        r.add(role + '-select-bank', ['select-window', '-t', 'bank:0'], client_mark=not monitor)
        output = '\\007' if kind == 'bell' else 'ACTIVITY' if kind == 'activity' else 'SILENCESTART'
        r.obligations['effect'].append(r.add(role + '-stimulus', ['respawn-pane', '-k', '-t', 'bank:1',
                "printf '" + output + "'; exec /bin/sleep 238"],
                consumer_argv=['/bin/sleep', '238'], consumer_label=name + '-' + role))
        if kind == 'silence':
            r.add(role + '-wait-silence', ['display-message', '-p', ''], client_input=[dict(hex='', after_ms=1300)])
        extra = {} if monitor else {'client_event': dict(hex='07', present=value == first if visual else value == second)}
        expected = ('0' if value == first else '1') + '\n' if monitor else None
        r.obligations['effect'].append(r.add(role, ['display-message', '-p', '-t', 'bank:1', '#{window_' + kind + '_flag}'],
                                            expected, settle_terminal=True, **extra))
    r.add('close-consumer', ['kill-window', '-t', 'bank:1'])
    return r.finish()
