"""Prompt history consumers include the files written by normal daemon closure."""
from pathlib import Path

OPTIONS = {'history-file', 'prompt-history-limit'}
PATHS = {'lock-auto', 'home/first.history', 'home/second.history'}


def capture_files(run, expectations, label):
    from options_bank_linux import require, pin
    result = []
    for index, expected in enumerate(expectations):
        require(expected['path'] in PATHS, 'bounded fixture file target')
        path = run.root / expected['path']
        require(not path.is_symlink(), 'redirected fixture file')
        present = path.exists()
        data = path.read_text() if present else ''
        require(present == expected['exists'] and data == expected['text'], 'application file consumer')
        snapshot = run.root / (label + '-' + str(index) + '.txt')
        snapshot.write_text(data)
        result.append(dict(source=str(path), exists=present, snapshot=pin(snapshot)))
    return result


def verify_files(records, expectations, root):
    from options_bank_linux import require, authenticate
    require(len(records) == len(expectations), 'application file population')
    for record, expected in zip(records, expectations):
        authenticate(record['snapshot'])
        require(record['source'] == str(Path(root) / expected['path']) and record['exists'] == expected['exists']
                and Path(record['snapshot']['path']).parent == Path(root)
                and Path(record['snapshot']['path']).read_text() == expected['text'], 'application file replay')


def plan(commands, entry, contract):
    from options_bank_linux_recipe import Recipe
    name = entry['name']
    first, second = ('~/first.history', '~/second.history') if name == 'history-file' else ('1', '2')
    r = Recipe(commands, entry, 'server', first, second)
    r.mutations()
    r.add('status-on', ['set-option', '-g', 'status', 'on'])
    r.obligations['effect'] = []
    variant = contract.get('default_recipe', {}).get('history_variant', 'first')
    if variant not in ('first', 'second'):
        raise RuntimeError('unknown history variant')
    values = [('effect', first if variant == 'first' else second)] if name == 'history-file' else [('effect', first), ('second-effect', second), ('restored-effect', first)]
    for role, value in values:
        r.set(role + '-value', value)
        for text in ('alpha', 'beta', 'gamma'):
            r.add(role + '-prompt-' + text, ['command-prompt', '-p', 'HISTORY', 'display-message "%%"'],
                  client_screen=[dict(text='HISTORY', row=23)], dismiss_input=(text + '\r').encode().hex())
        extra = {'must_differ_from': name + ':effect'} if role == 'second-effect' else {'must_equal_to': name + ':effect'} if role == 'restored-effect' else {}
        if name == 'history-file':
            text = 'command:alpha\ncommand:beta\ncommand:gamma\n'
            extra['closure_files'] = [dict(path='home/' + variant + '.history', exists=True, text=text),
                                      dict(path='home/' + ('second' if variant == 'first' else 'first') + '.history', exists=False, text='')]
        r.obligations['effect'].append(r.add(role, ['show-prompt-history', '-T', 'command'], **extra))
    if name == 'history-file':
        return dict(id=entry['id'], indices=list(range(r.begin, len(commands))), obligations=r.obligations, status='REQUIRED')
    return r.finish()
