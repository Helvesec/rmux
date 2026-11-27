"""OSC title policy consumed by the native terminal parser."""

OPTIONS = {'allow-set-title'}


def plan(commands, entry):
    from options_bank_linux_recipe import Recipe
    r = Recipe(commands, entry, 'window', 'on', 'off')
    r.mutations()
    target = 'bank:1'
    r.obligations['effect'] = []
    for role, value, title, expected in [('effect', 'on', 'FIRST', 'FIRST'),
                                        ('disabled-effect', 'off', 'SECOND', 'FIRST'),
                                        ('enabled-effect', 'on', 'THIRD', 'THIRD')]:
        r.obligations['effect'].append(r.set(role + '-value', value))
        command = ['new-window', '-d', '-t', target, '-n', 'effect'] if role == 'effect' else ['respawn-pane', '-k', '-t', target]
        script = "printf '\\033]2;" + title + "\\007'; exec /bin/sleep 236"
        r.obligations['effect'].append(r.add(role + '-create', [*command, script],
                                            consumer_argv=['/bin/sleep', '236'],
                                            consumer_label=r.name + '-' + role))
        r.obligations['effect'].append(r.add(role, ['display-message', '-p', '-t', target, '#{pane_title}'],
                                            expected + '\n', settle_terminal=True))
    r.add('close-consumer', ['kill-window', '-t', target])
    return r.finish()
