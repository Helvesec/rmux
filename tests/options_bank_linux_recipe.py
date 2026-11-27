"""Shared mutation steps for recipes with independently measured consumers."""
import shlex


class Recipe:
    def __init__(self, plan, entry, scope, first, second):
        self.plan, self.entry, self.begin = plan, entry, len(plan)
        self.name, self.scope = entry['name'], scope
        self.first, self.second, self.obligations = first, second, {}
        self.global_flags = {'server': '-s', 'session': '-g', 'window': '-gw'}[scope]
        self.local_flags = {'server': '-s', 'session': '', 'window': '-w'}[scope]
        self.target = 'bank' if scope == 'session' else 'bank:0'

    def add(self, role, argv, expected=None, error=False, **extra):
        index = len(self.plan)
        row = dict(role=self.name + ':' + role, argv=argv, error=error, **extra)
        if expected is not None:
            row['expected_stdout'] = expected
        self.plan.append(row)
        return index

    def read(self, role, value=None, local=False, extra=''):
        flags = (self.local_flags if local else self.global_flags) + 'v' + extra
        if not flags.startswith('-'):
            flags = '-' + flags
        return self.add(role, ['show-options', flags,
                              *(['-t', self.target] if local and self.scope != 'server' else []), self.name],
                        None if value is None else value + '\n')

    def set(self, role, value=None, flags=None, local=False, command='set-option', error=False):
        flags = flags if flags is not None else self.local_flags if local else self.global_flags
        return self.add(role, [command, *([flags] if flags else []),
                              *(['-t', self.target] if local and self.scope != 'server' else []),
                              self.name, *([value] if value is not None else [])], error=error)

    def mutations(self):
        o, first, second = self.obligations, self.first, self.second
        o['default'] = [self.read('default')]
        o['replace'] = [self.set('replace', first), self.read('value', first),
                        self.add('named', ['show-options', self.global_flags, self.name])]
        local = self.scope != 'server'
        o['scope'] = [self.set('local', second, local=local), self.read('local-value', second, local=local)]
        if local:
            o['scope'] += [self.read('global-value', first),
                           self.add('invalid-target', ['set-option', *([self.local_flags] if self.local_flags else []),
                                                       '-t', 'missing-recipe-target', self.name, first], error=True)]
        else:
            for flag in ('-g', '-w', '-p'):
                o['scope'] += [self.set('inferred' + flag, second, flag), self.read('inferred-value' + flag, second)]
        o['only-if-unset'] = [self.set('only-existing', first, (self.local_flags or '-') + 'o', local=local, error=True),
                              self.read('only-value', second, local=local)]
        o['quiet'] = [self.set('only-quiet', first, (self.local_flags or '-') + 'oq', local=local),
                       self.read('quiet-value', second, local=local),
                       self.add('quiet-missing', ['show-options', '-qv', '-t', 'bank', '@missing-recipe-value'], '')]
        o['unset'] = [self.set('unset', flags=(self.local_flags or '-') + 'u', local=local), self.read('inherited', None if not local else first, local=local, extra='A')]
        if local:
            o['only-if-unset'] += [self.set('only-absent', second, (self.local_flags or '-') + 'o', local=True),
                                   self.read('only-absent-value', second, local=True),
                                   self.set('clear-local', flags=(self.local_flags or '-') + 'u', local=True)]
        o['format'] = [self.set('format', '#{?session_name,' + first + ',' + second + '}', self.global_flags + 'F'),
                        self.read('formatted-value', first), self.read('hook-inclusive-value', first, extra='H')]
        o['append'] = [self.set('append', second, self.global_flags + 'a')]
        o['configuration'] = [self.add('source', ['source-file', '-'],
                                        source_text='set-option ' + self.global_flags + ' ' + self.name + ' ' + shlex.quote(first) + '\n'),
                               self.read('source-value', first)]
        o['sibling'] = [self.set('window-command', first, '-g', command='set-window-option'), self.read('window-value', first)]
        if self.scope == 'window':
            o['sibling'] += [self.set('unset-panes', flags='-wU', local=True), self.read('inherited-panes', first, local=True, extra='A')]
        self.set('prepare-consumer', first)

    def terminal(self, role, output, observe, expected, setup=None, script=None, targeted=True):
        label, target = self.name + '-' + role, 'bank:1'
        indices = [self.add(role + '-create', ['new-window', '-d', '-t', 'bank:', '-n', 'effect',
                                             (script or 'printf ' + shlex.quote(output)) + '; exec /bin/sleep 236'],
                            consumer_argv=['/bin/sleep', '236'], consumer_label=label)]
        if setup:
            indices.extend(self.add(role + '-setup-' + str(i), argv) for i, argv in enumerate(setup))
        indices += [self.add(role, [*observe, *(['-t', target] if targeted else []), *expected[0]], expected[1], settle_terminal=True),
                     self.add(role + '-close', ['kill-window', '-t', target])]
        self.obligations.setdefault('effect', []).extend(indices)

    def finish(self):
        self.set('restore-default', flags=self.global_flags + 'u')
        return dict(id=self.entry['id'], indices=list(range(self.begin, len(self.plan))),
                    obligations=self.obligations, status='REQUIRED')
