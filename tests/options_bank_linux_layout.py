"""Layout options consumed by the layout engine's actual pane rectangles."""

OPTIONS = {'main-pane-width', 'main-pane-height', 'other-pane-width',
           'other-pane-height', 'tiled-layout-max-columns', 'window-size', 'aggressive-resize'}
RECTANGLES = '#{pane_left}|#{pane_top}|#{pane_width}|#{pane_height}'


def plan(commands, entry):
    from options_bank_linux_recipe import Recipe
    name = entry['name']
    if name in ('window-size', 'aggressive-resize'):
        return client_size_plan(commands, entry)
    first, second = ('1', '2') if name == 'tiled-layout-max-columns' else ('manual', 'latest') if name == 'window-size' else ('20', '30')
    if name == 'other-pane-height':
        first, second = '4', '7'
    r = Recipe(commands, entry, 'window', first, second)
    r.mutations()
    if name == 'window-size':
        r.add('attach-readiness', ['refresh-client'], attach_client=True)
    if name == 'other-pane-height':
        r.add('small-main', ['set-option', '-gw', 'main-pane-height', '6'])
    if name == 'other-pane-width':
        r.add('small-main', ['set-option', '-gw', 'main-pane-width', '6'])
    r.add('create-consumer', ['new-window', '-d', '-t', 'bank:1', '-n', 'effect', '/bin/sleep', '240'])
    for index in range(1, 6 if name == 'tiled-layout-max-columns' else 3):
        duration = str(240 - index)
        r.add('split-' + str(index), ['split-window', '-h', '-d', '-t', 'bank:1', '/bin/sleep', duration],
              consumer_argv=['/bin/sleep', duration], consumer_label=name + '-split-' + str(index))
    layout = 'tiled' if name == 'tiled-layout-max-columns' else 'main-vertical' if name.endswith('-width') else 'main-horizontal'
    r.obligations['effect'] = []
    for role, value in [('effect', first), ('second-effect', second), ('restored-effect', first)]:
        if name == 'window-size' and value == 'manual':
            r.add(role + '-resize', ['resize-window', '-t', 'bank:1', '-x', '90', '-y', '30'])
        r.obligations['effect'].append(r.set(role + '-value', value, local=name == 'window-size'))
        r.obligations['effect'].append(r.add(role + '-layout', ['select-layout', '-t', 'bank:1', layout]))
        extra = {'must_differ_from': name + ':effect'} if role == 'second-effect' else {'must_equal_to': name + ':effect'} if role == 'restored-effect' else {}
        r.obligations['effect'].append(r.add(role, ['list-panes', '-t', 'bank:1', '-F', RECTANGLES],
                                            settle_terminal=True, **extra))
    r.add('close-consumer', ['kill-window', '-t', 'bank:1'])
    return r.finish()


def client_size_plan(commands, entry):
    from options_bank_linux_recipe import Recipe
    name = entry['name']
    first, second = ('smallest', 'largest') if name == 'window-size' else ('off', 'on')
    r = Recipe(commands, entry, 'window', first, second)
    r.mutations()
    if name == 'aggressive-resize':
        r.add('largest-policy', ['set-option', '-gw', 'window-size', 'largest'])
    r.add('create-consumer', ['new-window', '-t', 'bank:1', '-n', 'effect', '/bin/sleep', '240'])
    r.add('other-session', ['new-session', '-d', '-s', 'other', '/bin/sleep', '238'],
          consumer_argv=['/bin/sleep', '238'], consumer_label=name + '-other')
    r.add('share-window', ['link-window', '-d', '-s', 'bank:1', '-t', 'other:1'])
    r.add('sized-client', ['display-message', '-p', ''], add_client=dict(label='sized', session='other', cols=120, rows=40))
    r.obligations['effect'] = []
    for role, value in [('effect', first), ('second-effect', second), ('restored-effect', first)]:
        r.obligations['effect'].append(r.set(role + '-value', value))
        extra = {'must_differ_from': name + ':effect'} if role == 'second-effect' else {'must_equal_to': name + ':effect'} if role == 'restored-effect' else {}
        r.obligations['effect'].append(r.add(role, ['display-message', '-p', '-t', 'bank:1', '#{window_width}|#{window_height}'], settle_terminal=True, **extra))
    r.add('close-other', ['kill-session', '-t', 'other'])
    r.add('close-consumer', ['kill-window', '-t', 'bank:1'])
    return r.finish()
