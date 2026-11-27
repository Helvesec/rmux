"""Copy mode recipes observe navigation and the actual copy-pipe child."""

OPTIONS = {'mode-keys', 'word-separators', 'wrap-search', 'copy-command'}


def plan(commands, entry):
    from options_bank_linux_recipe import Recipe
    name = entry['name']
    first, second = {'mode-keys': ('emacs', 'vi'), 'word-separators': (':', '_'),
                     'wrap-search': ('off', 'on'),
                     'copy-command': ('cat > copy-first', 'cat > copy-second')}[name]
    scope = 'server' if name == 'copy-command' else 'session' if name == 'word-separators' else 'window'
    r = Recipe(commands, entry, scope, first, second)
    r.mutations()
    for role, value in [('effect', first), ('second-effect', second)]:
        r.set(role + '-value', value)
        setup = [['copy-mode', '-t', 'bank:1'],
                 ['send-keys', '-X', '-t', 'bank:1', 'history-top'],
                 ['send-keys', '-X', '-t', 'bank:1', 'start-of-line']]
        if name == 'mode-keys':
            setup += [['send-keys', '-X', '-t', 'bank:1', 'cursor-right'],
                      ['send-keys', '-X', '-t', 'bank:1', 'cursor-right'],
                      ['send-keys', '-t', 'bank:1', 'h']]
            r.terminal(role, 'abcde', ['display-message', '-p'],
                       (['#{copy_cursor_x}'], '2\n' if value == 'emacs' else '1\n'), setup)
        elif name == 'word-separators':
            setup += [['send-keys', '-X', '-t', 'bank:1', 'next-word']]
            r.terminal(role, 'aa:bb_cc', ['display-message', '-p'],
                       (['#{copy_cursor_x}'], '2\n' if value == ':' else '5\n'), setup)
        elif name == 'wrap-search':
            setup += [['send-keys', '-X', '-t', 'bank:1', 'search-backward', 'needle']]
            r.terminal(role, 'needle\\r\\nxxxxx\\r\\nneedle', ['display-message', '-p'],
                       (['#{copy_cursor_y}'], '0\n' if value == 'off' else '2\n'), setup)
        else:
            setup += [['send-keys', '-X', '-t', 'bank:1', 'select-line'],
                      ['send-keys', '-X', '-t', 'bank:1', 'copy-pipe-and-cancel']]
            file = 'copy-first' if value == first else 'copy-second'
            setup += [['load-buffer', '-b', 'piped', file]]
            r.terminal(role, 'COPY\\r\\n', ['show-buffer', '-b', 'piped'], ([], 'COPY'), setup, targeted=False)
            load = next(row for row in commands if row['role'] == name + ':' + role + '-setup-5')
            load.update(wait_file=file, expected_file_bytes='COPY')
    return r.finish()
