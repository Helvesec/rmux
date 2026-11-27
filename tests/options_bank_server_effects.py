"""Measured server option consumers for the Linux per-option adapter."""

DEFAULT_ALIASES = ('split-pane=split-window\nsplitp=split-window\n'
                   'server-info=show-messages -JT\ninfo=show-messages -JT\n'
                   'choose-window=choose-tree -w\nchoose-session=choose-tree -s\n')
CONFIGURATIONS = {
    'buffer-limit': 'set-option -s buffer-limit 1\n',
    'command-alias': 'set-option -s command-alias "bank-alias=display-message -p alias-config"\n',
}


def server_option_plan(plan, entry):
    name, begin, obligations = entry['name'], len(plan), {}

    def add(role, argv, expected=None, error=False):
        index = len(plan)
        row = {'role': name + ':' + role, 'argv': argv, 'error': error}
        if expected is not None:
            row['expected_stdout'] = expected
        plan.append(row)
        return index

    def read(role, value):
        return add(role, ['show-options', '-sv', name], value)

    def set_value(role, value, flags='-s', command='set-option'):
        return add(role, [command, flags, name, value])

    if name == 'buffer-limit':
        obligations['default'] = [read('default', '50\n')]
        obligations['replace'] = [set_value('replace', '2'), read('value', '2\n'),
                                  add('named', ['show-options', '-s', name], 'buffer-limit 2\n')]
        add('named-buffer', ['set-buffer', '-b', 'named', 'named'])
        add('first-buffer', ['set-buffer', 'first'])
        add('second-buffer', ['set-buffer', 'second'])

        def effect(role, content, expected):
            return [add(role + '-store', ['set-buffer', content]),
                    add(role, ['list-buffers', '-F', '#{buffer_name}|#{buffer_sample}'], expected)]

        obligations['effect'] = effect('effect', 'third', 'buffer2|third\nbuffer1|second\nnamed|named\n')
        obligations['scope'] = []
        for flags, value in (('-g', '3'), ('-w', '2'), ('-p', '1')):
            obligations['scope'] += [set_value('scope' + flags, value, flags),
                                     read('scope-value' + flags, value + '\n')]
        obligations['effect'] += effect('scope-effect', 'fourth', 'buffer3|fourth\nnamed|named\n')
        obligations['only-if-unset'] = [add('only-existing', ['set-option', '-so', name, '9'], error=True),
                                       read('only-existing-value', '1\n')]
        obligations['quiet'] = [set_value('only-existing-quiet', '9', '-soq'), read('quiet-value', '1\n')]
        obligations['format'] = [set_value('format', '#{e|+|:1,1}', '-sF'), read('formatted-value', '2\n')]
        obligations['effect'] += effect('format-effect', 'fifth', 'buffer4|fifth\nbuffer3|fourth\nnamed|named\n')
        obligations['unset'] = [add('unset', ['set-option', '-su', name]), read('reset-default', '50\n')]
        obligations['effect'] += effect('reset-effect', 'sixth', 'buffer5|sixth\nbuffer4|fifth\nbuffer3|fourth\nnamed|named\n')
        obligations['sibling'] = [set_value('window-command', '1', '-g', 'set-window-option'),
                                  read('window-value', '1\n')]
        obligations['effect'] += effect('window-effect', 'seventh', 'buffer6|seventh\nnamed|named\n')
        obligations['sibling'] += [set_value('append-number', '2', '-sa'), read('append-value', '2\n'),
                                   add('invalid-number', ['set-option', '-s', name, '0'], error=True),
                                   read('invalid-value', '2\n')]
        obligations['effect'] += effect('sibling-effect', 'eighth', 'buffer7|eighth\nbuffer6|seventh\nnamed|named\n')
        obligations['configuration'] = [add('configuration', ['source-file', name + '.conf']),
                                        read('configuration-value', '1\n')]
        obligations['effect'] += effect('configuration-effect', 'ninth', 'buffer8|ninth\nnamed|named\n')
        for buffer in ('buffer8', 'named'):
            add('delete-' + buffer, ['delete-buffer', '-b', buffer])
        add('empty-store', ['list-buffers', '-F', '#{buffer_name}'], '')
    else:
        assert name == 'command-alias'

        def alias(value):
            return 'bank-alias=display-message -p ' + value

        def effect(role, value, command='bank-alias', error=False):
            return [add(role, [command], None if error else value + '\n', error)]

        obligations['default'] = [read('default', DEFAULT_ALIASES)]
        obligations['replace'] = [set_value('replace', alias('alias-effect')),
                                  read('value', alias('alias-effect') + '\n'),
                                  add('named', ['show-options', '-s', name],
                                      'command-alias[0] "' + alias('alias-effect') + '"\n')]
        obligations['effect'] = effect('effect', 'alias-effect')
        obligations['scope'] = []
        for flags, value in (('-g', 'alias-global'), ('-w', 'alias-window'), ('-p', 'alias-pane')):
            obligations['scope'] += [set_value('scope' + flags, alias(value), flags),
                                     read('scope-value' + flags, alias(value) + '\n')]
            obligations['effect'] += effect('scope-effect' + flags, value)
        obligations['only-if-unset'] = [add('only-existing', ['set-option', '-so', name, alias('blocked')], error=True),
                                       read('only-existing-value', alias('alias-pane') + '\n')]
        obligations['quiet'] = [set_value('only-existing-quiet', alias('blocked'), '-soq'),
                                read('quiet-value', alias('alias-pane') + '\n')]
        obligations['format'] = [set_value('format', 'bank-alias=display-message -p #{?session_name,alias-format,other}', '-sF'),
                                 read('formatted-value', alias('alias-format') + '\n')]
        obligations['effect'] += effect('format-effect', 'alias-format')
        obligations['unset'] = [add('unset', ['set-option', '-su', name]), read('reset-default', DEFAULT_ALIASES)]
        obligations['effect'] += effect('reset-effect', '', error=True)
        obligations['sibling'] = [set_value('window-command', alias('alias-sibling'), '-g', 'set-window-option'),
                                  read('window-value', alias('alias-sibling') + '\n'),
                                  set_value('append-alias', 'bank-two=display-message -p alias-second', '-sa'),
                                  read('append-value', alias('alias-sibling') + '\nbank-two=display-message -p alias-second\n')]
        obligations['effect'] += effect('window-effect', 'alias-sibling') + effect('append-effect', 'alias-second', 'bank-two')
        obligations['sibling'] += [add('unset-index', ['set-option', '-su', name + '[1]']),
                                   read('unset-index-value', alias('alias-sibling') + '\n')]
        obligations['effect'] += effect('unset-index-effect', '', 'bank-two', error=True)
        obligations['configuration'] = [add('configuration', ['source-file', name + '.conf']),
                                        read('configuration-value', alias('alias-config') + '\n')]
        obligations['effect'] += effect('configuration-effect', 'alias-config')
    add('restore-default', ['set-option', '-su', name])
    read('restored-default', '50\n' if name == 'buffer-limit' else DEFAULT_ALIASES)
    add('create-consumer', ['new-window', '-d', '-t', 'bank:', '-n', 'effect', '/bin/sleep', '240'])
    add('close-consumer', ['kill-window', '-t', 'bank:effect'])
    return {'id': entry['id'], 'indices': list(range(begin, len(plan))),
            'obligations': obligations, 'status': 'REQUIRED'}
