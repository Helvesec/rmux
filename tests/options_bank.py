#!/usr/bin/env python3
"""Common generated options bank. Never treats missing native evidence as a skip.

generate REGISTRY CONTRACT OUTPUT; check REGISTRY CONTRACT OUTPUT; canaries REGISTRY CONTRACT OUTPUT
The Rust test export is generated from registry::registry(), with its independent
authenticated tmux scanner. Native contributions use the same cell identifiers.
"""
import copy
import ast
import datetime
import re
import gzip
import hashlib
import itertools
import json
from pathlib import Path
import sys
import subprocess
import tarfile


RECEIPT_BINDING_V1 = {'schema': 1,
 'cell': {'scope': 'server-global',
          'os': 'linux',
          'route': 'cli.by-name',
          'input_position': 'after-input',
          'terminal': 'tty',
          'sequence': 'isolated',
          'outcome': 'success',
          'operation': 'replace'},
 'operations': [{'role': 'preload-first',
                 'step': 'display-first',
                 'argv': ['display-message', '-c', '$client', '$first']},
                {'role': 'preload-last',
                 'step': 'display-last',
                 'argv': ['display-message', '-c', '$client', '$last']},
                {'role': 'before', 'step': 'query-history-before', 'argv': ['show-messages']},
                {'role': 'mutation',
                 'step': 'set-limit-1',
                 'argv': ['set-option', '-s', '$option', '1']},
                {'role': 'value',
                 'step': 'query-value',
                 'argv': ['show-options', '-sv', '$option']},
                {'role': 'after', 'step': 'query-history-after', 'argv': ['show-messages']}],
 'kind': 'message-limit-preloaded-v1'}


class GateError(ValueError):
    pass


def require(ok, reason):
    if not ok:
        raise GateError(reason)


def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def load(path):
    return json.loads(Path(path).read_text())


def save(path, data):
    with Path(path).open('x') as stream:
        json.dump(data, stream, indent=2, sort_keys=True)
        stream.write('\n')


def pin(path):
    path = Path(path).resolve()
    return {'path': str(path), 'bytes': path.stat().st_size, 'sha256': sha(path)}


def authenticate(item):
    p = Path(item['path'])
    require(p.is_file(), 'missing artifact: ' + str(p))
    require(p.stat().st_size == item['bytes'] and sha(p) == item['sha256'],
            'changed artifact: ' + str(p))


def component(value):
    return isinstance(value, str) and re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.:-]*', value) is not None


def unique_components(values, label):
    require(isinstance(values, list) and values and all(component(x) for x in values), 'empty/ambiguous ' + label)
    require(len(values) == len(set(values)), 'duplicate ' + label)


def structure_gate(registry, contract):
    """Prove injective ID components before yielding any Cartesian product."""
    require(type(contract.get('schema')) is int and contract['schema'] == 1, 'contract schema')
    require(contract.get('criterion') == '2A.options', 'contract criterion')
    require(type(registry.get('schema')) is int and registry['schema'] == 1, 'registry schema')
    require(type(registry['registry_count']) is int and registry['registry_count'] > 0, 'registry count schema')
    order, axes, selectors = contract['axis_order'], contract['axes'], contract['scope_selectors']
    unique_components(order, 'axis order')
    require(isinstance(axes, dict) and set(axes) == set(order), 'axes/order mismatch')
    for key in order:
        unique_components(axes[key], 'axis ' + key)
    require(isinstance(selectors, dict) and selectors and all(component(x) for x in selectors), 'empty/ambiguous scope selectors')
    flattened = []
    for key, values in selectors.items():
        unique_components(values, 'selectors ' + key)
        flattened.extend(values)
    require(len(flattened) == len(set(flattened)), 'duplicate cross-scope selector')
    entries = registry['options']
    require(isinstance(entries, list) and entries, 'empty registry')
    unique_components([x['id'] for x in entries], 'option IDs')
    unique_components([x['name'] for x in entries], 'option names')
    for entry in entries:
        unique_components(entry['scopes'], 'option scopes')
        require(set(entry['scopes']) <= set(selectors), 'unknown scope')


def command_time(value):
    require(isinstance(value, str), 'command timestamp type')
    try:
        stamp = datetime.datetime.fromisoformat(value)
    except ValueError as error:
        raise GateError('invalid command timestamp') from error
    require(stamp.tzinfo is not None and stamp.utcoffset() is not None, 'timestamp timezone')
    return stamp


def bound_recipe(cell_id, contract):
    authenticate(contract['registry'])
    registry = load(contract['registry']['path'])
    inventory_gate(registry, contract)
    recipe, binding = contract['recipe'], contract['receipt_binding']
    require(type(binding['schema']) is int and binding['schema'] == 1, 'receipt binding schema')
    require(binding == RECEIPT_BINDING_V1 and recipe['option_id'] == 'wire:MessageLimit',
            'unsupported cell/operation binding')
    require(hashlib.sha256(json.dumps(recipe, sort_keys=True).encode()).hexdigest() == contract['recipe_sha256'], 'recipe digest')
    cells = contract['native_cell_ids']
    require(isinstance(cells, list) and cells and all(isinstance(x, str) for x in cells) and len(cells) == len(set(cells)), 'native cell IDs')
    require(cell_id in cells, 'unfrozen native cell')
    option = [x for x in registry['options'] if x['id'] == recipe['option_id']]
    require(len(option) == 1, 'recipe option identity')
    context = binding['cell']
    require(set(context) == {'scope', *contract['axis_order']}, 'receipt binding axes')
    selectors = [s for kind in option[0]['scopes'] for s in contract['scope_selectors'][kind]]
    require(context['scope'] in selectors, 'recipe scope')
    require(all(context[k] in contract['axes'][k] for k in contract['axis_order']), 'recipe axis value')
    expected_id = '|'.join([recipe['option_id'], context['scope'], *(context[k] for k in contract['axis_order'])])
    require(cell_id == expected_id, 'cell/recipe identity')
    roles = ['preload-first', 'preload-last', 'before', 'mutation', 'value', 'after']
    operations = binding['operations']
    require(isinstance(operations, list) and [x['role'] for x in operations] == roles, 'frozen role order')
    unique_components(recipe['steps'], 'recipe steps')
    steps = [recipe['steps'].index(x['step']) for x in operations]
    require(steps == sorted(set(steps)), 'recipe step order')
    markers = recipe['markers']
    require(set(markers) == {'first', 'last', 'append'} and all(component(x) for x in markers.values())
            and len(set(markers.values())) == 3, 'recipe markers')
    return option[0]['name'], operations


def bound_commands(receipt, command_ids, option_name, operations, contract):
    roles = [x['role'] for x in operations]
    all_commands = receipt['commands']
    require(isinstance(command_ids, list) and len(command_ids) == len(roles)
            and all(type(i) is int and 0 <= i < len(all_commands) for i in command_ids), 'cell command indices')
    require(command_ids == sorted(set(command_ids)), 'cell command order/uniqueness')
    commands = [all_commands[i] for i in command_ids]
    require([x['role'] for x in commands] == roles, 'operation role order')
    require(all(sum(c['role'] == role for c in all_commands) == 1 for role in roles), 'duplicate required role')
    require(receipt['markers'] == contract['recipe']['markers'], 'markers not frozen recipe')
    public, attached = receipt['public'], receipt['attached_client'].split('|')
    require(isinstance(public, list) and len(public) == 6 and isinstance(public[1], str) and public[1].startswith('/')
            and len(attached) == 2 and attached[0].isdigit() and int(attached[0]) > 0
            and attached[1].startswith('/dev/pts/') and attached[1][9:].isdigit(), 'private socket/client target')
    prefix = [receipt['binary']['path'], '-N', '-S', public[1]]
    substitutions = {'$option': option_name, '$client': attached[1], **{'$' + k: v for k, v in receipt['markers'].items()}}
    previous, final = command_time(receipt['started']), command_time(receipt['ended'])
    require(previous <= final, 'receipt end before start')
    for command, operation in zip(commands, operations):
        args = operation['argv']
        require(isinstance(args, list) and args and all(isinstance(x, str) and x for x in args), 'frozen argv schema')
        require(all(not x.startswith('$') or x in substitutions for x in args), 'unknown argv substitution')
        expected = prefix + [substitutions.get(x, x) for x in args]
        require(command['argv'] == expected, 'argv binding: ' + operation['role'])
        require(type(command['exit_code']) is int and command['exit_code'] == 0
                and type(command['pid']) is int and command['pid'] > 0, 'failed/nonexecuted command')
        start, end = command_time(command['started']), command_time(command['ended'])
        require(start <= end, 'command end before start')
        require(previous <= start and end <= final, 'command chronology')
        previous = end
        for channel in ('stdout', 'stderr'):
            require(hashlib.sha256(command[channel].encode()).hexdigest() == command[channel + '_sha256'], 'output digest')
    return {x['role']: x for x in commands}


def inventory_gate(registry, contract):
    structure_gate(registry, contract)
    oracle = registry['oracle']
    for key, expected in contract['oracle_identity'].items():
        require(oracle.get(key) == expected, 'oracle identity/symbol: ' + key)
    require(registry['registry_symbol'] == 'registry::table::OPTIONS', 'registry symbol')
    require(registry['target_os'] == contract.get('native_os', 'linux'), 'export target')
    options = registry['options']
    ids = [entry['id'] for entry in options]
    names = [entry['name'] for entry in options]
    require(len(ids) == len(set(ids)) == registry['registry_count'], 'registry cardinality/IDs')
    require(len(names) == len(set(names)), 'duplicate name')
    require(set(names) == set(oracle['ordinary_names']), 'independent oracle name set mismatch')
    identities = canonical_registry_identities()
    require(set(names)==set(identities), 'canonical registry population')
    require(all(entry['id']==identities[entry['name']]['id'] and entry['wire_symbol']==identities[entry['name']]['wire_symbol'] for entry in options), 'canonical stable ID association')
    require(len(oracle['ordinary_names']) == len(set(oracle['ordinary_names'])), 'duplicate oracle name')
    for entry in options:
        require(entry['scopes'] and entry['default'] and entry['value_type'], 'missing metadata')
        require(entry['id'] == ('wire:' + entry['wire_symbol'] if entry['wire_symbol']
                               else 'named:' + entry['name']), 'unstable ID')
        require(set(entry['scopes']) <= set(contract['scope_selectors']), 'unknown scope')
        require(entry['consumer']['status'] == 'FAIL_MISSING', 'static mask is not consumer evidence')


def cell_ids(registry, contract):
    structure_gate(registry, contract)
    axes = contract['axes']
    for entry in sorted(registry['options'], key=lambda x: x['id']):
        scopes = sorted(s for kind in entry['scopes'] for s in contract['scope_selectors'][kind])
        for fields in itertools.product(scopes, *(axes[key] for key in contract['axis_order'])):
            yield '|'.join((entry['id'], *fields))


def population(registry, contract):
    structure_gate(registry, contract)
    count = sum(len(contract['scope_selectors'][scope])
                for entry in registry['options'] for scope in entry['scopes'])
    for axis in contract['axis_order']:
        count *= len(contract['axes'][axis])
    return count


def exact_rows(expected, actual):
    count = 0
    digest = hashlib.sha256()
    for want, got in itertools.zip_longest(expected, actual):
        require(want == got and want is not None, f'cell set/order mismatch at {count}: {want!r}/{got!r}')
        digest.update((want + '\n').encode())
        count += 1
    return {'cells': count, 'ids_sha256': digest.hexdigest()}


DEFAULT_CELL_V1 = {
    'scope': 'window-global', 'os': 'linux', 'route': 'cli.by-name',
    'input_position': 'before-input', 'terminal': 'non-tty',
    'sequence': 'isolated', 'outcome': 'success', 'operation': 'default',
}
DEFAULT_OPERATIONS_V1 = [
    {'role': 'inventory', 'argv': ['show-options', '-gw']},
    {'role': 'named', 'argv': ['show-options', '-gw', '$option']},
    {'role': 'value', 'argv': ['show-options', '-gwv', '$option']},
]


def default_plan(contract):
    """One supported read recipe; other routes/operations remain missing."""
    if contract.get('default_recipe', {}).get('kind') in ('registry-global-default-v1', 'registry-global-default-v2'):
        return registry_plan(contract)
    authenticate(contract['registry'])
    registry = load(contract['registry']['path'])
    inventory_gate(registry, contract)
    recipe = contract['default_recipe']
    require(type(recipe['schema']) is int and recipe['schema'] == 1
            and recipe['kind'] == 'global-window-default-v1', 'default recipe kind/schema')
    require(recipe['cell'] == DEFAULT_CELL_V1
            and recipe['operations'] == DEFAULT_OPERATIONS_V1, 'unsupported default observation/operation')
    require(set(recipe['cell']) == {'scope', *contract['axis_order']}, 'default cell axes')
    require(all(recipe['cell'][key] in contract['axes'][key] for key in contract['axis_order']),
            'default axis outside matrix')
    unique_components(recipe['option_ids'], 'default option IDs')
    require(len(contract['native_cell_ids']) == len(set(contract['native_cell_ids'])), 'duplicate native cell')
    require(hashlib.sha256(json.dumps(recipe, sort_keys=True).encode()).hexdigest()
            == contract['default_recipe_sha256'], 'default recipe digest')
    # A caller cannot set the measured default during setup and label it a default.
    require(recipe['startup'] == 'set-option -s exit-empty off\nset-option -g default-shell /bin/bash\n'
            'set-option -g status off\nset-option -gw automatic-rename off\n', 'default fixture startup')
    plan = [{'role': 'defaults-inventory', 'argv': ['show-options', '-gw']}]
    cells = {}
    for option_id in recipe['option_ids']:
        options = [entry for entry in registry['options'] if entry['id'] == option_id]
        require(len(options) == 1 and 'window' in options[0]['scopes'], 'default option/scope identity')
        name = options[0]['name']
        cell = '|'.join([option_id, recipe['cell']['scope'],
                         *(recipe['cell'][key] for key in contract['axis_order'])])
        require(cell in contract['native_cell_ids'], 'unfrozen default cell')
        indices = [0]
        for operation in recipe['operations'][1:]:
            indices.append(len(plan))
            plan.append({'role': 'defaults-' + operation['role'] + '-' + name,
                         'argv': [name if value == '$option' else value for value in operation['argv']]})
        cells[cell] = {'name': name, 'indices': indices}
    return plan, cells


def default_birth(birth, pid, label):
    """Validate the collector's recorded Linux identity without querying live PIDs."""
    require(isinstance(birth, dict) and type(pid) is int and pid > 0
            and type(birth.get('pid')) is int and birth['pid'] == pid
            and isinstance(birth.get('start_ticks'), str)
            and re.fullmatch(r'[0-9]+', birth['start_ticks']) is not None
            and type(birth.get('ppid')) is int and birth['ppid'] >= 0
            and isinstance(birth.get('state'), str)
            and re.fullmatch(r'[A-Za-z]', birth['state']) is not None,
            'default ' + label + ' birth identity')


def linux_closure(receipt, label=''):
    """A PASS label cannot contradict the collector's completion/closure facts."""
    closure = receipt.get('closure')
    require(isinstance(closure, dict) and receipt.get('execution_status') == 'COMPLETE'
            and closure.get('status') == 'PASS', 'unclosed/incomplete ' + label + 'product')
    require(closure.get('errors') == [], label + 'closure errors')
    # Linux collector accepts only ENOENT or ECONNREFUSED, never connect success.
    require(type(closure.get('endpoint_connect_errno')) is int
            and closure['endpoint_connect_errno'] in (2, 111), label + 'endpoint not closed')
    return closure


def default_phase(receipt, contract, phase, *, allow_synthetic=False):
    plan, cells = default_plan(contract)
    require(receipt.get('SYNTHETIC_ONLY_NOT_NATIVE_EVIDENCE', False) is allow_synthetic,
            'synthetic receipt cannot receive native credit')
    require(receipt['phase'] == phase and receipt['native_os'] == 'linux', 'default execution identity')
    require(receipt['binary'] == contract['native_binaries'][phase], 'default binary identity')
    require(receipt['source'] == contract['native_source'], 'default source identity')
    require(receipt['recipe_sha256'] == contract['default_recipe_sha256'], 'default recipe identity')
    require(receipt['driver'] == contract['default_driver'], 'default driver identity')
    closure = linux_closure(receipt, 'default ')
    require(isinstance(closure.get('exits'), dict)
            and set(closure['exits']) == {'daemon', 'helper'}, 'default closure process population')
    require(receipt['fd_balance']['before'] == receipt['fd_balance']['after'], 'default FD balance')
    require(not receipt.get('attached_client'), 'default non-tty context')
    require(receipt['startup']['sha256'] == hashlib.sha256(contract['default_recipe']['startup'].encode()).hexdigest(),
            'default startup identity')
    require(set(receipt['processes']) == {'daemon', 'helper'}, 'default fixture process population')
    for label, expected in [('daemon', contract['native_binaries'][phase]), ('helper', contract['python'])]:
        process = receipt['processes'][label]
        exit_record = closure['exits'][label]
        require(isinstance(process, dict) and isinstance(exit_record, dict), 'default process/exit record')
        require(process['exe_sha256'] == expected['sha256'], 'default process executable')
        birth = process.get('identity')
        default_birth(birth, birth.get('pid') if isinstance(birth, dict) else None, label)
        default_birth(exit_record.get('birth'), birth['pid'], label + ' exit')
        require(exit_record['birth']['start_ticks'] == birth['start_ticks']
                and exit_record['birth']['ppid'] == birth['ppid']
                and exit_record.get('pidfd_exit') is True and exit_record['after'] is None,
                'default process birth/closure')
        require('wait_status' in exit_record, 'default missing wait status')
        status = exit_record['wait_status']
        # Direct daemon has a Popen wait result. Helper may already be reaped by
        # its parent: None is UNKNOWN, not a successful numeric wait invented here.
        require(type(status) is int if label == 'daemon' else
                status is None or (type(status) is int and status >= 0), 'default wait status type')
    daemon, helper = [receipt['processes'][label]['identity'] for label in ('daemon', 'helper')]
    require(daemon['pid'] != helper['pid'] and helper['ppid'] == daemon['pid'],
            'default helper parent identity')
    public = receipt['public']
    require(isinstance(public, list) and len(public) == 6
            and all(isinstance(value, str) for value in public)
            and Path(public[1]).is_absolute() and public[2] == 'bank'
            and re.fullmatch(r'@[0-9]+', public[3]) is not None
            and re.fullmatch(r'%[0-9]+', public[4]) is not None
            and public[0] == str(daemon['pid']) and public[5] == str(helper['pid']),
            'default private endpoint')
    endpoint = receipt['endpoint']
    require(isinstance(endpoint, dict) and endpoint.get('path') == public[1]
            and type(endpoint.get('device')) is int and endpoint['device'] >= 0
            and type(endpoint.get('inode')) is int and endpoint['inode'] > 0,
            'default endpoint identity')
    root = str(Path(public[1]).parent)
    prefix = [receipt['binary']['path'], '-N', '-S', public[1]]
    expected = [
        {'role': 'new-session', 'argv': ['new-session', '-d', '-s', 'bank', '-n', 'bank',
         '-x', '80', '-y', '24', '-c', root, contract['python']['path'], '-I',
         contract['default_driver']['path'], 'helper', root, contract['default_recipe']['token']]},
        {'role': 'public-inventory', 'argv': ['list-panes', '-a', '-F',
         '#{pid}|#{socket_path}|#{session_name}|#{window_id}|#{pane_id}|#{pane_pid}']},
        *plan, {'role': 'closure-kill-server', 'argv': ['kill-server']}]
    commands = receipt['commands']
    require(isinstance(commands, list) and len(commands) == len(expected), 'default complete command sequence')
    start, final = command_time(receipt['started']), command_time(receipt['ended'])
    previous = start
    cleanup_start, cleanup_end = command_time(closure['started']), command_time(closure['ended'])
    elapsed = closure.get('elapsed_seconds')
    require(start <= cleanup_start <= cleanup_end <= final and (final-start).total_seconds() <= 300
            and type(elapsed) in (int, float) and 0 <= elapsed <= 60
            and (cleanup_end-cleanup_start).total_seconds() <= 60, 'default time envelope')
    for command, operation in zip(commands, expected):
        require(isinstance(command, dict), 'default command record')
        require(command['role'] == operation['role'] and command['argv'] == prefix + operation['argv'],
                'default operation/order/target: ' + operation['role'])
        require(type(command['exit_code']) is int and command['exit_code'] == 0
                and type(command['pid']) is int and command['pid'] > 0, 'default command not successful')
        default_birth(command.get('birth'), command['pid'], 'command')
        require(command['birth']['ppid'] == daemon['ppid'], 'default command parent identity')
        begin, end = command_time(command['started']), command_time(command['ended'])
        require(previous <= begin <= end <= final, 'default command chronology')
        previous = end
        require(command['wait_method'] == 'Popen.communicate' and command['after'] is None,
                'default command closure')
        for channel in ('stdout', 'stderr'):
            require(hashlib.sha256(command[channel].encode()).hexdigest()
                    == command[channel + '_sha256'], 'default output digest')
        require(command['stderr'] == '', 'default command stderr')
    require(commands[1]['stdout'] == '|'.join(public) + '\n', 'default public inventory identity')
    require(set(receipt['cells']) == set(cells), 'default missing/extra cell')
    if contract['default_recipe']['kind'] in ('registry-global-default-v1', 'registry-global-default-v2'):
        return registry_observations(receipt, contract, cells, phase)
    inventory = commands[2]['stdout'].splitlines()
    observations = {}
    for cell, spec in cells.items():
        indices = [index+2 for index in spec['indices']]
        require(receipt['cells'][cell] == indices
                and all(type(index) is int for index in receipt['cells'][cell]),
                'default cell observation indices')
        name = spec['name']
        rows = [line for line in inventory if line == name or line.startswith(name + ' ')]
        require(len(rows) == 1, 'default missing/duplicate inventory row')
        named, value = [commands[index]['stdout'] for index in indices[1:]]
        require(named == rows[0] + '\n', 'default named/unfiltered disagreement')
        require(value.endswith('\n') and len(value.splitlines()) == 1, 'default scalar observation')
        observations[cell] = {'name': name, 'named': named, 'value': value}
    if phase == 'oracle':
        authenticate(contract['default_oracle_baseline'])
        baseline = load(contract['default_oracle_baseline']['path'])
        authenticate(baseline['receipt'])
        archived = load(baseline['receipt']['path'])
        require(archived['binary'] == contract['native_binaries']['oracle'], 'default baseline oracle identity')
        raw = [command['stdout'] for command in archived['commands']
               if command['role'] == 'inventory--gw']
        require(len(raw) == 1, 'default archived window observation')
        for observed in observations.values():
            reference = baseline['options'][observed['name']]
            require(reference['named'].rstrip('\n') in raw[0].splitlines(),
                    'default baseline not archived raw observation')
            require(observed['named'] == reference['named'] and observed['value'] == reference['value'],
                    'default independent oracle baseline mismatch')
    return observations


def actual_default_pair(cell_id, pair, contract, *, allow_synthetic=False):
    require(set(pair) == {'oracle', 'candidate'}, 'missing actual default oracle/candidate')
    results, receipts = {}, {}
    for phase in ('oracle', 'candidate'):
        authenticate(pair[phase])
        receipts[phase] = load(pair[phase]['path'])
        results[phase] = default_phase(receipts[phase], contract, phase, allow_synthetic=allow_synthetic)
        require(cell_id in results[phase], 'unexecuted default cell')
    require(command_time(receipts['oracle']['ended']) <= command_time(receipts['candidate']['started']),
            'default products not sequential')
    require(results['oracle'][cell_id] == results['candidate'][cell_id], 'default oracle/candidate mismatch')
    return results['candidate'][cell_id]


def default_canaries(contract, pair, output):
    """Mutations of actual records are explicitly synthetic and never native credit."""
    root = output / 'default-canaries'
    root.mkdir()
    _, cells = default_plan(contract)
    selected = next(iter(cells))
    original = {phase: load(reference['path']) for phase, reference in pair.items()}
    results = []
    def case(name, mutate=None, expected='FAIL', reason='', native=False, remove_oracle=False):
        records, local = copy.deepcopy(original), copy.deepcopy(contract)
        for record in records.values():
            record['SYNTHETIC_ONLY_NOT_NATIVE_EVIDENCE'] = True
        if mutate:
            mutate(records, local)
        folder = root / name
        folder.mkdir()
        references = {}
        for phase, record in records.items():
            path = folder / (phase + '.json')
            save(path, record)
            references[phase] = pin(path)
        if remove_oracle:
            references.pop('oracle')
        try:
            actual_default_pair(selected, references, local, allow_synthetic=not native)
            actual, detail = 'PASS', 'returned normally'
        except (ValueError, KeyError, TypeError, IndexError, OSError) as error:
            actual, detail = 'FAIL', str(error)
        results.append({'id': name, 'expected_gate': expected, 'observed_gate': actual,
                        'reason': detail, 'status': 'PASS' if actual == expected
                        and (not reason or reason in detail) else 'FAIL'})
    def command(change, index=3):
        def mutation(records, contract):
            change(records['candidate']['commands'][index])
        return mutation
    case('exact-observed-oracle-default', expected='PASS')
    case('synthetic-cannot-be-native', native=True, reason='synthetic receipt')
    case('missing-oracle', remove_oracle=True, reason='missing actual')
    case('missing-cell', lambda rs,c: rs['candidate']['cells'].pop(selected), reason='missing/extra cell')
    case('wrong-observation', command(lambda x: x['argv'].__setitem__(4, 'show-messages')),
         reason='operation/order/target')
    case('read-replaced-by-set', command(lambda x: x.update(argv=x['argv'][:4]+['set-option','-gw','pane-border-format','wrong'])),
         reason='operation/order/target')
    case('wrong-order', lambda rs,c: rs['candidate']['commands'].__setitem__(
        slice(3,5), list(reversed(rs['candidate']['commands'][3:5]))), reason='operation/order/target')
    case('wrong-cell-indices', lambda rs,c: rs['candidate']['cells'].__setitem__(selected, [2,4,3]),
         reason='observation indices')
    case('wrong-binary', lambda rs,c: rs['candidate']['binary'].update(sha256='0'*64), reason='binary identity')
    case('wrong-source', lambda rs,c: rs['candidate'].update(source='0'*40), reason='source identity')
    case('wrong-recipe', lambda rs,c: rs['candidate'].update(recipe_sha256='0'*64), reason='recipe identity')
    case('wrong-executable-target', command(lambda x: x['argv'].__setitem__(0,'/nonexistent/other')),
         reason='operation/order/target')
    case('wrong-socket', command(lambda x: x['argv'].__setitem__(3,'/nonexistent/other.socket')),
         reason='operation/order/target')
    case('raw-output-tamper', command(lambda x: x.update(stdout=x['stdout']+'tamper\n')), reason='output digest')
    def rehashed_wrong_value(records, contract):
        x = records['candidate']['commands'][4]
        x['stdout'] = '1\n'
        x['stdout_sha256'] = hashlib.sha256(x['stdout'].encode()).hexdigest()
    case('printed-value-without-correct-default', rehashed_wrong_value, reason='oracle/candidate mismatch')
    case('default-mutated-in-startup', lambda rs,c: c['default_recipe'].update(
        startup=c['default_recipe']['startup']+'set-option -gw pane-border-format wrong\n'),
         reason='recipe digest')
    case('incomplete-closure', lambda rs,c: rs['candidate']['closure'].update(status='FAIL'),
         reason='unclosed/incomplete')
    case('wrong-oracle-symbol', lambda rs,c: c['oracle_identity'].update(symbol='wrong'),
         reason='oracle identity/symbol')

    # Shift the entire product, including closure, to isolate the pair boundary
    # from local chronology. Exercise both timezone representations independently.
    def boundary(oracle_zone, candidate_zone, micros):
        def mutate(records, contract):
            delta = (command_time(records['oracle']['ended'])
                     + datetime.timedelta(microseconds=micros)
                     - command_time(records['candidate']['started']))
            for phase, hours in [('oracle', oracle_zone), ('candidate', candidate_zone)]:
                record = records[phase]
                offset = delta if phase == 'candidate' else datetime.timedelta()
                zone = datetime.timezone(datetime.timedelta(hours=hours))
                for item in [record, *record['commands'], record['closure']]:
                    for key in ('started', 'ended'):
                        item[key] = (command_time(item[key]) + offset).astimezone(zone).isoformat()
        return mutate
    for oracle_zone, candidate_zone in itertools.product((-12, 0, 14), repeat=2):
        for micros in (-86400000000, -1, 0, 1, 1000000):
            case(f'zone-{oracle_zone}-{candidate_zone}-delta-{micros}',
                 boundary(oracle_zone, candidate_zone, micros),
                 expected='FAIL' if micros < 0 else 'PASS',
                 reason='default products not sequential' if micros < 0 else '')
    for phase in ('oracle', 'candidate'):
        case(phase + '-failed-execution',
             lambda rs,c,p=phase: rs[p].update(execution_status='FAIL'),
             reason='unclosed/incomplete default product')
        case(phase + '-missing-execution',
             lambda rs,c,p=phase: rs[p].pop('execution_status'),
             reason='unclosed/incomplete default product')

    # C1/C2: causal contradictions of facts emitted by the common Linux collector.
    def field(path, value=None, *, missing=False):
        def mutate(records, contract):
            target = records['candidate']
            for key in path[:-1]:
                target = target[key]
            if missing:
                target.pop(path[-1])
            else:
                target[path[-1]] = value
        return mutate
    case('live-endpoint-contradicts-closure', field(['closure', 'endpoint_connect_errno'], 0),
         reason='endpoint not closed')
    case('closure-errors-with-PASS-label', field(['closure', 'errors'], ['owned child remains']),
         reason='closure errors')
    def public_stdout(records, contract):
        command = records['candidate']['commands'][1]
        command['stdout'] = '999|/unrelated/socket|bank|@7|%9|888\n'
        command['stdout_sha256'] = hashlib.sha256(command['stdout'].encode()).hexdigest()
    case('forged-public-inventory-rehashed', public_stdout, reason='public inventory identity')
    case('wrong-CLI-birth', field(['commands', 3, 'birth', 'pid'], 999), reason='command birth identity')
    for code in (2, 111):
        case('known-closed-endpoint-' + str(code), field(['closure', 'endpoint_connect_errno'], code),
             expected='PASS')
    case('helper-wait-unknown', field(['closure', 'exits', 'helper', 'wait_status'], None), expected='PASS')
    for name, path, values in [
        ('closure-errors', ['closure', 'errors'], [None, {}, '']),
        ('closure-errno', ['closure', 'endpoint_connect_errno'], [None, '2', True, 2.0, 13]),
        ('closure-elapsed', ['closure', 'elapsed_seconds'], [None, '0', False, -1]),
        ('closure-exits', ['closure', 'exits'], [None, []]),
        ('daemon-wait', ['closure', 'exits', 'daemon', 'wait_status'], [None, '0', False, 0.0]),
        ('helper-wait', ['closure', 'exits', 'helper', 'wait_status'], ['0', False, 0.0, -1]),
        ('helper-pidfd', ['closure', 'exits', 'helper', 'pidfd_exit'], [None, False, 1, 'true']),
        ('helper-after', ['closure', 'exits', 'helper', 'after'], [{}]),
        ('CLI-birth', ['commands', 3, 'birth'], [None, [], 'birth']),
        ('CLI-birth-pid', ['commands', 3, 'birth', 'pid'], [None, '999', False, 0]),
        ('CLI-birth-ticks', ['commands', 3, 'birth', 'start_ticks'], [None, 1, '', '-1', 'NaN']),
        ('CLI-birth-parent', ['commands', 3, 'birth', 'ppid'], [None, '1', False, 999]),
        ('CLI-birth-state', ['commands', 3, 'birth', 'state'], [None, 1, '', 'SS']),
        ('endpoint', ['endpoint'], [None, []]),
        ('endpoint-path', ['endpoint', 'path'], ['/unrelated/socket', None, 0]),
        ('endpoint-device', ['endpoint', 'device'], [None, '1', False, -1]),
        ('endpoint-inode', ['endpoint', 'inode'], [None, '1', False, 0]),
        ('public', ['public'], [None, {}, []]),
        ('public-session', ['public', 2], ['wrong', 0]),
        ('public-window', ['public', 3], ['@-1', '@', 0]),
        ('public-pane', ['public', 4], ['%-1', '%', 0]),
    ]:
        # Missing list elements change the shape and are tested by the public case.
        if not isinstance(path[-1], int):
            case('missing-' + name, field(path, missing=True))
        for index, value in enumerate(values):
            case('invalid-' + name + '-' + str(index), field(path, value))
    case('wrong-helper-parent', field(['processes', 'helper', 'identity', 'ppid'], 999),
         reason='process birth/closure')
    case('wrong-helper-exit-birth', field(['closure', 'exits', 'helper', 'birth', 'start_ticks'], '999'),
         reason='process birth/closure')
    case('wrong-retained-daemon-birth-type', field(['processes', 'daemon', 'identity', 'pid'], '999'),
         reason='birth identity')
    case('missing-retained-helper-birth', field(['processes', 'helper', 'identity'], missing=True),
         reason='birth identity')
    def other_legal_ids(records, contract):
        record = records['candidate']
        record['public'][3:5] = ['@7', '%9']
        command = record['commands'][1]
        command['stdout'] = '|'.join(record['public']) + '\n'
        command['stdout_sha256'] = hashlib.sha256(command['stdout'].encode()).hexdigest()
    case('legal-zero-and-nonzero-public-IDs', other_legal_ids, expected='PASS')
    return {'status': 'PASS' if all(x['status']=='PASS' for x in results) else 'FAIL',
            'kind': 'SYNTHETIC_ONLY_NOT_NATIVE_EVIDENCE', 'count': len(results), 'controls': results}


def message_limit_observation(before, after, markers):
    """Validate the measured observer effect before normalizing its time/PID."""
    messages = lambda text, marker: sum(line.endswith(': ' + marker) for line in text.splitlines())
    require(messages(before['stdout'], markers['first']) == 1
            and messages(before['stdout'], markers['last']) == 1, 'two preloaded messages not established')
    text = after['stdout']
    require(messages(text, markers['first']) == messages(text, markers['last']) == 0
            and len(text.splitlines()) == 1, 'consumer did not retain exactly observer command')
    require(type(after['pid']) is int and after['pid'] > 0
            and re.fullmatch(r'(?:[01][0-9]|2[0-3]):[0-5][0-9]: client-'
                             + str(after['pid']) + r' command: show-messages\n', text) is not None,
            'consumer did not retain bound observer command')
    return {'value': '1', 'retained': ['client-$observer command: show-messages']}


def actual_pair(cell_id, pair, contract, *, allow_synthetic=False):
    """Validate raw, pinned executions, not a caller's PASS label."""
    if cell_id.split('|')[0] in contract.get('default_recipe', {}).get('option_ids', []):
        return actual_default_pair(cell_id, pair, contract, allow_synthetic=allow_synthetic)
    require(all(phase in pair for phase in ('oracle', 'candidate')), 'missing actual oracle/candidate execution')
    option_name, operations = bound_recipe(cell_id, contract)
    observations, receipts = [], {}
    for phase in ('oracle', 'candidate'):
        require(phase in pair, 'missing actual ' + phase + ' execution')
        reference = pair[phase]
        authenticate(reference)
        receipt = receipts[phase] = load(reference['path'])
        require(receipt.get('SYNTHETIC_ONLY_NOT_NATIVE_EVIDENCE', False) is allow_synthetic,
                'synthetic receipt cannot receive native credit')
        require(receipt['phase'] == phase and receipt['native_os'] == 'linux', 'execution identity')
        linux_closure(receipt)
        require(receipt['binary'] == contract['native_binaries'][phase], 'binary identity')
        require(receipt['source'] == contract['native_source'], 'source identity')
        require(receipt['recipe_sha256'] == contract['recipe_sha256'], 'recipe identity')
        command_ids = receipt['cells'].get(cell_id)
        require(command_ids is not None and command_ids, 'missing cell execution')
        by_role = bound_commands(receipt, command_ids, option_name, operations, contract)
        require(by_role['value']['stdout'].strip() == '1', 'displayed value not 1')
        observations.append(message_limit_observation(by_role['before'], by_role['after'], receipt['markers']))
    require(command_time(receipts['oracle']['ended']) <= command_time(receipts['candidate']['started']),
            'products not sequential')
    require(observations[0] == observations[1], 'oracle/candidate effect mismatch')


def verify(registry_path, contract_path, output):
    registry, contract = load(registry_path), load(contract_path)
    for artifact in contract['artifacts']:
        authenticate(artifact)
    require(pin(registry_path) == contract['registry'], 'registry export identity')
    inventory_gate(registry, contract)
    manifest = load(output / 'BANK.json')
    require(manifest['contract'] == pin(contract_path), 'contract identity')
    require(manifest['registry'] == pin(registry_path), 'bank registry identity')
    with gzip.open(output / 'cells.txt.gz', 'rt') as stream:
        observed = exact_rows(cell_ids(registry, contract), (line.rstrip('\n') for line in stream))
    require(observed == manifest['population'], 'bank population/digest')
    require(observed['cells'] == population(registry, contract), 'cardinality equation')
    board = load(output / 'SCOREBOARD.json')
    require(board['population'] == observed, 'scoreboard population')
    require(board['default_status'] == 'FAIL_MISSING' and board['skips'] == 0, 'missing evidence converted to skip')
    required = set(board['executions'])
    require(all(key in set(contract['native_cell_ids']) for key in required), 'unfrozen native cell')
    found = set()
    for key in cell_ids(registry, contract):
        if key in required:
            found.add(key)
    require(found == required, 'execution outside matrix')
    failures, passed = {}, []
    for key, pair in board['executions'].items():
        try:
            actual_pair(key, pair, contract)
            passed.append(key)
        except (GateError, KeyError, OSError) as error:
            failures[key] = str(error)
    return {'structure': 'PASS', 'coverage': 'PASS' if len(passed) == observed['cells'] else 'FAIL',
            **observed, 'pass_cells': passed, 'failed_executions': failures,
            'missing_cells': observed['cells'] - len(board['executions']), 'skips': 0,
            'os': {os_name: ('PENDING_NATIVE' if os_name != 'linux' else 'PARTIAL_NATIVE')
                   for os_name in contract['axes']['os']}}


def generate(registry_path, contract_path, output):
    registry, contract = load(registry_path), load(contract_path)
    inventory_gate(registry, contract)
    for artifact in contract['artifacts']:
        authenticate(artifact)
    require(pin(registry_path) == contract['registry'], 'registry identity')
    output.mkdir()
    digest, count = hashlib.sha256(), 0
    with (output / 'cells.txt.gz').open('xb') as raw:
        with gzip.GzipFile(fileobj=raw, mode='wb', compresslevel=1, mtime=0) as stream:
            for cell in cell_ids(registry, contract):
                line = (cell + '\n').encode()
                stream.write(line)
                digest.update(line)
                count += 1
    pop = {'cells': count, 'ids_sha256': digest.hexdigest()}
    require(count == population(registry, contract), 'generated cardinality')
    save(output / 'BANK.json', {'schema': 1, 'registry': pin(registry_path), 'contract': pin(contract_path),
         'population': pop, 'classification': 'compatible',
         'classification_reason': 'Ordinary names match independent tmux inventory; compatibility is an obligation, not an execution claim.',
         'applicability': 'All declared cells retained; questionable route/flag combinations await validated classification, never silently excluded.',
         'adapters': {'linux': 'tests/options_bank_linux.py', 'macos': 'PENDING_NATIVE_ADAPTER',
                      'windows': 'PENDING_NATIVE_ADAPTER'}})
    save(output / 'SCOREBOARD.json', {'schema': 1, 'population': pop, 'default_status': 'FAIL_MISSING',
         'default_reason': 'Required cell has no two-product value/effect evidence.', 'skips': 0, 'executions': {}})
    return pop


def binding_canaries(registry, contract, output, legacy=None):
    """Synthetic counterexamples; fixtures and results can never be native credit."""
    root = output / 'binding-canaries'
    root.mkdir()
    cell = contract['native_cell_ids'][0]
    markers = contract['recipe']['markers']
    option = next(x['name'] for x in registry['options'] if x['id'] == contract['recipe']['option_id'])
    seeds = {}
    for phase in ('oracle', 'candidate'):
        day = '1999-12-31' if phase == 'oracle' else '2000-01-01'
        socket = str(root / (phase + '.nonexistent-socket'))
        # Independent literal operations, not emitted by the binding validator.
        operations = [
            ('preload-first', ['display-message', '-c', '/dev/pts/999', markers['first']], ''),
            ('preload-last', ['display-message', '-c', '/dev/pts/999', markers['last']], ''),
            ('before', ['show-messages'], 'T: ' + markers['first'] + '\nT: ' + markers['last'] + '\n'),
            ('mutation', ['set-option', '-s', option, '1'], ''),
            ('value', ['show-options', '-sv', option], '1\n'),
            ('after', ['show-messages'], '12:34: client-105 command: show-messages\n')]
        commands = []
        for i, (role, args, stdout) in enumerate(operations):
            commands.append({'role': role, 'argv': [contract['native_binaries'][phase]['path'], '-N', '-S', socket, *args],
                             'pid': 100 + i, 'started': f'{day}T00:00:{i*2:02d}+00:00',
                             'ended': f'{day}T00:00:{i*2+1:02d}+00:00', 'exit_code': 0, 'stdout': stdout, 'stderr': ''})
        seeds[phase] = {'SYNTHETIC_ONLY_NOT_NATIVE_EVIDENCE': True, 'phase': phase, 'native_os': 'linux',
                        'binary': contract['native_binaries'][phase], 'source': contract['native_source'],
                        'recipe_sha256': contract['recipe_sha256'], 'execution_status': 'COMPLETE',
                        'closure': {'status': 'PASS', 'errors': [], 'endpoint_connect_errno': 2},
                        'markers': copy.deepcopy(markers), 'public': ['10', socket, 'bank', '@0', '%0', '11'],
                        'attached_client': '12|/dev/pts/999', 'started': f'{day}T00:00:00+00:00',
                        'ended': f'{day}T00:00:12+00:00', 'cells': {cell: list(range(6))}, 'commands': commands}
    results, baseline = [], []
    def observe(name, operation, expected, reason='', destination=None):
        try:
            operation()
            actual, detail = 'PASS', 'returned normally'
        except (ValueError, KeyError, TypeError, IndexError, OSError, StopIteration) as error:
            actual, detail = 'FAIL', str(error)
        row = {'id': name, 'expected_gate': expected, 'observed_gate': actual, 'reason': detail,
               'status': 'PASS' if actual == expected and (not reason or reason in detail) else 'FAIL'}
        (results if destination is None else destination).append(row)
    def pair_case(name, mutate=None, expected='FAIL', reason='', native=False, phase='candidate'):
        documents, local = copy.deepcopy(seeds), copy.deepcopy(contract)
        selected = cell
        if mutate:
            selected = mutate(documents[phase], local) or cell
        folder = root / name
        folder.mkdir()
        pair = {}
        for phase, document in documents.items():
            for command in document['commands']:
                for channel in ('stdout', 'stderr'):
                    command[channel + '_sha256'] = hashlib.sha256(command[channel].encode()).hexdigest()
            path = folder / (phase + '.json')
            save(path, document)
            pair[phase] = pin(path)
        observe(name, lambda: actual_pair(selected, pair, local, allow_synthetic=not native), expected, reason)
        if legacy:
            observe(name, lambda: legacy['actual_pair'](selected, pair, local), expected, destination=baseline)
    def argv(index, tail):
        return lambda r, c: r['commands'][index].update(argv=r['commands'][index]['argv'][:4] + tail)
    def field(index, key, value):
        return lambda r, c: r['commands'][index].update({key: value})
    def wrong_markers(r, c):
        for key, old in list(r['markers'].items()):
            changed = 'UNFROZEN_' + key
            r['markers'][key] = changed
            for command in r['commands']:
                command['argv'] = [x.replace(old, changed) for x in command['argv']]
                command['stdout'] = command['stdout'].replace(old, changed)
    pair_case('valid-synthetic', expected='PASS')

    # A PASS closure must agree with the completion, errors and endpoint facts
    # in either product. Keep each contradiction independent of the others.
    for phase in ('oracle', 'candidate'):
        prefix = phase + '-'
        pair_case(prefix + 'failed-execution-with-PASS-closure',
                  lambda r,c: r.update(execution_status='FAIL'),
                  reason='unclosed/incomplete product', phase=phase)
        for key, reason in [('execution_status', 'unclosed/incomplete product'),
                            ('errors', 'closure errors'),
                            ('endpoint_connect_errno', 'endpoint not closed')]:
            def missing(r, c, key=key):
                (r if key == 'execution_status' else r['closure']).pop(key)
            pair_case(prefix + 'missing-' + key, missing, reason=reason, phase=phase)
        for index, errors in enumerate((['owned child remains'], None, {}, '', False)):
            pair_case(prefix + 'PASS-closure-errors-' + str(index),
                      lambda r,c,v=errors: r['closure'].update(errors=v),
                      reason='closure errors', phase=phase)
        for index, code in enumerate((0, 13, None, '2', True, False, 2.0, 111.0)):
            pair_case(prefix + 'PASS-closure-errno-' + str(index),
                      lambda r,c,v=code: r['closure'].update(endpoint_connect_errno=v),
                      reason='endpoint not closed', phase=phase)
        for code in (2, 111):
            pair_case(prefix + 'known-closed-endpoint-' + str(code),
                      lambda r,c,v=code: r['closure'].update(endpoint_connect_errno=v),
                      expected='PASS', phase=phase)

    def candidate_boundary(delta, zone=datetime.timezone.utc):
        def mutate(receipt, contract):
            offset = (command_time(seeds['oracle']['ended']) + delta
                      - command_time(receipt['started']))
            # Move the complete phase so an invalid pair still has valid local
            # command chronology; only the inter-product boundary can reject it.
            for item in [receipt, *receipt['commands']]:
                for key in ('started', 'ended'):
                    item[key] = (command_time(item[key]) + offset).astimezone(zone).isoformat()
        return mutate
    pair_case('candidate-before-oracle', candidate_boundary(datetime.timedelta(days=-1)),
              reason='products not sequential')
    pair_case('candidate-overlaps-oracle-by-one-microsecond',
              candidate_boundary(datetime.timedelta(microseconds=-1)), reason='products not sequential')
    def oracle_closes_after_candidate_starts(receipt, contract):
        receipt['ended'] = (command_time(seeds['candidate']['started'])
                            + datetime.timedelta(microseconds=1)).isoformat()
    pair_case('oracle-closes-after-candidate-starts', oracle_closes_after_candidate_starts,
              reason='products not sequential', phase='oracle')
    for hours in (0, -12, 14):
        zone = datetime.timezone(datetime.timedelta(hours=hours))
        pair_case('candidate-at-oracle-end-zone-' + str(hours),
                  candidate_boundary(datetime.timedelta(), zone), expected='PASS')
    pair_case('candidate-one-microsecond-after-oracle',
              candidate_boundary(datetime.timedelta(microseconds=1)), expected='PASS')

    pair_case('printed-value-alone-is-not-retention', field(5, 'stdout', ''), reason='consumer did not retain')
    pair_case('observer-command-is-not-payload', field(5, 'stdout', 'T: client command: show-messages\n'), reason='consumer did not retain')
    pair_case('unrelated-observer-PID', field(5, 'stdout', '12:34: client-999 command: show-messages\n'), reason='bound observer')
    pair_case('invalid-observer-time', field(5, 'stdout', '25:00: client-105 command: show-messages\n'), reason='bound observer')
    pair_case('last-preloaded-is-not-observer', field(5, 'stdout', 'T: ' + markers['last'] + '\n'), reason='consumer did not retain')
    pair_case('both-preloaded-is-not-last-only', field(5, 'stdout', 'T: ' + markers['first'] + '\nT: ' + markers['last'] + '\n'), reason='consumer did not retain')
    pair_case('new-append-is-not-preloaded-last', field(5, 'stdout', 'T: ' + markers['append'] + '\n'), reason='consumer did not retain')

    pair_case('synthetic-never-native-credit', reason='synthetic receipt', native=True)
    pair_case('mutation-replaced-by-read-command', argv(3, ['show-options', '-sv', option]), reason='argv binding: mutation')
    pair_case('mutation-after-final-observation', lambda r,c: r.update(commands=[r['commands'][i] for i in [0,1,2,4,5,3]]), reason='role order')
    pair_case('markers-not-frozen-recipe', wrong_markers, reason='markers not frozen')
    pair_case('commands-end-before-start', field(3, 'ended', '2000-01-01T00:00:05+00:00'), reason='end before start')
    pair_case('wrong-scope-flag', argv(3, ['set-option', '-g', option, '1']), reason='argv binding')
    pair_case('wrong-option', argv(3, ['set-option', '-s', 'not-the-option', '1']), reason='argv binding')
    pair_case('wrong-value', argv(3, ['set-option', '-s', option, '2']), reason='argv binding')
    pair_case('wrong-value-flags', argv(4, ['show-options', '-v', option]), reason='argv binding')
    pair_case('wrong-client-target', argv(0, ['display-message', '-c', '/dev/pts/998', markers['first']]), reason='argv binding')
    pair_case('missing-client-target', argv(0, ['display-message', markers['first']]), reason='argv binding')
    pair_case('wrong-socket-target', lambda r,c: r['commands'][3]['argv'].__setitem__(3, '/unrelated/nonexistent-socket'), reason='argv binding')
    pair_case('wrong-executable', lambda r,c: r['commands'][3]['argv'].__setitem__(0, '/nonexistent/other'), reason='argv binding')
    pair_case('extra-command-argument', argv(3, ['set-option', '-s', option, '1', 'extra']), reason='argv binding')
    pair_case('duplicate-role-outside-cell', lambda r,c: r['commands'].append(copy.deepcopy(r['commands'][3])), reason='duplicate required role')
    pair_case('duplicate-cell-index', lambda r,c: r['cells'][cell].__setitem__(4, 3), reason='order/uniqueness')
    pair_case('negative-cell-index', lambda r,c: r['cells'][cell].__setitem__(0, -1), reason='command indices')
    pair_case('boolean-cell-index', lambda r,c: r['cells'][cell].__setitem__(0, False), reason='command indices')
    pair_case('reordered-cell-indices', lambda r,c: r['cells'].__setitem__(cell, [0,1,2,4,5,3]), reason='order/uniqueness')
    pair_case('overlapping-command-times', field(3, 'started', '2000-01-01T00:00:04+00:00'), reason='chronology')
    pair_case('timestamp-without-timezone', field(3, 'started', '2000-01-01T00:00:06'), reason='timezone')
    pair_case('invalid-timestamp', field(3, 'started', 'not-a-date'), reason='timestamp')
    pair_case('receipt-ends-before-commands', lambda r,c: r.update(ended='2000-01-01T00:00:10+00:00'), reason='chronology')
    pair_case('alter-recipe-digest', lambda r,c: r.update(recipe_sha256='0'*64), reason='recipe identity')
    pair_case('wrong-recipe-option', lambda r,c: c['recipe'].update(option_id='wire:Wrong'), reason='unsupported cell/operation binding')
    def wrong_cell(r, c):
        changed = cell.replace('|replace', '|append')
        c['native_cell_ids'] = [changed]
        r['cells'][changed] = r['cells'].pop(cell)
        return changed
    pair_case('cell-does-not-match-recipe', wrong_cell, reason='cell/recipe identity')
    pair_case('declarative-read-as-mutation', lambda r,c: c['receipt_binding']['operations'][3].update(argv=['show-options','-sv','$option']), reason='unsupported cell/operation binding')
    pair_case('declarative-wrong-route', lambda r,c: c['receipt_binding']['cell'].update(route='sdk'), reason='unsupported cell/operation binding')
    pair_case('declarative-wrong-flag', lambda r,c: c['receipt_binding']['operations'][3].update(argv=['set-option','-g','$option','1']), reason='unsupported cell/operation binding')
    structural = [
        ('duplicate-os-axis', lambda r,c: c['axes'].__setitem__('os', ['linux','linux'])),
        ('empty-axis', lambda r,c: c['axes'].__setitem__('os', [])),
        ('duplicate-axis-order', lambda r,c: c['axis_order'].append('os')),
        ('empty-axis-order', lambda r,c: c.update(axis_order=[])),
        ('missing-axis', lambda r,c: c['axes'].pop('os')),
        ('unlisted-axis', lambda r,c: c['axes'].__setitem__('extra', ['x'])),
        ('duplicate-selector', lambda r,c: c['scope_selectors']['server'].append('server-global')),
        ('cross-scope-selector', lambda r,c: c['scope_selectors']['session'].append('server-global')),
        ('empty-selectors', lambda r,c: c.update(scope_selectors={})),
        ('pipe-in-axis', lambda r,c: c['axes'].__setitem__('os', ['linux|macos'])),
        ('newline-in-axis', lambda r,c: c['axes'].__setitem__('os', ['linux\n'])),
        ('pipe-in-name', lambda r,c: r['options'][0].update(name='ambiguous|name')),
        ('pipe-in-option-id', lambda r,c: r['options'][0].update(id='wire:Ambiguous|id')),
        ('duplicate-entry-scope', lambda r,c: r['options'][0]['scopes'].append(r['options'][0]['scopes'][0])),
        ('schema-version', lambda r,c: c.update(schema=2)),
        ('schema-boolean', lambda r,c: c.update(schema=True))]
    for name, mutate in structural:
        r, c = copy.deepcopy(registry), copy.deepcopy(contract)
        mutate(r, c)
        operations = {'inventory': lambda: inventory_gate(r,c), 'first-ID': lambda: next(cell_ids(r,c)),
                      'population': lambda: population(r,c)}
        for key, operation in operations.items():
            observe(name + '/' + key, operation, 'FAIL')
        if legacy:
            observe(name, lambda: (legacy['inventory_gate'](r,c), next(legacy['cell_ids'](r,c)), legacy['population'](r,c)),
                    'FAIL', destination=baseline)
    tiny = copy.deepcopy(registry)
    tiny['options'] = [copy.deepcopy(next(x for x in registry['options'] if x['id'] == contract['recipe']['option_id']))]
    tiny['registry_count'] = 1
    tiny['oracle']['ordinary_names'] = [option]
    minimal = copy.deepcopy(contract)
    minimal['axis_order'], minimal['axes'] = ['os'], {'os':['linux']}
    expected = [contract['recipe']['option_id'] + '|server-global|linux']
    observe('small-unique-ID-positive', lambda: require(list(cell_ids(tiny,minimal)) == expected
            and population(tiny,minimal) == 1, 'small independent ID oracle'), 'PASS')
    minimal['axes']['os'] = ['linux','linux']
    for label, operation in [('inventory',lambda: inventory_gate(tiny,minimal)), ('first-ID',lambda: next(cell_ids(tiny,minimal))),
                             ('population',lambda: population(tiny,minimal))]:
        observe('R1-small-duplicate-axis/' + label, operation, 'FAIL', 'duplicate axis os')
    counterexample = None
    if legacy:
        legacy['inventory_gate'](tiny,minimal)
        rows = list(legacy['cell_ids'](tiny,minimal))
        counterexample = {'rows': rows, 'cardinality': legacy['population'](tiny,minimal), 'unique_ids': len(set(rows))}
    return {'status': 'PASS' if all(x['status']=='PASS' for x in results) else 'FAIL',
            'kind': 'SYNTHETIC_ONLY_NOT_NATIVE_EVIDENCE', 'count': len(results), 'controls': results,
            'baseline_controls': baseline, 'legacy_small_duplicate': counterexample}


def canaries(registry_path, contract_path, output, legacy=None):
    registry, contract = load(registry_path), load(contract_path)
    results = []

    def reject(name, operation):
        try:
            operation()
        except (GateError, FileNotFoundError) as error:
            results.append({'id': name, 'status': 'PASS', 'observed_gate': 'FAIL', 'reason': str(error)})
        else:
            raise GateError('canary accepted: ' + name)

    inventory_gate(registry, contract)
    changed = copy.deepcopy(registry)
    changed['options'][0]['name'] += '-renamed-canary'
    reject('rename-known-option', lambda: inventory_gate(changed, contract))
    expected = list(itertools.islice(cell_ids(registry, contract), 3))
    require(exact_rows(iter(expected), iter(expected))['cells'] == 3, 'exact-set positive control')
    reject('remove-cell', lambda: exact_rows(iter(expected), iter(expected[1:])))
    reject('duplicate-cell', lambda: exact_rows(iter(expected), iter(expected + expected[:1])))
    reject('remove-oracle-call', lambda: actual_pair(contract['native_cell_ids'][0], {}, contract))
    reject('absent-oracle', lambda: authenticate({'path': str(output / 'absent-oracle'), 'bytes': 1, 'sha256': '0'*64}))
    for field, value in [('version', 'tmux wrong'), ('symbol', 'wrong_symbol'), ('source_sha256', '0'*64)]:
        changed = copy.deepcopy(registry)
        changed['oracle'][field] = value
        reject('alter-oracle-' + field, lambda d=changed: inventory_gate(d, contract))
    corrupted = copy.deepcopy(contract['registry'])
    corrupted['sha256'] = '0'*64
    reject('alter-artifact-bytes', lambda: authenticate(corrupted))
    changed = copy.deepcopy(registry)
    changed['options'][0]['id'] = 'wire:Unknown'
    reject('alter-option-id', lambda: inventory_gate(changed, contract))
    focused = binding_canaries(registry, contract, output, legacy=legacy)
    return {'status': focused['status'], 'kind': 'PURE_NEGATIVE_CONTROLS_NOT_NATIVE',
            'count': len(results) + focused['count'], 'controls': results, 'focused': focused}



# Exhaustive ordinary-registry projection from authenticated independent C
# initializers. It never imports the candidate table or a maintained name list.
# Trust anchors identify qualified evidence, never candidate option values.
# Linux compiler output was reproduced in L6-037; the tmux context is the
# independently reviewed, immutable L6-036 native3 oracle observation.
REGISTRY_CONTEXT_V1 = {
    'profile': 'tmux37b-linux-x86_64-gcc13-v1',
    'compiler': (1023032, '1b99826121ae6682a634e5efe09bd3e3df58ce58e0b28f849114ab5b89139c26'),
    'macros': (25277, '84d8b1f5b6d7d7442714349cf8595025721619886bf1ce51f9502839881c7e37'),
    'archive': (788458, '87f2e99e3b685973f2ca002ffd6ed7e51a5744f7009daae5a15670b6d532db96'),
    'oracle_contract': (36671, '5b1fbed619d7a96bffac0f0b9b6c88b67f27f899afc900e49525188de58f2689'),
    'oracle_receipt': (324663, 'b5c26895df765f20286d54df924bc0651b17cad7902f657933b94b279199a775'),
    'oracle_binary': (1377560, '032198096ed82c1473b41c62a2ee83f16d9796a3f6a26f6830637591c24418a7'),
}


def anchored_artifact(reference, anchor, label):
    require(isinstance(reference, dict) and set(reference)=={'path','bytes','sha256'}
            and isinstance(reference['path'], str) and Path(reference['path']).is_absolute()
            and type(reference['bytes']) is int and isinstance(reference['sha256'], str),
            'platform provenance artifact schema: '+label)
    require((reference['bytes'], reference['sha256']) == anchor,
            'platform provenance untrusted evidence: '+label)
    authenticate(reference)


def registry_platform_context(contract):
    provenance = contract.get('registry_platform_binding_provenance')
    fields = {'schema','profile','compiler','macros','archive','oracle_contract','oracle_receipt','bindings'}
    require(isinstance(provenance, dict) and set(provenance)==fields
            and type(provenance['schema']) is int and provenance['schema']==1,
            'platform provenance schema')
    require(provenance['profile']==REGISTRY_CONTEXT_V1['profile'], 'platform provenance profile')
    for label in fields - {'schema','profile','bindings'}:
        anchored_artifact(provenance[label], REGISTRY_CONTEXT_V1[label], label)
    bindings = contract.get('registry_platform_bindings')
    names = {'INT_MAX','UINT_MAX','SHRT_MAX','USHRT_MAX','_PATH_BSHELL','_PATH_VI','TMUX_TERM'}
    require(isinstance(bindings, dict) and set(bindings)==names
            and all(isinstance(x,str) for x in bindings.values()), 'platform binding population/schema')
    rules = provenance['bindings']
    require(isinstance(rules, dict) and set(rules)==names, 'platform provenance binding population')
    macros = dict(re.findall(r'^#define\s+(\w+)[ \t]+([^\n]+)',
                            Path(provenance['macros']['path']).read_text(), re.M))
    def expand(name, seen=()):
        require(name in macros and name not in seen and len(seen)<16, 'platform macro derivation')
        expression = macros[name]
        return re.sub(r'\b[A-Za-z_]\w*\b',
            lambda m:'('+expand(m[0], (*seen,name))+')' if m[0] in macros else m[0], expression)
    def integer(node):
        if isinstance(node,ast.Constant) and type(node.value) is int:
            return node.value
        require(isinstance(node,ast.BinOp) and isinstance(node.op,(ast.Add,ast.Mult)),
                'platform numeric derivation syntax')
        left,right=integer(node.left),integer(node.right)
        return left+right if isinstance(node.op,ast.Add) else left*right
    derived = {}
    for name in names - {'TMUX_TERM'}:
        require(rules[name]=={'kind':'cpp-macro','symbol':name}, 'platform provenance binding rule: '+name)
        if name.startswith('_PATH_'):
            expression = macros[name]
            require(re.fullmatch(r'"(?:\\.|[^"\\])*"',expression) is not None,
                    'platform path macro syntax')
            derived[name] = json.dumps(ast.literal_eval(expression))
        else:
            expression = re.sub(r'(?<=\d)[uUlL]+\b', '', expand(name))
            derived[name] = str(integer(ast.parse(expression,mode='eval').body))
    require(rules['TMUX_TERM']=={'kind':'oracle-build-default','option':'default-terminal'},
            'platform provenance terminal rule')
    archived = load(provenance['oracle_contract']['path'])
    receipt = load(provenance['oracle_receipt']['path'])
    anchored_artifact(receipt['binary'], REGISTRY_CONTEXT_V1['oracle_binary'], 'oracle_binary')
    require(receipt['phase']=='oracle' and receipt['native_os']=='linux'
            and receipt['source']==archived['native_source']
            and receipt['recipe_sha256']==archived['default_recipe_sha256']
            and receipt['execution_status']=='COMPLETE' and receipt['closure']['status']=='PASS'
            and receipt['closure']['errors']==[], 'platform oracle measured context')
    observations = [x for x in receipt['commands'] if x['role']=='registry-value-default-terminal']
    require(len(observations)==1 and observations[0]['exit_code']==0, 'platform terminal observation')
    command=observations[0]
    require(command['argv'][0]==receipt['binary']['path'] and command['argv'][-3:]
            ==['show-options','-sv','default-terminal'] and command['stderr']=='',
            'platform terminal observation command')
    value=command['stdout']
    require(value.endswith('\n') and len(value.splitlines())==1
            and hashlib.sha256(value.encode()).hexdigest()==command['stdout_sha256'],
            'platform terminal observation bytes')
    derived['TMUX_TERM']=json.dumps(value[:-1])
    # A new producer digest cannot authorize another C source or build context.
    source_names={'table':'options-table.c','header':'tmux.h','compat':'compat.h',
                  'defaults_consumer':'options.c','startup':'tmux.c','keys':'key-string.c',
                  'colours':'colour.c','configure':'configure.ac'}
    require(set(contract['registry_oracle_sources'])==set(source_names), 'platform source population')
    with tarfile.open(provenance['archive']['path'], 'r:gz') as archive:
        for label,member in source_names.items():
            reference=contract['registry_oracle_sources'][label]
            authenticate(reference)
            expected=archive.extractfile('tmux-3.7b/'+member).read()
            require(Path(reference['path']).read_bytes()==expected, 'platform source/archive disagreement: '+label)
    require(bindings==derived, 'platform binding contradicts authenticated derivation')
    return derived


def canonical_registry_identities():
    """Read committed checker-side canonical sources, never the tested JSON row."""
    root=Path(__file__).resolve().parent.parent
    # table.rs concatenates the OPTIONS arrays of its section modules in this order.
    sections=['server','session','session_status','window','window_display']
    paths=['crates/rmux-core/src/options/registry/table/'+name+'.rs' for name in sections]
    paths.append('crates/rmux-proto/src/types/options.rs')
    texts=[]
    for path in paths:
        committed=subprocess.check_output(['/usr/bin/git','-C',str(root),'show','HEAD:'+path])
        require((root/path).read_bytes()==committed, 'uncommitted canonical identity source')
        texts.append(committed.decode())
    pattern=re.compile(r'//[^\n]*|/\*.*?\*/|"(?:\\.|[^"\\])*"|[A-Za-z_]\w*|::|[^\s]',re.S)
    def lex(text):
        return [m[0] for m in pattern.finditer(text) if not m[0].startswith(('//','/*'))]
    rows=[]
    for text in texts[:-1]:
        tokens=lex(text);begin=tokens.index('OPTIONS')
        require(tokens[begin:begin+9]==['OPTIONS',':','&','[','OptionMetadata',']','=','&','['],
                'canonical registry declaration')
        row=[];stack=[]
        for token in tokens[begin+9:]:
            if not stack and token==']':
                require(not row, 'canonical registry trailing row');break
            if not stack and token==',':
                require(bool(row),'empty canonical registry row');rows.append(row);row=[];continue
            row.append(token)
            if token in ('(','[','{'):stack.append(token)
            elif token in (')',']','}'):
                require(bool(stack) and stack.pop()=={')':'(',']':'[','}':'{'}[token],
                        'canonical registry delimiters')
        else:
            raise GateError('unterminated canonical registry')
    identities={}
    for row in rows:
        if row[:4]==['option','(','OptionName','::']:
            require(row[5]==',' and row[6].startswith('"') and row[7]==',', 'canonical typed identity')
            wire,name=row[4],ast.literal_eval(row[6])
        else:
            require(row[:10]==['OptionMetadata','{','wire_option',':','None',',','name',':',row[8],',']
                    and row[8].startswith('"'), 'canonical named identity')
            wire,name=None,ast.literal_eval(row[8])
        require(name not in identities, 'duplicate canonical name')
        identities[name]={'id':'wire:'+wire if wire else 'named:'+name,'wire_symbol':wire}
    proto=lex(texts[-1]);begin=next(i for i in range(len(proto)) if proto[i:i+3]==['enum','OptionName','{'])+3
    end=proto.index('}',begin);body=proto[begin:end]
    require(len(body)%2==0 and all(body[i+1]==',' for i in range(0,len(body),2)), 'wire enum syntax')
    variants=body[::2]
    actual=[x['wire_symbol'] for x in identities.values() if x['wire_symbol'] is not None]
    require(len(actual)==len(set(actual)) and set(actual)==set(variants), 'canonical wire domain')
    return identities


def registry_oracle(contract):
    bindings = registry_platform_context(contract)
    sources = contract['registry_oracle_sources']
    for reference in sources.values():
        authenticate(reference)
    source = Path(sources['table']['path']).read_text()
    header = Path(sources['header']['path']).read_text()
    compat = Path(sources['compat']['path']).read_text()
    require(sha(sources['table']['path']) == contract['oracle_identity']['source_sha256'],
            'registry oracle source identity')
    token = re.compile(r"/\*.*?\*/|//[^\n]*|\"(?:\\.|[^\"\\])*\"|'(?:\\.|[^'\\])*'|[A-Za-z_][A-Za-z_0-9]*|[0-9]+|[^\s]", re.S)
    def lex(value):
        return [m.group() for m in token.finditer(value) if not m.group().startswith(('/*', '//'))]
    constants = {}
    flattened = (header + '\n' + compat + '\n' + source).replace('\\\n', '')
    for name, value in re.findall(r'^#define\s+(\w+)[ \t]+([^\n]+)', flattened, re.M):
        constants[name] = lex(value)
    # These are qualified target/build bindings, not per-option expectations.
    for name, value in bindings.items():
        constants[name] = lex(value)
    for body in re.findall(r'enum\s+\w+\s*\{([^{}]*)\}', header, re.S):
        value = -1
        for item in body.split(','):
            match = re.fullmatch(r'\s*([A-Z_][A-Z_0-9]*)\s*(?:=\s*(-?\d+))?\s*', item)
            if not match:
                break
            value = int(match[2]) if match[2] is not None else value + 1
            constants[match[1]] = [str(value)]
    def expand(tokens, depth=0):
        require(depth < 10, 'recursive oracle constant')
        result = []
        for part in tokens:
            result.extend(expand(constants[part], depth+1) if part in constants else [part])
        return result
    def cstring(tokens):
        tokens = expand(tokens)
        require(tokens and all(part.startswith('"') for part in tokens), 'unresolved oracle string: ' + repr(tokens))
        return ''.join(ast.literal_eval(part) for part in tokens)
    def number(tokens):
        value = ''.join(expand(tokens))
        require(re.fullmatch(r'-?[0-9]+', value) is not None, 'unresolved oracle number: ' + value)
        return int(value)
    arrays = {}
    for name, body in re.findall(r'static const char \*(\w+)\[\]\s*=\s*\{(.*?)\};', source, re.S):
        parts, current = [], []
        for part in lex(body):
            if part == ',':
                if current != ['NULL']:
                    parts.append(cstring(current))
                current = []
            else:
                current.append(part)
        require(current in ([], ['NULL']), 'oracle array terminator')
        arrays[name] = parts
    aliases = {}
    alias_body = source.split('options_other_names[] = {', 1)[1].split('};', 1)[0]
    for alias, name in re.findall(r'\{\s*("[^"\n]+")\s*,\s*("[^"\n]+")\s*\}', alias_body):
        aliases.setdefault(ast.literal_eval(name), []).append(ast.literal_eval(alias))
    tokens = lex(source)
    pos = next(i+5 for i in range(len(tokens)) if tokens[i:i+5] == ['options_table','[',']','=','{'])
    entries = []
    while tokens[pos] != '}':
        if tokens[pos] == ',':
            pos += 1
            continue
        if tokens[pos] in ('OPTIONS_TABLE_HOOK','OPTIONS_TABLE_PANE_HOOK','OPTIONS_TABLE_WINDOW_HOOK'):
            pos = tokens.index(')', pos) + 1
            continue
        require(tokens[pos] == '{', 'oracle initializer')
        end = tokens.index('}', pos)
        row, fields, i = tokens[pos+1:end], {}, 0
        while i < len(row):
            if row[i] == ',':
                i += 1
                continue
            require(row[i] == '.' and row[i+2] == '=', 'oracle field')
            key, i = row[i+1], i+3
            begin = i
            while i < len(row) and not (row[i] == ',' and (i+1 == len(row) or row[i+1] == '.')):
                i += 1
            require(key not in fields, 'duplicate oracle field')
            fields[key] = row[begin:i]
        pos = end + 1
        if fields['name'] == ['NULL'] or 'OPTIONS_TABLE_IS_HOOK' in fields.get('flags', []):
            continue
        name, kind = cstring(fields['name']), fields['type'][0].removeprefix('OPTIONS_TABLE_').lower()
        scopes = [scope for scope in ('server','session','window','pane')
                  if 'OPTIONS_TABLE_' + scope.upper() in fields['scope']]
        flags = fields.get('flags', [])
        require(set(flags) <= {'OPTIONS_TABLE_IS_ARRAY','OPTIONS_TABLE_IS_STYLE','|'}, 'unknown oracle flag')
        is_array = 'OPTIONS_TABLE_IS_ARRAY' in flags
        value_type = {'kind': kind}
        if kind == 'choice':
            value_type['choices'] = arrays[fields['choices'][0]]
        if kind == 'number':
            value_type.update(minimum=number(fields['minimum']), maximum=number(fields['maximum']))
        if 'default_arr' in fields:
            default = {'kind':'array', 'values':arrays[fields['default_arr'][0]]}
        elif 'default_str' in fields:
            default = {'kind':'scalar', 'value':cstring(fields['default_str'])}
        else:
            numeric = fields.get('default_num', ['0'])
            if kind == 'key':
                keys = {"'\\177'": 'C-?', "'b'|KEYC_CTRL": 'C-b', 'KEYC_NONE':'None'}
                require(''.join(numeric) in keys, 'unknown oracle key default')
                value = keys[''.join(numeric)]
            else:
                value = number(numeric)
                if kind == 'colour':
                    colours = {-1:'none', 8:'default', **dict(enumerate(['black','red','green','yellow','blue','magenta','cyan','white']))}
                    require(value in colours, 'unknown oracle colour default')
                    value = colours[value]
                elif kind == 'flag':
                    value = 'on' if value else 'off'
                elif kind == 'choice':
                    value = value_type['choices'][value]
                else:
                    value = str(value)
            default = {'kind':'scalar', 'value':value}
        separator = cstring(fields['separator']) if 'separator' in fields else (' ,' if is_array else '')
        values = default.get('values')
        if values is None:
            value = default['value']
            values = [part for part in re.split('['+re.escape(separator)+']', value) if part] if is_array and separator else [value]
        entries.append({'name':name, 'aliases':aliases.get(name, []), 'value_type':value_type,
                        'scopes':scopes, 'show_scopes':scopes, 'global_root':scopes[0].title(),
                        'is_array':is_array, 'is_style':'OPTIONS_TABLE_IS_STYLE' in flags,
                        'separator':separator, 'default':default, 'consumer_values':values,
                        'oracle_fields':fields})
    require(len({entry['name'] for entry in entries}) == len(entries), 'duplicate oracle name')
    require(set(aliases) <= {entry['name'] for entry in entries}, 'unbound oracle alias')
    return {'schema':1, 'source':sources, 'platform_bindings':bindings,
            'target_os':'linux', 'platform_profile':REGISTRY_CONTEXT_V1['profile'],
            'entries':entries, 'count':len(entries), 'scope':'all ordinary non-hook entries'}


def registry_field_check(registry, oracle):
    """All fields and real Rust OptionStore observations, with no missing skips."""
    require(oracle.get('platform_profile')==REGISTRY_CONTEXT_V1['profile']
            and oracle.get('target_os')=='linux' and registry.get('target_os')=='linux',
            'registry target outside authenticated platform context')
    entries, expected = registry['options'], oracle['entries']
    require(len(entries) == registry['registry_count'], 'registry count')
    require(len({x['id'] for x in entries}) == len(entries), 'duplicate registry ID')
    require(len({x['name'] for x in entries}) == len(entries), 'duplicate registry name')
    require({x['name'] for x in entries} == {x['name'] for x in expected}, 'omitted/extra registry name')
    by_name = {entry['name']:entry for entry in expected}
    identities = canonical_registry_identities()
    require(set(identities)==set(by_name), 'canonical/oracle name population')
    failures, rows = [], []
    for entry in entries:
        oracle_entry = by_name[entry['name']]
        checks = {}
        identity = identities[entry['name']]
        checks['id'] = entry['id'] == identity['id']
        checks['wire_symbol'] = entry['wire_symbol'] == identity['wire_symbol']
        for field in ('name','aliases','value_type','scopes','show_scopes','global_root','is_array','separator','default'):
            checks[field] = entry[field] == oracle_entry[field]
        checks['style_flag'] = entry.get('is_style') == oracle_entry['is_style']
        consumer = entry.get('default_consumer', {})
        checks['consumer_values'] = consumer.get('values') == oracle_entry['consumer_values']
        checks['consumer_scope_validation'] = consumer.get('allowed_scopes') == oracle_entry['scopes']
        checks['consumer_show_scopes'] = consumer.get('show_scopes') == oracle_entry['show_scopes']
        checks['consumer_aliases'] = consumer.get('aliases') == oracle_entry['aliases']
        for field, passed in checks.items():
            if not passed:
                failures.append({'id':entry['id'], 'name':entry['name'], 'field':field,
                                 'actual':entry.get(field, consumer.get(field.removeprefix('consumer_'))),
                                 'expected':identity[field] if field in identity else oracle_entry.get(field, oracle_entry.get(field.removeprefix('consumer_')))})
        rows.append({'id':entry['id'],'name':entry['name'],'checks':checks,
                     'status':'PASS' if all(checks.values()) else 'FAIL'})
    return {'status':'PASS' if not failures else 'FAIL','entries':len(entries),
            'passed_entries':sum(x['status']=='PASS' for x in rows), 'failures':failures, 'rows':rows,
            'whole_criterion_closed':False, 'scope':'registry fields and default value consumer only; no downstream effect credit'}



def registry_trust_canaries(registry, contract, output):
    """Alter only untrusted inputs; require a causal provenance/identity rejection."""
    root=output/'registry-trust-canaries';root.mkdir()
    oracle=registry_oracle(contract)
    baseline=registry_field_check(registry,oracle)
    known={(x['name'],x['field']) for x in baseline['failures']}
    rows=[]
    def reject(name, mutate, reason):
        local=copy.deepcopy(contract);mutate(local)
        save(root/(name+'-contract.json'),local)
        try:
            registry_oracle(local)
        except (GateError, OSError, KeyError, TypeError) as error:
            require(reason in str(error), 'wrong provenance canary cause: '+name+': '+str(error))
            rows.append({'name':name,'status':'PASS','rejection':str(error)})
        else:
            raise GateError('provenance canary escaped: '+name)
    reject('missing-provenance',lambda c:c.pop('registry_platform_binding_provenance'),'provenance schema')
    for name,value in [('null',None),('list',[]),('empty',{})]:
        reject('malformed-provenance-'+name,lambda c,v=value:c.update(registry_platform_binding_provenance=v),'provenance schema')
    reject('bool-schema',lambda c:c['registry_platform_binding_provenance'].update(schema=True),'provenance schema')
    reject('unknown-context',lambda c:c['registry_platform_binding_provenance'].update(profile='invented'),'provenance profile')
    reject('omitted-binding-rule',lambda c:c['registry_platform_binding_provenance']['bindings'].pop('_PATH_BSHELL'),'binding population')
    reject('wrong-symbol',lambda c:c['registry_platform_binding_provenance']['bindings']['_PATH_BSHELL'].update(symbol='_PATH_VI'),'binding rule')
    reject('candidate-sentinel-as-oracle',lambda c:c['registry_platform_bindings'].update(_PATH_BSHELL='""'),'contradicts authenticated derivation')
    reject('numeric-override',lambda c:c['registry_platform_bindings'].update(INT_MAX='1'),'contradicts authenticated derivation')
    reject('terminal-override',lambda c:c['registry_platform_bindings'].update(TMUX_TERM='"screen"'),'contradicts authenticated derivation')
    reject('producer-only-digest',lambda c:c['registry_platform_binding_provenance']['macros'].update(sha256='0'*64),'untrusted evidence')
    reject('malformed-artifact',lambda c:c['registry_platform_binding_provenance']['macros'].update(bytes=True),'artifact schema')
    reject('missing-real-artifact',lambda c:c['registry_platform_binding_provenance']['macros'].update(path=str(root/'absent')),'missing artifact')
    def forge_macros(c):
        path=root/'forged-macros.txt'
        text=Path(c['registry_platform_binding_provenance']['macros']['path']).read_text()
        require('#define _PATH_BSHELL "/bin/sh"' in text,'macro forgery fixture')
        path.write_text(text.replace('#define _PATH_BSHELL "/bin/sh"','#define _PATH_BSHELL ""'))
        c['registry_platform_binding_provenance']['macros']=pin(path)
        c['registry_platform_bindings']['_PATH_BSHELL']='""'
    reject('rehashed-forged-macros',forge_macros,'untrusted evidence')
    def forge_receipt(c):
        path=root/'forged-oracle.json'
        record=load(c['registry_platform_binding_provenance']['oracle_receipt']['path'])
        command=next(x for x in record['commands'] if x['role']=='registry-value-default-terminal')
        command['stdout']='screen\n';command['stdout_sha256']=hashlib.sha256(command['stdout'].encode()).hexdigest()
        save(path,record);c['registry_platform_binding_provenance']['oracle_receipt']=pin(path)
        c['registry_platform_bindings']['TMUX_TERM']='"screen"'
    reject('rehashed-forged-oracle',forge_receipt,'untrusted evidence')
    def forge_source(c):
        path=root/'forged-options-table.c'
        text=Path(c['registry_oracle_sources']['table']['path']).read_text()
        path.write_text(text+'\n/* forged producer source */\n')
        c['registry_oracle_sources']['table']=pin(path)
        c['oracle_identity']['source_sha256']=sha(path)
    reject('rehashed-forged-C-source',forge_source,'source/archive disagreement')
    for name,wire in [('coherent-wire-rename','InventedHistoryLimit'),('typed-to-named',None)]:
        local=copy.deepcopy(registry)
        entry=next(x for x in local['options'] if x['name']=='history-limit')
        entry.update(wire_symbol=wire,id='wire:'+wire if wire else 'named:history-limit')
        save(root/(name+'-registry.json'),local)
        result=registry_field_check(local,oracle)
        errors={(x['name'],x['field']) for x in result['failures']}-known
        require({('history-limit','id'),('history-limit','wire_symbol')}<=errors,'coherent ID canary escaped')
        rows.append({'name':name,'status':'PASS','new_failures':sorted(errors)})
    for target in ('macos','windows',None):
        local=copy.deepcopy(registry);local['target_os']=target
        try:
            registry_field_check(local,oracle)
        except GateError as error:
            require('outside authenticated platform context' in str(error),'wrong platform canary cause')
            rows.append({'name':'unqualified-target-'+str(target),'status':'PASS','rejection':str(error)})
        else:
            raise GateError('unqualified target escaped')
    return {'status':'PASS','count':len(rows),'controls':rows,'baseline_raw_failures':baseline['failures'],
            'synthetic_only':True,'values_derived_from_authenticated_evidence':True}


def registry_canaries(registry, oracle):
    """Mutate the actual Rust export; each field mutation must add its own error."""
    baseline = registry_field_check(registry, oracle)
    known = {(x['id'], x['field']) for x in baseline['failures']}
    rows = []
    target = next(i for i, x in enumerate(registry['options'])
                  if x['name'] == 'history-limit')
    mutations = [
        ('wrong-scope', 'scopes', lambda e: e.update(scopes=['server'])),
        ('wrong-show-flag', 'show_scopes', lambda e: e.update(show_scopes=['pane'])),
        ('wrong-default', 'default', lambda e: e.update(default={'kind':'scalar','value':'1234567'})),
        ('wrong-maximum', 'value_type', lambda e: e['value_type'].update(maximum=17)),
        ('wrong-array-flag', 'is_array', lambda e: e.update(is_array=True)),
        ('wrong-style-flag', 'style_flag', lambda e: e.update(is_style=True)),
        ('wrong-root', 'global_root', lambda e: e.update(global_root='Server')),
        ('wrong-consumer-default', 'consumer_values', lambda e: e['default_consumer'].update(values=['1234567'])),
        ('wrong-consumer-scope', 'consumer_scope_validation', lambda e: e['default_consumer'].update(allowed_scopes=['server'])),
        ('wrong-consumer-show-flag', 'consumer_show_scopes', lambda e: e['default_consumer'].update(show_scopes=['pane'])),
        ('wrong-consumer-alias', 'consumer_aliases', lambda e: e['default_consumer'].update(aliases=['invented-alias'])),
    ]
    for name, field, mutate in mutations:
        specimen = copy.deepcopy(registry)
        entry = specimen['options'][target]
        require((entry['id'], field) not in known, 'canary target already fails')
        mutate(entry)
        result = registry_field_check(specimen, oracle)
        errors = {(x['id'], x['field']) for x in result['failures']}
        require((entry['id'], field) in errors - known, 'registry canary undetected: '+name)
        rows.append({'name':name, 'status':'PASS', 'detected_field':field})
    for name in ('duplicate-id', 'omitted-id'):
        specimen = copy.deepcopy(registry)
        if name == 'duplicate-id':
            specimen['options'][1]['id'] = specimen['options'][0]['id']
        else:
            specimen['options'].pop()
            specimen['registry_count'] -= 1
        try:
            registry_field_check(specimen, oracle)
        except GateError as error:
            rows.append({'name':name,'status':'PASS','rejection':str(error)})
        else:
            raise GateError('registry canary undetected: '+name)
    return {'status':'PASS', 'count':len(rows), 'canaries':rows,
            'baseline_status':baseline['status'], 'baseline_failures':baseline['failures'],
            'synthetic_only':True}


REGISTRY_STARTUP_V1 = ('set-option -s exit-empty off\nset-option -g status off\n'
                       'set-option -gw automatic-rename off\n')
REGISTRY_SETUP_VALUES_V1 = {'exit-empty':'off', 'status':'off', 'automatic-rename':'off'}


def registry_plan(contract):
    authenticate(contract['registry'])
    registry = load(contract['registry']['path'])
    inventory_gate(registry, contract)
    recipe = contract['default_recipe']
    require(type(recipe['schema']) is int and recipe['schema'] == 1
            and recipe['kind'] in ('registry-global-default-v1', 'registry-global-default-v2'), 'registry recipe schema')
    require(recipe['startup'] == REGISTRY_STARTUP_V1, 'registry setup overrides')
    context = {k:v for k,v in DEFAULT_CELL_V1.items() if k != 'scope'}
    require(recipe['context'] == context, 'registry context')
    require(recipe['environment'] in ({'SHELL':'/bin/bash'}, {'SHELL':'/bin/sh'}), 'registry environment')
    require(recipe['option_ids'] == [entry['id'] for entry in registry['options']], 'registry recipe population')
    require(hashlib.sha256(json.dumps(recipe, sort_keys=True).encode()).hexdigest()
            == contract['default_recipe_sha256'], 'registry recipe digest')
    require(set(contract['axes']) == set(context) and all(context[k] in contract['axes'][k] for k in context),
            'registry axes')
    roots = {'Server':('-s','server-global'), 'Session':('-g','session-global'), 'Window':('-gw','window-global')}
    plan = [{'role':'registry-inventory-'+root.lower(), 'argv':['show-options',flags]}
            for root,(flags,_) in roots.items()]
    plan.append({'role':'registry-inventory-pane', 'argv':['show-options','-pA']})
    if recipe['kind']=='registry-global-default-v2':
        plan.append({'role':'registry-inventory-window-inherited', 'argv':['show-options','-wA']})
        for scope in ('w','p'):
            plan.append({'role':'empty-array-named-'+scope, 'argv':['show-options','-'+scope+'A','pane-colours']})
            plan.append({'role':'empty-array-value-'+scope, 'argv':['show-options','-'+scope+'Av','pane-colours']})

    cells = {}
    for entry in registry['options']:
        flags, scope = roots[entry['global_root']]
        indices = [list(roots).index(entry['global_root'])]
        for role, suffix in [('named',''), ('value','v')]:
            indices.append(len(plan))
            plan.append({'role':'registry-'+role+'-'+entry['name'], 'argv':['show-options',flags+suffix,entry['name']]})
        for alias in entry['aliases']:
            plan.append({'role':'registry-alias-'+alias, 'argv':['show-options',flags+'v',alias]})
        cell = '|'.join([entry['id'], scope, *(context[k] for k in contract['axis_order'])])
        cells[cell] = {'name':entry['name'], 'indices':indices}
    require(set(cells) == set(contract['native_cell_ids']) and len(cells) == len(contract['native_cell_ids']),
            'registry native cell population')
    require(len(plan) + 3 <= 1024, 'registry command bound')
    return plan, cells


def registry_observations(receipt, contract, cells, phase):
    recipe = contract['default_recipe']
    root = str(Path(receipt['public'][1]).parent)
    expected_environment = {'HOME':root+'/home', 'XDG_CONFIG_HOME':root+'/xdg',
        'PATH':'/usr/bin:/bin','SHELL':recipe['environment']['SHELL'],'TERM':'xterm','LANG':'C.UTF-8',
        'RMUX_DISABLE_TMUX_SHIM':'1','RMUX_DISABLE_TMUX_FALLBACK':'1'}
    require(receipt.get('environment') == expected_environment, 'registry recorded environment')
    oracle = {entry['name']:entry for entry in registry_oracle(contract)['entries']}
    commands, observations = receipt['commands'], {}
    if recipe['kind']=='registry-global-default-v2':
        for scope, role in [('w','registry-inventory-window-inherited'),('p','registry-inventory-pane')]:
            named=[x['stdout'] for x in commands if x['role']=='empty-array-named-'+scope]
            value=[x['stdout'] for x in commands if x['role']=='empty-array-value-'+scope]
            full=[x['stdout'] for x in commands if x['role']==role]
            require(named==['pane-colours\n'], 'named empty array marker')
            require(value==[''], 'named empty array value')
            require(len(full)==1 and [x for x in full[0].splitlines() if x.startswith('pane-colours')]
                    ==['pane-colours*'], 'full empty array inheritance marker')
    for cell, spec in cells.items():
        indices = [index+2 for index in spec['indices']]
        require(receipt['cells'][cell] == indices and all(type(x) is int for x in receipt['cells'][cell]),
                'registry observation indices')
        name = spec['name']
        inventory = commands[indices[0]]['stdout'].splitlines()
        rows = [line for line in inventory if line == name or line.startswith(name+' ') or line.startswith(name+'[')]
        named, value = [commands[index]['stdout'] for index in indices[1:]]
        require(named == ''.join(row+'\n' for row in rows) and bool(rows), 'registry named/raw disagreement: '+name)
        if phase == 'oracle':
            expected = oracle[name]['consumer_values']
            if name in REGISTRY_SETUP_VALUES_V1:
                expected = [REGISTRY_SETUP_VALUES_V1[name]]
            elif name == 'default-shell':
                expected = [recipe['environment']['SHELL']]
            require(value == ''.join(item+'\n' for item in expected), 'registry oracle default/context: '+name)
        for alias in oracle[name]['aliases']:
            observed = [command['stdout'] for command in commands if command['role']=='registry-alias-'+alias]
            require(observed == [value], 'registry alias observation: '+alias)
        # Setup reads are retained for auditing, but cannot become default cells.
        if name not in REGISTRY_SETUP_VALUES_V1:
            observations[cell] = {'name':name, 'named':named, 'value':value}
    return observations


def registry_receipt_canaries(contract, pair, output):
    """Exercise the new profile using altered, explicitly synthetic real receipts."""
    root = output / 'registry-receipt-canaries'
    root.mkdir()
    _, cells = registry_plan(contract)
    selected = next(cell for cell, spec in cells.items() if spec['name']=='history-limit')
    originals = {phase:load(ref['path']) for phase,ref in pair.items()}
    rows = []
    def case(name, mutate=None, reason='', expected='FAIL', cell=selected):
        records, local = copy.deepcopy(originals), copy.deepcopy(contract)
        for record in records.values():
            record['SYNTHETIC_ONLY_NOT_NATIVE_EVIDENCE'] = True
        if mutate:
            mutate(records, local)
        folder = root / name
        folder.mkdir()
        refs = {}
        for phase, record in records.items():
            save(folder/(phase+'.json'), record)
            refs[phase] = pin(folder/(phase+'.json'))
        try:
            actual_default_pair(cell, refs, local, allow_synthetic=True)
            actual, detail = 'PASS', 'returned normally'
        except (ValueError, KeyError, TypeError, IndexError, OSError) as error:
            actual, detail = 'FAIL', str(error)
        rows.append({'name':name,'expected_gate':expected,'observed_gate':actual,'reason':detail,
                     'status':'PASS' if actual==expected and (not reason or reason in detail) else 'FAIL'})
    def output_change(phase, role, text):
        def change(records, contract):
            command = next(x for x in records[phase]['commands'] if x['role']==role)
            command['stdout'] = text
            command['stdout_sha256'] = hashlib.sha256(text.encode()).hexdigest()
        return change
    case('actual-complete-population', expected='PASS')
    case('recorded-environment-contradiction', lambda rs,c:rs['candidate']['environment'].update(SHELL='/bin/false'),
         reason='recorded environment')
    case('unsupported-environment', lambda rs,c:c['default_recipe'].update(environment={'SHELL':'/bin/false'}),
         reason='registry environment')
    case('omitted-recipe-id', lambda rs,c:c['default_recipe']['option_ids'].pop(), reason='recipe population')
    case('duplicate-recipe-id', lambda rs,c:c['default_recipe']['option_ids'].append(c['default_recipe']['option_ids'][0]),
         reason='recipe population')
    case('omitted-native-cell', lambda rs,c:c['native_cell_ids'].pop(), reason='native cell population')
    case('extra-native-cell', lambda rs,c:c['native_cell_ids'].append('invented'), reason='native cell population')
    case('missing-recorded-cell', lambda rs,c:rs['candidate']['cells'].pop(selected), reason='missing/extra cell')
    case('oracle-default-rehashed', output_change('oracle','registry-value-history-limit','1234567\n'),
         reason='oracle default/context')
    case('candidate-default-rehashed', output_change('candidate','registry-value-history-limit','1234567\n'),
         reason='oracle/candidate mismatch')
    case('candidate-named-rehashed', output_change('candidate','registry-named-history-limit','history-limit 1234567\n'),
         reason='named/raw disagreement')
    alias = next(x['role'] for x in originals['candidate']['commands'] if x['role'].startswith('registry-alias-'))
    case('candidate-alias-rehashed', output_change('candidate',alias,'1234567\n'), reason='alias observation')
    case('changed-setup', lambda rs,c:c['default_recipe'].update(startup=c['default_recipe']['startup']+'set -g mouse on\n'),
         reason='setup overrides')
    if contract['default_recipe']['kind']=='registry-global-default-v2':
        for phase in ('oracle','candidate'):
            for scope in ('w','p'):
                case(phase+'-named-empty-array-star-'+scope,
                     output_change(phase,'empty-array-named-'+scope,'pane-colours*\n'),
                     reason='named empty array marker')
        def missing_full_marker(records, local):
            command=next(x for x in records['candidate']['commands'] if x['role']=='registry-inventory-pane')
            command['stdout']=command['stdout'].replace('pane-colours*\n','pane-colours\n')
            command['stdout_sha256']=hashlib.sha256(command['stdout'].encode()).hexdigest()
        case('full-empty-array-star-removed',missing_full_marker,reason='full empty array inheritance marker')
    for name in REGISTRY_SETUP_VALUES_V1:
        cell = next(cell for cell, spec in cells.items() if spec['name']==name)
        case('setup-not-default-'+name, cell=cell, reason='unexecuted default cell')
    return {'status':'PASS' if all(x['status']=='PASS' for x in rows) else 'FAIL',
            'count':len(rows),'controls':rows,'synthetic_only':True}


def self_test():
    """Replay the existing binding/inventory controls with synthetic artifacts."""
    import tempfile
    contract = load(Path(__file__).with_name('options_bank_contract.json'))
    identities = canonical_registry_identities()
    registry = {'schema': 1, 'registry_count': len(identities), 'target_os': 'linux',
                'registry_symbol': 'registry::table::OPTIONS',
                'oracle': {**contract['oracle_identity'], 'ordinary_names': list(identities)},
                'options': [{'name': name, **identity,
                             'scopes': ['server' if name == 'message-limit' else 'session'],
                             'default': 'fixture', 'value_type': 'Number',
                             'consumer': {'status': 'FAIL_MISSING'}} for name, identity in identities.items()]}
    audit = Path(__file__).resolve().parent.parent / 'target' / 'tmp'
    audit.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=audit) as folder:
        root = Path(folder)
        registry_path = root / 'registry.json'
        save(registry_path, registry)
        contract.update(registry=pin(registry_path), artifacts=[], native_source='fixture-head',
                        native_binaries={phase: pin(__file__) for phase in ('oracle', 'candidate')},
                        recipe_sha256=hashlib.sha256(json.dumps(contract['recipe'], sort_keys=True).encode()).hexdigest())
        contract_path = root / 'contract.json'
        save(contract_path, contract)
        result = canaries(registry_path, contract_path, root)
        require(result['status'] == 'PASS', 'self-test controls failed: ' + json.dumps(result['focused']['controls']))
        return {key: result[key] for key in ('status', 'kind', 'count')}


if __name__ == '__main__' and sys.argv[1:] == ['--self-test']:
    print(json.dumps(self_test(), sort_keys=True))
elif __name__ == '__main__':
    require(len(sys.argv) == 5, __doc__)
    mode, registry_path, contract_path, out = sys.argv[1:]
    output = Path(out)
    if mode == 'generate':
        result = generate(registry_path, contract_path, output)
    elif mode == 'check':
        result = verify(registry_path, contract_path, output)
    elif mode == 'canaries':
        result = canaries(registry_path, contract_path, output)
    elif mode == 'registry-oracle':
        result = registry_oracle(load(contract_path))
        save(output, result)
    elif mode == 'registry-check':
        result = registry_field_check(load(registry_path), registry_oracle(load(contract_path)))
        save(output, result)
    else:
        raise GateError('unknown mode')
    print(json.dumps(result, sort_keys=True))
    if (mode == 'check' and result['coverage'] != 'PASS') or (mode == 'registry-check' and result['status'] != 'PASS'):
        sys.exit(1)
