"""Terminal output recipes measured against the native tmux oracle."""

TERMINAL_OPTIONS = {'variation-selector-always-wide', 'alternate-screen',
                    'allow-rename', 'scroll-on-clear', 'default-terminal',
                    'automatic-rename', 'automatic-rename-format'}


def terminal_plan(plan, entry):
    from options_bank_linux_recipe import Recipe
    name = entry['name']
    scope = 'server' if name in ('variation-selector-always-wide', 'default-terminal') else 'window'
    first, second = ('vt100', 'xterm') if name == 'default-terminal' else ('RENAMED', 'SECOND') if name == 'automatic-rename-format' else ('off', 'on')
    if name == 'automatic-rename':
        # The frozen fixture disables automatic naming globally. Read the
        # independent registry default only after explicitly resetting it.
        plan.append(dict(role=name + ':reset-startup-override',
                         argv=['set-option', '-gwu', name], error=False))
    r = Recipe(plan, entry, scope, first, second)
    r.mutations()
    if name == 'automatic-rename':
        r.add('stable-fixture-policy', ['set-option', '-w', '-t', 'bank:0', 'automatic-rename', 'off'])
        r.add('stable-fixture-name', ['rename-window', '-t', 'bank:0', 'bank'])
    if name == 'variation-selector-always-wide':
        for role, value, width in [('effect', 'off', 1), ('wide-effect', 'on', 2)]:
            r.set(role + '-value', value)
            r.terminal(role, '\\342\\235\\244\\357\\270\\217', ['display-message', '-p'],
                       (['#{cursor_x}'], str(width) + '\n'))
    elif name == 'alternate-screen':
        output = '\\033[?1049hALT\\033[?1049lNORMAL'
        for role, value, text in [('effect', 'off', 'ALTNORMAL'), ('alternate-effect', 'on', 'NORMAL')]:
            r.set(role + '-value', value)
            r.terminal(role, output, ['capture-pane', '-p'], ([], text + '\n' * 24))
    elif name == 'scroll-on-clear':
        output = 'one\\r\\ntwo\\r\\nthree\\033[H\\033[2J'
        for role, value, size in [('effect', 'off', 0), ('history-effect', 'on', 3)]:
            r.set(role + '-value', value)
            r.terminal(role, output, ['display-message', '-p'], (['#{history_size}'], str(size) + '\n'))
    elif name == 'allow-rename':
        for role, value, text in [('effect', 'off', 'effect'), ('rename-effect', 'on', 'RENAMED')]:
            r.set(role + '-value', value)
            r.terminal(role, '\\033kRENAMED\\033\\\\', ['display-message', '-p'],
                       (['#{window_name}'], text + '\n'))
    elif name == 'default-terminal':
        for role, value in [('effect', first), ('term-effect', second)]:
            r.set(role + '-value', value)
            r.terminal(role, '', ['capture-pane', '-p'], ([], value + '\n' * 24),
                       script='printf "%s" "$TERM"')
    else:
        for role, value in [('effect', first), ('rename-effect', second)]:
            r.set(role + '-value', value)
            if name == 'automatic-rename':
                r.add(role + '-format', ['set-option', '-gw', 'automatic-rename-format', 'RENAMED'])
            expected = value if name == 'automatic-rename-format' else 'effect' if value == 'off' else 'RENAMED'
            r.terminal(role, '', ['display-message', '-p'], (['#{window_name}'], expected + '\n'),
                       setup=[['set-option', '-w', '-t', 'bank:1', 'automatic-rename', value if name == 'automatic-rename' else 'on']])
        r.add('clear-rename-format', ['set-option', '-gwu', 'automatic-rename-format'])
    return r.finish()


def codepoint_widths_plan(plan, entry):
    name, begin, obligations = entry['name'], len(plan), {}

    def add(role, argv, expected=None, error=False, **extra):
        index = len(plan)
        row = dict(role=name + ':' + role, argv=argv, error=error, **extra)
        if expected is not None:
            row['expected_stdout'] = expected
        plan.append(row)
        return index

    def read(role, value):
        return add(role, ['show-options', '-sv', name], value)

    def mutate(role, value, flags='-s', command='set-option'):
        return add(role, [command, flags, name, value])

    def effect(role, width):
        label = name + '-' + role
        return [add(role + '-create', ['new-window', '-d', '-t', 'bank:', '-n', 'effect',
                    "printf '\\303\\251'; exec /bin/sleep 237"],
                    consumer_argv=['/bin/sleep', '237'], consumer_label=label),
                add(role, ['display-message', '-p', '-t', 'bank:effect', '#{cursor_x}'],
                    str(width) + '\n', settle_terminal=True),
                add(role + '-close', ['kill-window', '-t', 'bank:effect'])]

    obligations['default'] = [read('default', '')]
    obligations['effect'] = effect('default-effect', 1)
    obligations['replace'] = [mutate('replace', 'U+00E9=2'), read('value', 'U+00E9=2\n'),
                              add('named', ['show-options', '-s', name], 'codepoint-widths[0] U+00E9=2\n')]
    obligations['effect'] += effect('effect', 2)
    obligations['scope'] = []
    for flag in ('-g', '-w', '-p'):
        obligations['scope'] += [mutate('inferred-scope' + flag, 'U+00E9=2', flag),
                                 read('inferred-value' + flag, 'U+00E9=2\n')]
    obligations['only-if-unset'] = [add('only-existing', ['set-option', '-so', name, 'U+00E9=1'], error=True),
                                   read('only-value', 'U+00E9=2\n')]
    obligations['quiet'] = [mutate('only-quiet', 'U+00E9=1', '-soq'), read('quiet-value', 'U+00E9=2\n')]
    obligations['format'] = [mutate('format', 'U+00E9=#{e|+|:1,1}', '-sF'),
                             read('formatted-value', 'U+00E9=2\n')]
    obligations['sibling'] = [mutate('window-command', 'U+00E9=2', '-g', 'set-window-option'),
                              read('window-value', 'U+00E9=2\n'),
                              mutate('append', 'U+8868=1', '-sa'),
                              read('appended-value', 'U+00E9=2\nU+8868=1\n'),
                              add('unset-index', ['set-option', '-su', name + '[1]']),
                              read('index-value', 'U+00E9=2\n')]
    obligations['configuration'] = [add('source', ['source-file', '-'],
                                        source_text='set-option -s codepoint-widths U+00E9=2\n'),
                                     read('source-value', 'U+00E9=2\n')]
    obligations['unset'] = [add('unset', ['set-option', '-su', name]), read('reset-value', '')]
    obligations['effect'] += effect('reset-effect', 1)
    return dict(id=entry['id'], indices=list(range(begin, len(plan))),
                obligations=obligations, status='REQUIRED')
