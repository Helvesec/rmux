"""Downstream command and environment consumers."""

OPTIONS = {'default-client-command', 'update-environment', 'synchronize-panes'}


def plan(commands, entry):
    from options_bank_linux_recipe import Recipe
    name = entry['name']
    scope, first, second = {'default-client-command': ('server', 'display-message -p FIRST', 'display-message -p SECOND'),
                            'update-environment': ('session', 'BANK_ALPHA', 'BANK_BETA'),
                            'synchronize-panes': ('window', 'off', 'on')}[name]
    r = Recipe(commands, entry, scope, first, second)
    r.mutations()
    r.obligations['effect'] = []
    if name == 'default-client-command':
        for role, value, expected in [('effect', first, 'FIRST'), ('second-effect', second, 'SECOND'), ('restored-effect', first, 'FIRST')]:
            r.obligations['effect'] += [r.set(role + '-value', value), r.add(role, ['--'], expected + '\n')]
    elif name == 'update-environment':
        r.add('global-environment', ['set-environment', '-g', 'BANK_ALPHA', 'present'])
        for role, value, expected in [('effect', first, 'unset'), ('second-effect', second, 'present'), ('restored-effect', first, 'unset')]:
            r.obligations['effect'] += [r.set(role + '-value', value),
                r.add(role + '-create', ['new-session', '-d', '-s', 'environment',
                    'printf "%s" "${BANK_ALPHA-unset}"; exec /bin/sleep 237'],
                      consumer_argv=['/bin/sleep', '237'], consumer_label=name + '-' + role),
                r.add(role, ['capture-pane', '-p', '-t', 'environment:0', '-S', '0', '-E', '0'], expected + '\n', settle_terminal=True)]
            r.add(role + '-close', ['kill-session', '-t', 'environment'])
    else:
        r.add('create-consumer', ['new-window', '-d', '-t', 'bank:1', '-n', 'effect', 'stty -echo; exec /bin/cat'],
              consumer_argv=['/bin/cat'], consumer_exe='/bin/cat')
        r.add('split-consumer', ['split-window', '-d', '-t', 'bank:1', 'stty -echo; exec /bin/cat'],
              consumer_argv=['/bin/cat'], consumer_exe='/bin/cat', consumer_label=name + '-split')
        for role, value, text, expected in [('effect', first, 'FIRST', '\n\n\n'),
                                          ('second-effect', second, 'SECOND', 'SECOND\n\n\n'),
                                          ('restored-effect', first, 'THIRD', 'SECOND\n\n\n')]:
            r.obligations['effect'] += [r.set(role + '-value', value),
                r.add(role + '-input', ['send-keys', '-t', 'bank:1.0', text, 'Enter'], settle_terminal=True),
                r.add(role, ['capture-pane', '-p', '-t', 'bank:1.1', '-S', '0', '-E', '2'], expected, settle_terminal=True)]
        r.add('close-consumer', ['kill-window', '-t', 'bank:1'])
    return r.finish()
