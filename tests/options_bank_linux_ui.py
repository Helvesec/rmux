"""Interactive consumer recipes for message, copy, border and pane styles."""

STYLES = {'message-style', 'message-command-style', 'mode-style',
          'menu-style', 'menu-selected-style', 'menu-border-style',
          'popup-style', 'popup-border-style', 'pane-border-style',
          'pane-active-border-style', 'window-style', 'window-active-style',
          'copy-mode-match-style', 'copy-mode-current-match-style',
          'copy-mode-mark-style', 'copy-mode-line-number-style',
          'copy-mode-current-line-number-style', 'copy-mode-position-style',
          'copy-mode-selection-style', 'tree-mode-preview-style'}
TEXT = {'message-format', 'pane-border-format', 'remain-on-exit-format',
        'copy-mode-position-format', 'tree-mode-preview-format', 'fill-character'}
OPTIONS = {*STYLES, *TEXT, 'message-line', 'copy-mode-line-numbers',
           'pane-border-lines', 'pane-border-indicators', 'menu-border-lines',
           'popup-border-lines'}


def plan(commands, entry):
    from options_bank_linux_recipe import Recipe
    name = entry['name']
    scope = 'session' if name.startswith('message-') else 'window'
    first, second = ('fg=red', 'fg=blue') if name in STYLES else ('FIRST', 'SECOND')
    if name == 'fill-character':
        first, second = 'X', 'Y'
    elif name == 'message-line':
        first, second = '0', '1'
    elif name == 'copy-mode-line-numbers':
        first, second = 'off', 'absolute'
    elif name.endswith('-lines'):
        first, second = 'single', 'double'
    elif name == 'pane-border-indicators':
        first, second = 'off', 'arrows'
    r = Recipe(commands, entry, scope, first, second)
    r.mutations()
    r.add('setup-status', ['set-option', '-g', 'status', '2' if name == 'message-line' else 'on'])
    r.add('stable-status', ['set-option', '-g', 'status-right', ''])
    script = "printf 'AAAA\\r\\nBBBB\\r\\nAAAA\\r\\n'; exec /bin/sleep 236"
    r.add('create-consumer', ['new-window', '-t', 'bank:1', '-n', 'effect', script],
          consumer_argv=['/bin/sleep', '236'])
    r.obligations['effect'] = []
    for role, value in [('effect', first), ('second-effect', second), ('restore-effect', first)]:
        r.obligations['effect'].append(r.set(role + '-value', value))
        marker = value if name in TEXT else 'AAAA'
        expectation = dict(text=marker)
        if name in STYLES:
            expectation['fg'] = 'red' if value == first else 'blue'
        argv = ['refresh-client', '-S']
        setup = []
        close = None
        if name.startswith('message-'):
            marker = value if name == 'message-format' else 'NOTICE'
            expectation['text'] = marker
            if name == 'message-command-style':
                setup += [['set-option', '-g', 'status-keys', 'vi']]
                argv = ['command-prompt', '-p', 'NOTICE', 'display-message "%%"']
                close = ['send-keys', '-K', 'Escape']
            else:
                argv = ['display-message', '-d', '5000', 'NOTICE']
                close = ['send-keys', '-K', 'Escape']
            if name == 'message-line':
                expectation['row'] = 22 if value == '0' else 23
        elif name.startswith('copy-mode-') or name == 'mode-style':
            if name == 'copy-mode-position-style':
                setup += [['set-option', '-gw', 'copy-mode-position-format', 'POSITION']]
                expectation['text'] = 'POSITION'
            if 'line-number' in name:
                setup += [['set-option', '-gw', 'copy-mode-line-numbers', value if name == 'copy-mode-line-numbers' else 'absolute']]
            setup += [['copy-mode', '-t', 'bank:1'],
                      ['send-keys', '-X', '-t', 'bank:1', 'history-top']]
            if name in ('copy-mode-match-style', 'copy-mode-current-match-style'):
                setup += [['send-keys', '-X', '-t', 'bank:1', 'search-forward', 'AAAA']]
            elif name == 'copy-mode-mark-style':
                # The mark itself has a cursor background; inspect its styled line.
                expectation.update(text='AAA', column=1)
                setup += [['send-keys', '-X', '-t', 'bank:1', 'set-mark'],
                          ['send-keys', '-X', '-t', 'bank:1', 'cursor-down']]
            elif name in ('copy-mode-selection-style', 'mode-style'):
                setup += [['send-keys', '-X', '-t', 'bank:1', 'select-line']]
            elif 'line-number' in name:
                expectation['text'] = '2' if name == 'copy-mode-line-number-style' else '1' if value == 'absolute' or name != 'copy-mode-line-numbers' else 'AAAA'
            elif name == 'copy-mode-position-style':
                pass
            close = ['send-keys', '-X', '-t', 'bank:1', 'cancel']
        elif name.startswith('menu-'):
            expectation['text'] = '│' if name == 'menu-border-style' else 'OTHER' if name == 'menu-style' else 'ITEM'
            if name == 'menu-border-lines':
                expectation['text'] = '┌' if value == 'single' else '╔'
            argv = ['display-menu', '-x', '10', '-y', '5', '-T', 'NOTICE', 'ITEM', 'a', 'display-message done',
                    'OTHER', 'b', 'display-message done']
            close = ['send-keys', '-K', 'Escape']
        elif name.startswith('popup-'):
            expectation['text'] = '│' if name == 'popup-border-style' else 'ITEM'
            if name == 'popup-border-lines':
                expectation['text'] = '┌' if value == 'single' else '╔'
            argv = ['display-popup', '-E', '-w', '30', '-h', '10', '-T', 'NOTICE', "printf ITEM; exec /bin/cat"]
            close = ['send-keys', '-K', 'Escape']
        elif name.startswith('pane-border-') or name == 'pane-active-border-style':
            setup += [['split-window', '-h', '-d', '-t', 'bank:1', '/bin/sleep', '239'],
                      ['set-option', '-gw', 'pane-border-status', 'top']]
            expectation['text'] = value if name == 'pane-border-format' else 'BORDER'
            if name != 'pane-border-format':
                setup += [['set-option', '-gw', 'pane-border-format', 'BORDER']]
            if name in ('pane-border-lines', 'pane-border-indicators'):
                expectation['text'] = '─' if name == 'pane-border-lines' and value == 'single' else '═' if name == 'pane-border-lines' else 'BORDER'
            close = ['kill-pane', '-t', 'bank:1.1']
        elif name in ('window-style', 'window-active-style'):
            if name == 'window-style':
                setup += [['split-window', '-h', '-t', 'bank:1', '/bin/sleep', '239']]
                close = ['kill-pane', '-t', 'bank:1.1']
        elif name == 'remain-on-exit-format':
            setup += [['set-option', '-w', '-t', 'bank:1', 'remain-on-exit', 'on'],
                      ['respawn-pane', '-k', '-t', 'bank:1', '/bin/sh', '-c', 'exit 0']]
        elif name.startswith('tree-mode-'):
            argv = ['choose-tree', '-t', 'bank:1']
            expectation['text'] = value if name == 'tree-mode-preview-format' else '0:'
            close = ['send-keys', '-t', 'bank:1', 'q']
        elif name == 'fill-character':
            setup += [['resize-window', '-t', 'bank:1', '-x', '50', '-y', '15']]
        for index, step in enumerate(setup):
            extra = dict(consumer_argv=['/bin/sleep', '239'], consumer_label=name + '-' + role + '-split') if step[0] == 'split-window' else {}
            r.obligations['effect'].append(r.add(role + '-setup-' + str(index), step, settle_terminal=True, **extra))
        blocking = name.startswith(('menu-', 'popup-')) or name == 'message-command-style'
        extra = {'dismiss_input': '1b'} if blocking else {}
        if name.startswith('popup-'):
            extra.update(dismiss_input='04', consumer_argv=['/bin/cat'], consumer_exe='/bin/cat',
                         consumer_label=name + '-' + role + '-popup')
        if name == 'message-command-style':
            extra.update(before_input=[dict(hex='1b', after_ms=650)], dismiss_input='690d')
        r.obligations['effect'].append(r.add(role, argv, client_screen=[expectation], settle_terminal=True, **extra))
        if blocking:
            close = None
        if close:
            r.add(role + '-close', close)
    r.add('select-bank', ['select-window', '-t', 'bank:0'])
    r.add('close-consumer', ['kill-window', '-t', 'bank:1'])
    return r.finish()
