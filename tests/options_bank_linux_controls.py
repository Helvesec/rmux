"""Cursor, title and terminfo options consumed by real client controls."""
import shlex

CURSORS = {'cursor-colour', 'cursor-style', 'prompt-cursor-colour',
           'prompt-cursor-style', 'prompt-command-cursor-style'}
OPTIONS = {*CURSORS, 'set-titles', 'set-titles-string', 'allow-passthrough',
           'terminal-features', 'terminal-overrides', 'focus-events'}


def plan(commands, entry):
    from options_bank_linux_recipe import Recipe
    name = entry['name']
    scope = 'server' if name.startswith('terminal-') or name == 'focus-events' else 'window' if name.startswith('cursor-') or name == 'allow-passthrough' else 'session'
    first, second = ('red', 'blue') if name.endswith('-colour') else ('block', 'bar') if 'cursor' in name else ('off', 'on')
    if name == 'set-titles-string':
        first, second = 'FIRST', 'SECOND'
    elif name == 'terminal-features':
        first, second = 'xterm*:title', 'xterm*:title:RGB'
    elif name == 'terminal-overrides':
        first, second = 'xterm*:Ss=FIRST%p1%d', 'xterm*:Ss=SECOND%p1%d'
    r = Recipe(commands, entry, scope, first, second)
    r.mutations()
    if name == 'set-titles':
        r.add('setup-title', ['set-option', '-g', 'set-titles-string', 'OUTER#{window_name}'])
    if name == 'set-titles-string':
        r.add('enable-title', ['set-option', '-g', 'set-titles', 'on'])
    if name == 'terminal-overrides':
        r.add('setup-cursor', ['set-option', '-gw', 'cursor-style', 'bar'])
    if name == 'prompt-command-cursor-style':
        r.add('vi-prompt', ['set-option', '-g', 'status-keys', 'vi'])
    r.set('prime-alternative', second)
    r.add('prime-drain', ['display-message', '-p', ''], client_input=[dict(hex='', after_ms=150)])
    r.obligations['effect'] = []
    for role, value in [('effect', first), ('second-effect', second), ('restored-effect', first)]:
        index = r.set(role + '-value', value)
        commands[index]['client_mark'] = True
        r.obligations['effect'].append(index)
        if name in ('terminal-features', 'terminal-overrides'):
            r.add(role + '-detach', ['detach-client', '-s', 'bank'])
            extra = dict(reattach=role)
            if name == 'terminal-features':
                extra.update(must_differ_from=name + ':effect') if role == 'second-effect' else extra.update(must_equal_to=name + ':effect') if role == 'restored-effect' else None
                argv = ['display-message', '-p', '-c', '$attached', '#{client_termfeatures}']
            else:
                extra['client_control'] = '(?:FIRST|SECOND)[0-9]+'
                argv = ['refresh-client', '-R']
        elif name == 'allow-passthrough':
            ready = 'passthrough-' + role
            code = ('import os,time; from pathlib import Path; '
                    + 'Path(' + repr(ready) + ').write_text("READY"); os.read(0,1); '
                    + 'os.write(1,b"\\x1bPtmux;\\x1b\\x1b]2;PASSTHROUGH\\x07\\x1b\\\\"); time.sleep(236)')
            script = 'stty raw -echo; exec /usr/bin/python3 -c ' + shlex.quote(code)
            r.add(role + '-create', ['new-window', '-t', 'bank:1', '-n', 'effect', script],
                  consumer_argv=['/usr/bin/python3', '-c', code], consumer_exe='/usr/bin/python3', consumer_label=name + '-' + role)
            r.add(role + '-release', ['send-keys', '-H', '-t', 'bank:1', '78'],
                  wait_file=ready, expected_file_bytes='READY')
            argv = ['display-message', '-p', '-t', 'bank:1', '#{cursor_x}']
            extra = dict(client_event=dict(hex='PASSTHROUGH'.encode().hex(), present=value == second))
        elif name in ('set-titles', 'set-titles-string'):
            if name == 'set-titles':
                r.add(role + '-title-trigger', ['rename-window', '-t', 'bank:0', role])
            argv = ['refresh-client', '-R']
            marker = 'OUTER' if name == 'set-titles' else value
            extra = dict(client_event=dict(hex=marker.encode().hex(), present=name == 'set-titles-string' or value == second))
        elif name == 'focus-events':
            r.add(role + '-detach', ['detach-client', '-s', 'bank'])
            argv = ['refresh-client', '-R']
            # Complete the real terminal's DA handshake, which enables features.
            extra = dict(reattach=role, client_input=[dict(hex='1b5b3f313b3263', after_ms=150)],
                         client_event=dict(hex='1b5b3f3130303468', present=value == second))
        else:
            argv = ['refresh-client', '-R']
            extra = dict(client_control='\x1b\\]12;([^\x07\x1b]+)' if name.endswith('-colour') else '\x1b\\[([0-9]+) q')
            if name.startswith('prompt-'):
                argv = ['command-prompt', '-p', 'PROMPT', 'display-message "%%"']
                extra['dismiss_input'] = '0d'
                if name == 'prompt-command-cursor-style':
                    extra['before_input'] = [dict(hex='1b', after_ms=650)]
                    extra['dismiss_input'] = '690d'
        if 'client_control' in extra:
            if role == 'second-effect':
                extra['control_differ_from'] = name + ':effect'
            elif role == 'restored-effect':
                extra['control_equal_to'] = name + ':effect'
        r.obligations['effect'].append(r.add(role, argv, **extra))
        if name == 'allow-passthrough':
            r.add(role + '-close', ['kill-window', '-t', 'bank:1'])
    return r.finish()
