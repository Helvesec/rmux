"""Destructive policy consumers retain the owned process and private endpoint."""

OPTIONS = {'exit-empty', 'exit-unattached', 'destroy-unattached', 'detach-on-destroy'}


def plan(commands, entry):
    from options_bank_linux_recipe import Recipe
    name = entry['name']
    server = name.startswith('exit-')
    if name == 'exit-empty':
        commands.append(dict(role=name + ':reset-startup', argv=['set-option', '-su', name], error=False))
    r = Recipe(commands, entry, 'server' if server else 'session', 'off', 'on')
    r.mutations()
    r.obligations['effect'] = []

    def create(role, session, duration):
        return r.add(role, ['new-session', '-d', '-s', session, '/bin/sleep', duration],
                     consumer_argv=['/bin/sleep', duration], consumer_label=name + '-' + role)

    if name == 'detach-on-destroy':
        create('other-session', 'other', '238')
        r.add('destroy-first', ['kill-session', '-t', 'bank'])
        r.obligations['effect'].append(r.add('effect', ['list-clients', '-F', '#{client_session}'], 'other\n', attach_client=True))
        r.add('detach-other', ['detach-client', '-s', 'other'])
        create('new-bank', 'bank', '237')
        r.add('second-attach', ['display-message', '-p', ''], reattach='second')
        r.set('second-value', 'on')
        r.add('destroy-second', ['kill-session', '-t', 'bank'])
        r.obligations['effect'].append(r.add('second-effect', ['list-clients', '-F', '#{client_session}'], ''))
        return r.finish()
    if name == 'exit-empty':
        r.add('destroy-first', ['kill-session', '-t', 'bank'])
        r.obligations['effect'].append(r.add('effect', ['has-session', '-t', 'bank'], error=True, expected_daemon_alive=True))
        create('new-bank', 'bank', '237')
    else:
        r.add('detach-first', ['detach-client', '-s', 'bank'], attach_client=True)
        r.obligations['effect'].append(r.add('effect', ['has-session', '-t', 'bank'], expected_daemon_alive=True))
        r.add('second-attach', ['display-message', '-p', ''], reattach='second')
    r.set('second-value', 'on')
    if name == 'exit-unattached':
        r.add('enable-empty-exit', ['set-option', '-s', 'exit-empty', 'on'])
    r.add('destroy-second', ['kill-session', '-t', 'bank'] if name == 'exit-empty' else ['detach-client', '-s', 'bank'])
    r.obligations['effect'].append(r.add('second-effect', ['has-session', '-t', 'bank'], error=True,
                                        expected_daemon_alive=name == 'destroy-unattached'))
    # A terminating policy is deliberately the final operation on this server.
    if name == 'destroy-unattached':
        return r.finish()
    return dict(id=entry['id'], indices=list(range(r.begin, len(commands))), obligations=r.obligations, status='REQUIRED')
