#!/usr/bin/env python3
"""Git source inventory v1. See README.md for counting and checking contracts."""
import argparse
import collections
import functools
import json
import re
from pathlib import Path, PurePosixPath as P
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import toml_reader
from model import (CLASSES, MAINTAINED, canonical, conjunction, digest, git, expand_declarations,
                   normalize, possible, read_tree, simplify, validate_declarations)

TOOL = Path(__file__).resolve().parent
VERSION = 'rmux-source-inventory/1'
# Exact syn rendering of include!(concat!(env!("CARGO_MANIFEST_DIR"), "/path")).
# An escaped literal, another variable or extra tokens stay unsupported.
MANIFEST_INCLUDE = re.compile(r'concat ! \(env ! \("CARGO_MANIFEST_DIR"\) , "(/[^"\\]*)"\)')


def parser_scan(binary, files, policy):
    identity = json.loads(subprocess.check_output([str(binary), '--identity']))
    for name in ('src/main.rs', 'src/cfg.rs', 'src/template.rs', 'Cargo.toml', 'Cargo.lock'):
        if identity.get(name) != (TOOL / name).read_text():
            raise ValueError(f'parser binary does not match tool inputs: {name}')
    requests, paths = [], []
    for path, entry in files.items():
        if path.endswith('.rs'):
            paths.append(path)
            grammar = policy['rust_roots'].get(path, {}).get('grammar', 'file')
            requests.append(canonical({'source': entry['data'].decode(), 'grammar': grammar}))
    result = subprocess.run([str(binary)], input=('\n'.join(requests) + '\n').encode(),
                            stdout=subprocess.PIPE, check=True)
    rows = [json.loads(line) for line in result.stdout.splitlines()]
    if len(rows) != len(paths):
        raise ValueError('parser response population mismatch')
    return dict(zip(paths, rows))


def targets(files, policy):
    roots, manifests = [], {}
    for path, entry in files.items():
        if P(path).name != 'Cargo.toml':
            continue
        manifest = toml_reader.loads(entry['data'].decode())
        if 'package' not in manifest:
            continue
        base = str(P(path).parent)
        package = manifest['package']
        role = policy['crate_roles'].get(path, 'production')
        if role not in CLASSES:
            raise ValueError(f'invalid crate role: {path}')
        manifests[base] = {'name': package['name'], 'manifest': path,
                           'dependencies': {k: v for k, v in manifest.items()
                                            if k.endswith('dependencies') or k == 'target'},
                           'features': manifest.get('features', {})}

        def add(relative, kind, required=False):
            target = normalize(base + '/' + relative)
            if target in files:
                roots.append((target, kind, True, path))
            elif required:
                raise ValueError(f'missing declared Cargo target: {path}: {target}')
        lib = manifest.get('lib', {})
        add(lib.get('path', 'src/lib.rs'), role, 'path' in lib)
        for kind in ('bin', 'example', 'test', 'bench'):
            target_role = 'tests' if kind in ('test', 'bench') else role
            plural = 'benches' if kind == 'bench' else kind + 's'
            directory = 'src/bin' if kind == 'bin' else plural
            explicit = manifest.get(kind, [])
            explicit_names = {target.get('name') for target in explicit}
            for target in explicit:
                relative = target.get('path')
                if relative is None and target.get('name'):
                    name = target['name']
                    candidates = [f'{directory}/{name}.rs', f'{directory}/{name}/main.rs']
                    if kind == 'bin' and name == package['name']:
                        candidates.append('src/main.rs')
                    found = [p for p in candidates if normalize(base + '/' + p) in files]
                    # Cargo's edition-2015 bin fallback also resolves ambiguity.
                    if (len(found) != 1 and kind == 'bin'
                            and package.get('edition', '2015') == '2015'
                            and normalize(base + '/src/main.rs') in files):
                        found = ['src/main.rs']
                    if len(found) != 1:
                        raise ValueError(f'unresolved/ambiguous Cargo {kind} target: '
                                         f'{path}: {name}: {candidates}')
                    relative = found[0]
                if relative:
                    add(relative, target_role, True)
            if package.get('auto' + plural, True):
                if kind == 'bin' and package['name'] not in explicit_names:
                    add('src/main.rs', role)
                prefix = normalize(base + '/' + directory) + '/'
                for candidate in files:
                    if not candidate.startswith(prefix):
                        continue
                    rel = candidate[len(prefix):]
                    if ('/' not in rel and rel.endswith('.rs')) or (
                            rel.count('/') == 1 and rel.endswith('/main.rs')):
                        # Explicit declarations own their names, even with a custom path.
                        name = P(rel).stem if '/' not in rel else rel.split('/')[0]
                        if name not in explicit_names:
                            add(directory + '/' + rel, target_role)
        build = package.get('build', 'build.rs')
        if build:
            add(build, 'tooling')
    for path, row in policy['rust_roots'].items():
        if path in files:
            if not row.get('evidence') or row['role'] not in CLASSES:
                raise ValueError(f'invalid Rust root declaration: {path}')
            roots.append((path, row['role'], True, row['evidence']))
    return sorted(set(roots)), manifests


def at(parsed, offset):
    # Preserve enclosing conditions before inner contradictions simplify to false.
    return [r['condition'] for r in sorted(parsed['regions'],
            key=lambda r: (r['start'], -r['end'])) if r['start'] <= offset < r['end']]


def descend(ancestry, conditions):
    chain = list(ancestry)
    for condition in conditions:
        combined = conjunction(chain[-1], condition)
        if combined != chain[-1]:
            chain.append(combined)
    return chain


def module_edges(path, parsed, files, policy, module_dir, crate_dir=None):
    parent = str(P(path).parent)
    default = module_dir
    edges = []
    for m in parsed['modules']:
        inline = m['inline']
        # An explicit outer inline #[path] is relative to the source file,
        # without the implicit non-mod.rs stem. Nested modules extend that base.
        base = parent if inline and inline[0]['path'] is not None else default
        for ancestor in inline:
            segment = ancestor['path'] if ancestor['path'] is not None else ancestor['name']
            base = normalize(base + '/' + segment)
        if m['path']:
            # #[path] in an inline module starts at that module's directory.
            base = base if inline else parent
            candidates = [normalize(base + '/' + m['path'])]
        else:
            base = normalize(base + '/' + m['name'])
            candidates = [base + '.rs', base + '/mod.rs']
        existing = [p for p in candidates if p in files]
        if len(existing) != 1:
            raise ValueError(f'{path}: unresolved/ambiguous module {candidates}')
        child_dir = str(P(existing[0]).parent) if m['path'] or existing[0].endswith('/mod.rs') else str(P(existing[0]).with_suffix(''))
        edges.append((existing[0], at(parsed, m['offset']), 'mod', child_dir))
    for inc in parsed['includes']:
        tokens = inc['tokens']
        key = path + '::' + tokens
        manifest_relative = MANIFEST_INCLUDE.fullmatch(tokens)
        try:
            relative = json.loads(tokens)
        except json.JSONDecodeError:
            if manifest_relative and crate_dir is not None:
                target = normalize(crate_dir + manifest_relative.group(1))
                if target not in files:
                    raise ValueError(f'{path}: missing include {target}')
                edges.append((target, at(parsed, inc['offset']), 'include', module_dir))
                continue
            declaration = policy['includes'].get(key)
            if not declaration or not declaration.get('evidence'):
                raise ValueError(f'{path}: unsupported include {tokens}')
            if declaration.get('generated'):
                continue
            target = declaration['path']
        else:
            target = normalize(parent + '/' + relative)
        if target not in files:
            raise ValueError(f'{path}: missing include {target}')
        edges.append((target, at(parsed, inc['offset']), 'include', module_dir))
    return edges


def contexts(files, parsed, roots, policy, errors):
    contexts = collections.defaultdict(dict)
    for path, result in parsed.items():
        errors.extend(f'{path}: {e}' for e in result.get('errors', []))
    # CARGO_MANIFEST_DIR names the crate being compiled, never the included file's
    # own location. A standalone root declares no crate, so its includes stay refused.
    pending = [(p, r, [c], user, str(P(p).parent),
                str(P(user).parent) if P(user).name == 'Cargo.toml' and user in files else None)
               for p, r, c, user in roots]
    edges, visited = {}, set()
    while pending:
        path, role, ancestry, consumer, directory, crate_dir = pending.pop()
        key = canonical([role, ancestry, directory])
        if key in contexts[path]:
            contexts[path][key]['consumers'].add(consumer)
        else:
            if len(contexts[path]) > 128:
                raise ValueError(f'too many module contexts (cycle?): {path}')
            contexts[path][key] = {'role': role, 'condition': ancestry[-1], 'ancestry': ancestry,
                                   'module_dir': directory, 'consumers': {consumer}}
        # A second crate reaching this context can resolve different manifest includes.
        if (path, key, crate_dir) in visited:
            continue
        visited.add((path, key, crate_dir))
        if parsed[path].get('errors'):
            continue
        edge_key = (path, directory, crate_dir)
        if edge_key not in edges:
            try:
                edges[edge_key] = module_edges(path, parsed[path], files, policy, directory, crate_dir)
            except ValueError as e:
                errors.append(str(e))
                edges[edge_key] = []
        for target, conditional, kind, child_dir in edges[edge_key]:
            combined = descend(ancestry, conditional)
            pending.append((target, role, combined, path + ':' + kind, child_dir, crate_dir))
    return contexts


@functools.lru_cache(maxsize=16384)
def region_role(serialized):
    contexts = json.loads(serialized)
    live, enclosing = set(), set()
    for role, ancestry in contexts:
        for depth, condition in enumerate(reversed(ancestry)):
            if possible(condition, False):
                resolved = role
            elif possible(condition, True):
                resolved = 'tests' if role in ('production', 'tooling') else role
            else:
                continue
            (live if depth == 0 else enclosing).add(resolved)
            break
        else:
            enclosing.add(role)
    # Dead consumers cannot outrank live ones. Entirely dead source retains its
    # nearest reachable enclosing class, including across module/include edges.
    roles = live or enclosing
    for shared in ('production', 'tooling', 'specifications'):
        if shared in roles and roles <= {'production', 'tooling', 'specifications', 'tests'}:
            return shared
    if len(roles) != 1:
        raise ValueError(f'incompatible source consumers: {sorted(roles)}')
    return roles.pop()


def rust_lines(entry, parsed, consumers):
    data = entry['data']
    regions = parsed['regions']
    boundaries = sorted({0, len(data)} | {r[k] for r in regions for k in ('start', 'end')})
    segments = []
    for start, end in zip(boundaries, boundaries[1:]):
        if start == end:
            continue
        ancestry = [[c['role'], descend(c.get('ancestry', [c['condition']]), at(parsed, start))]
                    for c in consumers.values()]
        conditions = [[role, chain[-1]] for role, chain in ancestry]
        role = region_role(canonical(ancestry))
        segments.append({'start_byte': start, 'end_byte': end, 'class': role,
                         'conditions': conditions})
    # A physical line is credited once. Ignore horizontal whitespace when a line
    # has substantive bytes in multiple spans; production wins mixed lines.
    lines, mixed, offset, index = [], [], 0, 0
    physical = data.split(b'\n')
    if physical and physical[-1] == b'':
        physical.pop()
    for number, line in enumerate(physical, 1):
        end = offset + len(line)
        roles, fallback = set(), set()
        while index < len(segments) and segments[index]['end_byte'] <= offset:
            index += 1
        j = index
        while j < len(segments) and segments[j]['start_byte'] <= end:
            segment = segments[j]
            a, b = max(offset, segment['start_byte']), min(end, segment['end_byte'])
            if a < b or (not line and a == b and a < segment['end_byte']):
                fallback.add(segment['class'])
                if data[a:b].strip():
                    roles.add(segment['class'])
            j += 1
        roles = roles or fallback
        if not roles:
            roles = {region_role(canonical([[c['role'], c.get('ancestry', [c['condition']])]
                                           for c in consumers.values()]))}
        if len(roles) > 1:
            mixed.append(number)
        role = 'production' if 'production' in roles else sorted(roles)[0]
        lines.append(role)
        offset = end + 1
    runs = []
    for number, role in enumerate(lines, 1):
        if runs and runs[-1]['class'] == role:
            runs[-1]['end'] = number
        else:
            runs.append({'start': number, 'end': number, 'class': role})
    return collections.Counter(lines), runs, mixed, segments


def semantic_attributes(parsed, consumers):
    entries = []
    for attribute in parsed['semantic_attributes']:
        condition = simplify(attribute['condition'])
        if not possible(condition, False) and not possible(condition, True):
            continue
        ancestry = [[c['role'], descend(c.get('ancestry', [c['condition']]),
                                       at(parsed, attribute['start']))]
                    for c in consumers.values()]
        entries.append(dict(attribute, condition=condition,
                            classification=region_role(canonical(ancestry))))
    return entries


def scan(repo, revision, binary, policy, policy_bytes, *, parsed=None):
    files = read_tree(repo, revision)
    errors = []
    policy = expand_declarations(policy)
    assigned = validate_declarations(policy, files)
    parsed = parser_scan(binary, files, policy) if parsed is None else parsed
    if set(parsed) != {p for p in files if p.endswith('.rs')}:
        raise ValueError('parser response population mismatch')
    roots, manifests = targets(files, policy)
    consumers = contexts(files, parsed, roots, policy, errors)
    rows, crates = [], {}
    ratchets = {'path_literal': [], 'allow_dead_code_literal': [], 'markdown': []}
    for path, entry in files.items():
        data = entry['data']
        crate = next((base for base in sorted(manifests, key=len, reverse=True)
                      if base == '.' or path.startswith(base + '/')), None)
        owner = manifests[crate]['name'] + '@' + crate if crate is not None else 'repository'
        row = {'path': path, 'blob': entry['blob'], 'bytes': len(data), 'crate': owner}
        if path.endswith('.rs'):
            if path not in consumers or parsed[path].get('errors'):
                errors.append(f'unclassified Rust: {path}')
                row.update(counts={}, regions=[], physical_lines=data.count(b'\n') + bool(data and not data.endswith(b'\n')))
            else:
                counts, regions, mixed, conditional = rust_lines(entry, parsed[path], consumers[path])
                row.update(counts=dict(counts), regions=regions, mixed_lines=mixed,
                           conditional_regions=conditional,
                           physical_lines=sum(counts.values()),
                           consumers=[dict(c, consumers=sorted(c['consumers']))
                                      for _, c in sorted(consumers[path].items())],
                           functions=parsed[path]['functions'], opaque_macros=parsed[path]['macros'],
                           semantic_attributes=semantic_attributes(parsed[path], consumers[path]))
        else:
            role = assigned.get(path)
            if role is None:
                errors.append(f'unclassified file: {path}')
            try:
                data.decode('utf-8')
                textual = b'\0' not in data
            except UnicodeDecodeError:
                textual = False
            count = data.count(b'\n') + bool(data and not data.endswith(b'\n')) if textual else 0
            row.update(counts={role: count} if role else {}, physical_lines=count,
                       binary=not textual, regions=[{'start': 1, 'end': count, 'class': role}] if count else [])
            if path in policy['documents']:
                row['document'] = policy['documents'][path]
            if path in policy['provenance']:
                row['provenance'] = policy['provenance'][path]
        rows.append(row)
        if path.startswith(('src/', 'crates/')):
            for key, literal in [('path_literal', b'#[path ='), ('allow_dead_code_literal', b'#[allow(dead_code)]')]:
                for number, line in enumerate(data.split(b'\n'), 1):
                    n = line.count(literal)
                    if n:
                        ratchets[key].append({'path': path, 'line': number, 'occurrences': n})
        if path.endswith('.md'):
            ratchets['markdown'].append(path)
        summary = crates.setdefault(owner, {'files': 0, 'physical_lines': 0, 'counts': collections.Counter(),
                                            'production_ge_600': [], 'functions_ge_80': []})
        summary['files'] += 1
        summary['physical_lines'] += row['physical_lines']
        summary['counts'].update(row['counts'])
        for function in row.get('functions', []):
            if function['end'] - function['start'] + 1 >= 80:
                summary['functions_ge_80'].append(dict(function, path=path))
        if row['counts'].get('production', 0) >= 600:
            summary['production_ge_600'].append(path)
    totals = collections.Counter()
    for row in rows:
        totals.update(row['counts'])
    inputs = {p.name: digest(p.read_bytes()) for p in sorted(TOOL.glob('*.py'))}
    for p in sorted((TOOL / 'src').glob('*.rs')):
        inputs['src/' + p.name] = digest(p.read_bytes())
    for name in ('Cargo.toml', 'Cargo.lock'):
        inputs[name] = digest((TOOL / name).read_bytes())
    return {'schema': VERSION, 'revision': revision, 'tree': git(repo, 'rev-parse', revision + '^{tree}').decode().strip(),
            'tool_inputs_sha256': inputs, 'declarations_sha256': digest(policy_bytes),
            'parser': 'syn 2.0.118 / proc-macro2 1.0.106; UTF-8 byte spans',
            'unit': 'LF physical lines including final unterminated line; production wins mixed lines',
            'files': rows, 'crates': crates, 'totals': totals, 'ratchets': ratchets,
            'semantic_attributes_schema': 1,
            'manifests': manifests, 'errors': sorted(set(errors)),
            'reduction_credit': 0, 'G': None,
            'nonblocking': ['function spans >=80 LF lines, descriptive only; includes tests', 'opaque macro source tokens; no expansion',
                            'complexity and duplication not measured', 'dependencies are manifest declarations, not resolved target graphs']}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, default=Path('.'))
    parser.add_argument('--revision', required=True)
    parser.add_argument('--parser', type=Path, required=True)
    parser.add_argument('--declarations', type=Path, default=TOOL / 'declarations.json')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--check-size', action='store_true', help='enforce <=599 production lines per file')
    args = parser.parse_args()
    try:
        policy_bytes = args.declarations.read_bytes()
        report = scan(args.repo, args.revision, args.parser, json.loads(policy_bytes), policy_bytes)
        args.output.write_text(canonical(report) + '\n')
        violations = sum(len(c['production_ge_600']) for c in report['crates'].values())
        print(canonical({'revision': args.revision, 'files': len(report['files']), 'totals': report['totals'],
                         'errors': len(report['errors']), 'production_ge_600': violations}))
        if report['errors']:
            print('\n'.join(report['errors']), file=sys.stderr)
        return int(bool(report['errors']) or (args.check_size and violations > 0))
    except (ValueError, KeyError, UnicodeError, subprocess.CalledProcessError) as e:
        args.output.write_text(canonical({'schema': VERSION, 'errors': [str(e)]}) + '\n')
        print(str(e), file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
