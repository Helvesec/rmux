"""Status line options consumed by rendering into an attached terminal."""

STYLES = {'status-style': ('LEFT', 'session'),
          'status-left-style': ('LEFT', 'session'),
          'status-right-style': ('RIGHT', 'session'),
          'window-status-style': ('OTHER', 'window'),
          'window-status-current-style': ('CURRENT', 'window'),
          'window-status-last-style': ('OTHER', 'window'),
          'pane-status-current-style': ('CURRENT', 'window'),
          'pane-status-style': ('OTHER', 'window'),
          'session-status-current-style': ('bank*', 'window'),
          'session-status-style': ('other', 'window'),
          'window-status-bell-style': ('OTHER', 'window'),
          'window-status-activity-style': ('OTHER', 'window')}
TEXT = {'status-left': ('FIRST', 'SECOND', 'session'),
        'status-right': ('FIRST', 'SECOND', 'session'),
        'status-format': ('FIRST', 'SECOND', 'session'),
        'window-status-format': ('FIRST', 'SECOND', 'window'),
        'window-status-current-format': ('FIRST', 'SECOND', 'window'),
        'window-pane-status-format': ('FIRST', 'SECOND', 'window'),
        'window-pane-current-status-format': ('FIRST', 'SECOND', 'window'),
        'window-status-separator': ('AAA', 'BBB', 'window')}
OPTIONS = {*STYLES, *TEXT, 'status-bg', 'status-fg', 'status-left-length',
           'status-right-length', 'status-position', 'status-justify', 'status'}


def plan(commands, entry):
    from options_bank_linux_recipe import Recipe
    name = entry['name']
    first, second, scope = TEXT.get(name, ('fg=red', 'fg=blue', 'session'))
    if name in STYLES:
        scope = STYLES[name][1]
    if name in ('status-bg', 'status-fg'):
        first, second = 'red', 'blue'
    elif name.endswith('-length'):
        first, second = '3', '6'
    elif name == 'status-position':
        first, second = 'top', 'bottom'
    elif name == 'status-justify':
        first, second = 'left', 'right'
    elif name == 'status':
        first, second = 'on', '2'
        commands.append(dict(role=name + ':reset-startup', argv=['set-option', '-gu', name], error=False))
    r = Recipe(commands, entry, scope, first, second)
    r.mutations()
    setup = [('status', 'on'), ('status-left', 'LEFT'), ('status-right', 'RIGHT'),
             ('status-style', 'default'), ('status-left-style', 'default'),
             ('status-right-style', 'default'), ('status-interval', '0')]
    for option, value in setup:
        if option != name:
            r.add('setup-' + option, ['set-option', '-g', option, value])
    for option, value in [('window-status-current-format', 'CURRENT'), ('window-status-format', 'OTHER')]:
        if option != name:
            r.add('setup-' + option, ['set-option', '-gw', option, value])
    r.add('create-consumer', ['new-window', '-d', '-t', 'bank:1', '-n', 'other', '/bin/sleep', '240'])
    r.add('remember-other', ['select-window', '-t', 'bank:1'])
    r.add('select-bank', ['select-window', '-t', 'bank:0'])
    if name.startswith('pane-status') or name.startswith('window-pane-'):
        r.add('pane-line', ['set-option', '-g', 'status', '2'])
        r.add('pane-current-text', ['set-option', '-gw', 'window-pane-current-status-format', 'CURRENT'] if name != 'window-pane-current-status-format'
              else ['display-message', '-p', ''])
        r.add('pane-other-text', ['set-option', '-gw', 'window-pane-status-format', 'OTHER'] if name != 'window-pane-status-format'
              else ['display-message', '-p', ''])
        r.add('split', ['split-window', '-d', '-t', 'bank:0', '/bin/sleep', '239'],
              consumer_argv=['/bin/sleep', '239'], consumer_label=name + '-split')
    if name.startswith('session-status'):
        r.add('session-line', ['set-option', '-g', 'status', '3'])
        r.add('other-session', ['new-session', '-d', '-s', 'other', '/bin/sleep', '239'],
              consumer_argv=['/bin/sleep', '239'], consumer_label=name + '-session')
    if name.endswith('-length'):
        r.add('length-text', ['set-option', '-g', name.removesuffix('-length'), 'ABCDEFGH'])
    r.obligations['effect'] = []
    for role, value in [('effect', first), ('second-effect', second), ('restore-effect', first)]:
        r.obligations['effect'].append(r.set(role + '-value', value))
        if name in ('window-status-bell-style', 'window-status-activity-style'):
            kind = 'bell' if name == 'window-status-bell-style' else 'activity'
            r.add(role + '-monitor', ['set-option', '-w', '-t', 'bank:1', 'monitor-' + kind, 'on'])
            r.add(role + '-clear', ['select-window', '-t', 'bank:1'])
            r.add(role + '-select', ['select-window', '-t', 'bank:0'])
            r.add(role + '-stimulus', ['respawn-pane', '-k', '-t', 'bank:1',
                    "printf '" + ('\\007' if kind == 'bell' else 'ACTIVITY') + "'; exec /bin/sleep 238"],
                  consumer_argv=['/bin/sleep', '238'], consumer_label=name + '-' + role)
        text = value if name in TEXT else 'LEFT'
        expectation = dict(text=text)
        if name in STYLES:
            expectation.update(text=STYLES[name][0], fg='red' if value == first else 'blue')
        elif name in ('status-bg', 'status-fg'):
            expectation['bg' if name == 'status-bg' else 'fg'] = value
        elif name.endswith('-length'):
            expectation['text'] = 'ABC' if value == first else 'ABCDEF'
        elif name == 'status-position':
            expectation['row'] = 0 if value == 'top' else 23
        elif name == 'status':
            expectation['row'] = 23 if value == 'on' else 22
        elif name == 'status-justify':
            expectation['text'] = 'CURRENT'
            expectation['column'] = 4 if value == 'left' else 62
        r.obligations['effect'].append(r.add(role, ['refresh-client', '-S'], client_screen=[expectation]))
    if name.startswith('pane-status') or name.startswith('window-pane-'):
        r.add('close-split', ['kill-pane', '-t', 'bank:0.1'])
    if name.startswith('session-status'):
        r.add('close-other-session', ['kill-session', '-t', 'other'])
    r.add('close-consumer', ['kill-window', '-t', 'bank:1'])
    return r.finish()
