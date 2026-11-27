"""Application protocols consume clipboard, parser, editor and locking options."""
import shlex

OPTIONS = {'input-buffer-size', 'set-clipboard', 'get-clipboard', 'editor',
           'lock-command', 'lock-after-time'}


def plan(commands, entry):
    from options_bank_linux_recipe import Recipe
    name = entry['name']
    first, second = ('1048576', '2097152') if name == 'input-buffer-size' else ('off', 'on') if name == 'set-clipboard' else ('off', 'buffer') if name == 'get-clipboard' else ('0', '1') if name == 'lock-after-time' else (
        "/bin/sh -c 'printf FIRST > \"$1\"' editor", "/bin/sh -c 'printf SECOND > \"$1\"' editor") if name == 'editor' else (
        'printf FIRST > lock-first', 'printf SECOND > lock-second')
    r = Recipe(commands, entry, 'session' if name.startswith('lock-') else 'server', first, second)
    r.mutations()
    r.obligations['effect'] = []
    if name == 'get-clipboard':
        r.add('allow-clipboard-query', ['set-option', '-s', 'set-clipboard', 'on'])
        r.add('seed-clipboard', ['set-buffer', 'HELLO'])
    if name == 'editor':
        r.add('editor-client', ['refresh-client'], attach_client=True)
    if name == 'lock-after-time':
        r.add('locking-application', ['set-option', '-g', 'lock-command', 'printf LOCKED > lock-auto'])
        r.add('clear-fixture', ['run-shell', 'rm -f lock-auto'])
    for role, value in [('effect', first), ('second-effect', second), ('restored-effect', first)]:
        if name == 'lock-after-time' and role == 'restored-effect':
            break
        r.obligations['effect'].append(r.set(role + '-value', value))
        if name == 'input-buffer-size':
            script = '/usr/bin/python3 -c ' + shlex.quote('import sys; sys.stdout.buffer.write(b"\x1b]2;" + b"x" * 1500000 + b"\x07")') + '; exec /bin/sleep 236'
            r.terminal(role, '', ['display-message', '-p'], (['#{n:pane_title}'], '1500000\n' if value == second else None), script=script)
            row = next(row for row in commands if row['role'] == name + ':' + role)
            if role == 'second-effect':
                row['must_differ_from'] = name + ':effect'
            elif role == 'restored-effect':
                row['must_equal_to'] = name + ':effect'
        elif name == 'set-clipboard':
            r.terminal(role, '\\033]52;c;SEVMTE8=\\007', ['list-buffers', '-F', '#{buffer_sample}'],
                       ([], 'HELLO\n' if value == second else ''), targeted=False)
            if value == second:
                r.add(role + '-clear-buffer', ['delete-buffer'])
        elif name == 'get-clipboard':
            code = ('import os,time; os.write(1,b"\x1b]52;c;?\x07"); data=b""\n'
                    'while not data.endswith((b"\x07",b"\x1b\\\\")): data+=os.read(0,65536)\n'
                    'os.write(1,data.hex().encode()); time.sleep(236)')
            script = 'stty raw -echo; exec /usr/bin/python3 -c ' + shlex.quote(code)
            r.add(role + '-create', ['new-window', '-d', '-t', 'bank:1', '-n', 'effect', script],
                  consumer_argv=['/usr/bin/python3', '-c', code], consumer_exe='/usr/bin/python3', consumer_label=name + '-' + role)
            extra = {'must_differ_from': name + ':effect'} if role == 'second-effect' else {'must_equal_to': name + ':effect'} if role == 'restored-effect' else {}
            r.obligations['effect'].append(r.add(role, ['capture-pane', '-p', '-t', 'bank:1', '-S', '0', '-E', '0'], settle_terminal=True, **extra))
            r.add(role + '-close', ['kill-window', '-t', 'bank:1'])
        elif name == 'editor':
            r.add(role + '-buffer', ['set-buffer', '-b', 'edited', 'BASE'])
            r.add(role + '-chooser', ['choose-buffer', '-t', 'bank:0'])
            r.add(role + '-edit', ['send-keys', '-t', 'bank:0', 'e'])
            r.obligations['effect'].append(r.add(role, ['show-buffer', '-b', 'edited'], 'FIRST' if value == first else 'SECOND', settle_terminal=True))
            r.add(role + '-close', ['send-keys', '-t', 'bank:0', 'q'])
        elif name == 'lock-command':
            r.add(role + '-lock', ['lock-client', '-t', '$attached'], attach_client=True)
            file = 'lock-first' if value == first else 'lock-second'
            text = 'FIRST' if value == first else 'SECOND'
            r.obligations['effect'].append(r.add(role, ['load-buffer', '-b', 'locked', file], wait_file=file, expected_file_bytes=text))
            r.obligations['effect'].append(r.add(role + '-buffer', ['show-buffer', '-b', 'locked'], text))
        else:
            r.obligations['effect'].append(r.add(role, ['display-message', '-p', ''],
                client_input=[dict(hex='78', after_ms=3000)], files_expected=[dict(path='lock-auto', exists=value == second,
                                                                            text='LOCKED' if value == second else '')]))
    return r.finish()
