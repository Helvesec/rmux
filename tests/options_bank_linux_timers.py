"""Timer and clock options consumed by independently observed terminal frames."""

OPTIONS = {'display-time', 'display-panes-time', 'display-panes-colour',
           'display-panes-active-colour', 'status-interval', 'status-keys',
           'clock-mode-colour', 'clock-mode-style', 'pane-colours'}


def plan(commands, entry):
    from options_bank_linux_recipe import Recipe
    name = entry['name']
    scope = 'window' if name.startswith('clock-') or name == 'pane-colours' else 'session'
    first, second = ('red', 'blue') if name.endswith('-colour') or name == 'pane-colours' else ('emacs', 'vi') if name == 'status-keys' else ('12', '24-with-seconds') if name == 'clock-mode-style' else ('0', '1') if name == 'status-interval' else ('100', '1000')
    r = Recipe(commands, entry, scope, first, second)
    r.mutations()
    if name == 'status-interval':
        # Answer the attached terminal's DA query before measuring the timer.
        # Otherwise tmux's five-second startup fallback redraws status and
        # reruns its format job during restoration, independently of interval.
        r.add('terminal-ready', ['display-message', '-p', ''],
              client_input=[dict(hex='1b5b3f313b3263', after_ms=0)])
    r.add('status-on', ['set-option', '-g', 'status', 'on'])
    r.add('stable-right', ['set-option', '-g', 'status-right', ''])
    if name.startswith('clock-') or name.startswith('display-panes'):
        r.add('manual-overlay-window', ['set-option', '-w', '-t', 'bank:0', 'window-size', 'manual'])
        r.add('plain-overlay-size', ['resize-window', '-t', 'bank:0', '-x', '20', '-y', '4'])
    if name == 'display-panes-colour':
        r.add('split-consumer', ['split-window', '-h', '-d', '-t', 'bank:0', '/bin/sleep', '239'],
              consumer_argv=['/bin/sleep', '239'], consumer_label=name + '-split')
    if name == 'display-panes-time':
        r.add('overlay-colour', ['set-option', '-g', 'display-panes-active-colour', 'red'])
    if name == 'pane-colours':
        r.add('create-consumer', ['new-window', '-t', 'bank:1', '-n', 'effect',
              "printf '\\033[30mCOLOUR'; exec /bin/sleep 236"], consumer_argv=['/bin/sleep', '236'])
    r.obligations['effect'] = []
    for role, value in [('effect', first), ('second-effect', second), ('restored-effect', first)]:
        r.obligations['effect'].append(r.set(role + '-value', value))
        extra, close = {}, None
        if name == 'display-time':
            r.add(role + '-message', ['display-message', 'NOTICE'])
            r.add(role + '-wait', ['display-message', '-p', ''], client_input=[dict(hex='', after_ms=250)])
            expectation = dict(text='NOTICE', row=23, absent=value == first)
        elif name.startswith('display-panes'):
            r.add(role + '-overlay', ['display-panes', '-b', '-t', '$attached'])
            r.add(role + '-wait', ['display-message', '-p', ''], client_input=[dict(hex='', after_ms=250)])
            expectation = dict(text='1' if name == 'display-panes-colour' else '0', fg=value if name.endswith('-colour') else 'red')
            if name == 'display-panes-time':
                expectation['absent'] = value == first
        elif name.startswith('clock-'):
            r.add(role + '-clock', ['clock-mode', '-t', 'bank:0'])
            expectation = dict(text_pattern=r'\d{1,2}:\d{2}(?::\d{2})?(?: [AP]M)?')
            if name == 'clock-mode-colour':
                expectation['fg'] = value
            else:
                expectation['colon_count'] = 1 if value == first else 2
            close = ['send-keys', '-t', 'bank:0', 'q']
        elif name == 'status-keys':
            expectation = dict(text='PROMPT', row=23, absent=value == 'emacs')
            extra.update(dismiss_input='690d', before_input=[dict(hex='1b', after_ms=650)])
        elif name == 'status-interval':
            ticker = 'ticker-' + role
            r.add(role + '-seed', ['run-shell', 'printf FIRST > ' + ticker])
            r.add(role + '-clear', ['set-option', '-g', 'status-left', ''])
            r.add(role + '-template', ['set-option', '-g', 'status-left', '#(cat ' + ticker + ')'])
            r.add(role + '-prime', ['refresh-client', '-S'], client_screen=[dict(text='FIRST', row=23)])
            r.add(role + '-change', ['run-shell', 'printf SECOND > ' + ticker])
            r.add(role + '-wait', ['display-message', '-p', ''], client_input=[dict(hex='', after_ms=1500)])
            expectation = dict(text='FIRST' if value == first else 'SECOND', row=23)
        else:
            expectation = dict(text='COLOUR', fg=value)
        argv = ['display-message', '-p', ''] if name == 'status-interval' else ['command-prompt', '-p', 'PROMPT', 'display-message "%%"'] if name == 'status-keys' else ['refresh-client', '-S']
        r.obligations['effect'].append(r.add(role, argv, client_screen=[expectation], **extra))
        if close:
            r.add(role + '-close', close)
    if name == 'status-interval':
        r.add('stop-status-jobs', ['set-option', '-g', 'status-left', ''], client_input=[dict(hex='', after_ms=250)])
    if name == 'display-panes-colour':
        r.add('close-split', ['kill-pane', '-t', 'bank:0.1'])
    if name == 'pane-colours':
        r.add('close-consumer', ['kill-window', '-t', 'bank:1'])
    return r.finish()
