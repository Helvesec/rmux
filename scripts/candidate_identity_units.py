"""Compiler-unit provenance, locked registry sources, and transitive link inputs."""
import hashlib
import importlib.util
import json
import os
from os import link
from pathlib import Path
import secrets
import subprocess
import tarfile

GRAPH = b'RMUX_BUILD_GRAPH_V1:'
LOCKED_GRAPH = b'RMUX_BUILD_GRAPH_V2:'


def graph_marker(graph, sha):
    prefix = LOCKED_GRAPH if 'lock_context_version' in graph else GRAPH
    return prefix + sha.encode() + b':END'


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def values(args, option):
    result = []
    for i, arg in enumerate(args):
        if arg == option and i + 1 < len(args):
            result.append(args[i + 1])
        elif arg.startswith(option + '='):
            result.append(arg[len(option) + 1:])
    return result


def manifest_for(code, env):
    if env.get('CARGO_MANIFEST_DIR'):
        return Path(env['CARGO_MANIFEST_DIR']).resolve() / 'Cargo.toml'
    return next((parent / 'Cargo.toml' for parent in code.parents
                 if (parent / 'Cargo.toml').is_file()), None)


def source_proof(g, code, source, head, target, env):
    manifest = manifest_for(code, env)
    if code.is_relative_to(target) and env.get('RMUX_IDENTITY_DOC_SESSION'):
        ticket = g.generation(source, head, target)
        if ticket.get('kind') == 'compile-consumer':
            g.support('candidate_identity_consumers').input_path(g, ticket, code)
        return dict(kind=ticket.get('kind', 'doctest'), generation=ticket,
                    input=str(code), input_sha256=g.file_hash(code), manifest=None)
    if code.is_relative_to(source):
        g.tracked_input(code, source, head)
        if manifest is None:
            raise g.IdentityError('workspace unit manifest missing')
        unit = g.support('candidate_identity_manifest').binding(g, source, head, target)
        # The bootstrap wrapper is a tracked harness input, not a tool source.
        wrapper = (unit is not None and code == source / 'scripts/candidate_identity_windows.rs'
                   and g.host_os() == 'windows'
                   and manifest == source / 'Cargo.toml')
        if unit and not wrapper and (str(manifest) != unit['manifest']
                                     or not code.is_relative_to(Path(unit['manifest']).parent)):
            raise g.IdentityError('compiler source belongs to another compilation unit')
        g.tracked_input(manifest, source, head)
        return dict(kind='identity-wrapper' if wrapper else 'workspace',
                    input=str(code), input_sha256=g.file_hash(code),
                    manifest=str(manifest), manifest_sha256=g.file_hash(manifest))
    if manifest is None or not code.is_relative_to(manifest.parent):
        raise g.IdentityError('dependency source has no qualified manifest')
    package = g.support('toml_reader').loads(manifest.read_text())['package']
    pinned = g.support('candidate_identity_manifest').registry_pin(g, package, source, head, target)
    root = manifest.parent
    if root.parent.parent.name != 'src' or root.parent.parent.parent.name != 'registry':
        raise g.IdentityError('external source is not a qualified Cargo registry extraction')
    archive = root.parent.parent.parent / 'cache' / root.parent.name / (root.name + '.crate')
    checksum = pinned['checksum']
    if not archive.is_file() or g.file_hash(archive) != checksum:
        raise g.IdentityError('registry archive differs from the locked checksum')
    input_seen = False
    alternate = root.with_name(root.name.swapcase())
    folded = (alternate.is_dir() and not alternate.is_symlink()
              and alternate.samefile(root))
    extracted = {}
    with tarfile.open(archive) as packed:
        for entry in packed:
            if not entry.isfile():
                continue
            parts = Path(entry.name).parts
            if not parts or parts[0] != root.name or '..' in parts:
                raise g.IdentityError('registry archive path is not canonical')
            path = root.joinpath(*parts[1:])
            if path.is_symlink() or not path.is_file():
                raise g.IdentityError('registry extraction differs from locked archive')
            expected = hashlib.sha256(packed.extractfile(entry).read()).hexdigest()
            # Cargo extracts in archive order. On an insensitive volume, a later
            # Readme.md replaces README.md; sensitive volumes retain both files.
            key = tuple(part.casefold() for part in parts) if folded else parts
            extracted[key] = (path, expected)
    for path, expected in extracted.values():
        if g.file_hash(path) != expected:
            raise g.IdentityError('registry input differs from locked archive')
        input_seen |= path.samefile(code)
    if not input_seen:
        raise g.IdentityError('compiler input is absent from the locked archive')
    return dict(kind='registry', input=str(code), input_sha256=g.file_hash(code),
                manifest=str(manifest), manifest_sha256=g.file_hash(manifest),
                archive=str(archive), archive_sha256=checksum,
                package=pinned['name'], version=pinned['version'], registry_source=pinned['source'])


def output_paths(args):
    explicit = values(args, '-o')
    if explicit:
        return [Path(name).resolve() for name in explicit]
    dirs = values(args, '--out-dir')
    crates = values(args, '--crate-name')
    if len(dirs) != 1 or len(crates) != 1:
        return []
    extra = next((arg.removeprefix('-C').split('=', 1)[1] for arg in args
                  if arg.removeprefix('-C').startswith('extra-filename=')), '')
    name = crates[0] + extra
    directory = Path(dirs[0]).resolve()
    return [directory / (prefix + name + suffix) for prefix, suffix in
            [('', '.exe' if os.name == 'nt' else ''), ('lib', '.rlib'), ('lib', '.rmeta'),
             ('lib', '.so'), ('lib', '.dylib'), ('', '.dll'), ('', '.lib')]]


def receipt_key(path, sha):
    # Clippy emits empty metadata for several executable units. Those equal
    # bytes cannot identify a producer; retain their absolute output path too.
    if sha == hashlib.sha256(b'').hexdigest():
        return digest([str(path), sha])
    return sha


def compiler_receipt_key(path, sha):
    # Equal library bytes can come from independent nested Cargo outputs.
    # This index selects that output's producer; historical graphs still pin
    # the immutable receipt, and retained objects keep their existing index.
    return digest(['compiler-output-v1', str(path), sha])


def preserve(g, directory, record):
    """Publish a completed observation once, without replacing its history."""
    directory.mkdir(exist_ok=True)
    raw = (json.dumps(record, sort_keys=True) + '\n').encode()
    receipt = directory / (digest(record) + '.json')
    temporary = directory / (str(os.getpid()) + '-' + secrets.token_hex(8) + '.tmp')
    temporary.write_bytes(raw)
    try:
        try:
            link(temporary, receipt)
        except FileExistsError:
            if receipt.read_bytes() != raw:
                raise g.IdentityError('immutable compiler observation changed')
    finally:
        temporary.unlink()
    return receipt


def verify(g, path, source, head, target, seen=None, expected=None, receipt=None, hashes=None):
    owner = os.name != 'nt' and getattr(g, '_qualification', None) is None
    if owner:
        spec = importlib.util.spec_from_file_location(
            'candidate_identity_qualification',
            Path(__file__).with_name('candidate_identity_qualification.py'))
        qualification = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(qualification)
        g = qualification.scope(g)
    path = Path(path).resolve(strict=True)
    if not path.is_relative_to(target):
        raise g.IdentityError('linked unit is outside the candidate target')
    hashes = {} if hashes is None else hashes
    file_hash = g.file_hash
    sha = (file_hash(path) if os.name == 'nt'
           else g._qualification.file_hash(path, keep=owner))
    if expected is not None and sha != expected:
        raise g.IdentityError('linked unit bytes changed')
    seen = {} if seen is None else seen
    directory = target / 'identity-units'
    selected = Path(receipt) if receipt else directory / (receipt_key(path, sha) + '.json')
    compiled = directory / (compiler_receipt_key(path, sha) + '.json')
    if os.name != 'nt' and receipt is None and compiled.exists():
        selected = compiled
    try:
        read = getattr(g, 'read_bytes', Path.read_bytes)
        raw = read(selected)
        record = json.loads(raw)
        if selected == compiled and (record['kind'] != 'library'
                                     or record['compiler_output'] != str(path)):
            raise g.IdentityError('compiler observation belongs to another output')
        if (os.name != 'nt' and record['kind'] == 'library' and 'compiler_output' in record
                and str(path) not in (record['path'], record['compiler_output'])):
            raise g.IdentityError('compiler observation belongs to another output')
        immutable = directory / (digest(record) + '.json')
        historical = read(immutable)
        if (selected.parent != directory or (receipt and selected != immutable)
                or historical != raw):
            raise g.IdentityError('compiler observation history differs')
        key = str(immutable)
        if key in seen:
            return seen[key]
        receipt = immutable
        graph = record['graph']
        manifests = g.support('candidate_identity_manifest')
        unit = manifests.graph_binding(g, graph, source, head, target)
        lock = Path(unit['lock']) if unit else source / 'Cargo.lock'
        if (record['sha256'] != sha or graph['head'] != head or graph['source'] != str(source)
                or digest(graph) != record['graph_sha256']
                or graph.get('compilation_unit') != unit
                or graph['lock_sha256'] != file_hash(lock)
                or file_hash(record['path']) != sha
                or file_hash(graph['compiler']) != graph['compiler_sha256']):
            raise g.IdentityError('compiler unit provenance differs')
        if record['kind'] in ('bin', 'rust-test', 'build-script'):
            marker = graph_marker(graph, record['graph_sha256'])
            data = read(path)
            if marker not in data:
                raise g.IdentityError('executable transitive graph is not embedded')
        if os.name != 'nt':
            g._qualification.release(path)
        # The compiler consumes the current alias, but a dependency graph
        # binds the witnessed immutable bytes, so later rebuilds preserve it.
        qualified_path = record['path'] if record['kind'] == 'library' else str(path)
        result = dict(path=qualified_path, sha256=sha, receipt_sha256=hashlib.sha256(raw).hexdigest(),
                      graph_sha256=record['graph_sha256'], receipt=str(receipt), kind=record['kind'])
        if 'lock_contexts' in graph:
            result['lock_contexts'] = graph['lock_contexts']
        seen[key] = result
        dependencies = []
        for dep in graph['dependencies']:
            if dep.get('kind') == 'toolchain':
                if file_hash(dep['path']) != dep['sha256']:
                    raise g.IdentityError('compiler-supplied library changed')
                continue
            actual = verify(g, dep['path'], source, head, target, seen, dep['sha256'], dep['receipt'], hashes)
            if actual['receipt_sha256'] != dep['receipt_sha256']:
                raise g.IdentityError('transitive compiler receipt changed')
            dependencies.append(actual)
        generated = graph.get('generation')
        if generated:
            generated = g.support('candidate_identity_buildscripts').verify_generation(
                g, generated, source, head, target)
        if graph.get('consumer'):
            g.support('candidate_identity_consumers').validate(
                g, graph['consumer'], source, head, target)
        if ('lock_contexts' in graph and graph['lock_contexts'] != manifests.closure(
                g, graph['input'], dependencies, generated, source, head, target)):
            raise g.IdentityError('compiler transitive lock closure differs')
        if 'lock_contexts' not in graph:
            # V1 means one workspace. A V2 child cannot widen that old contract.
            producers = dependencies + ([generated['producer']]
                                         if generated and 'producer' in generated else [])
            for producer in producers:
                if ('lock_contexts' in producer and
                        (unit or manifests.root_binding(g, source, head)) not in producer['lock_contexts']):
                    raise g.IdentityError('legacy compiler dependencies cross workspace locks')
        if owner:
            g._qualification.validate()
        return result
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise g.IdentityError('linked unit has no matching qualified compiler output') from error


def before(g, args, source, head, target, env=None):
    env = os.environ if env is None else env
    code = next((Path(arg).resolve() for arg in args[1:]
                 if not arg.startswith('-') and arg.endswith('.rs')), None)
    if code is None:
        return None
    compiler = Path(args[0]).resolve(strict=True)
    expected = Path(env.get('RMUX_IDENTITY_RUSTC', env.get('RUSTC', ''))).resolve()
    if compiler not in (expected, expected.with_name('clippy-driver.exe' if os.name == 'nt' else 'clippy-driver')):
        raise g.IdentityError('unqualified unit compiler')
    manifests = g.support('candidate_identity_manifest')
    proof = source_proof(g, code, source, head, target, env)
    outputs = output_paths(args)
    if not outputs or any(not path.is_relative_to(target) for path in outputs):
        raise g.IdentityError('compiler unit output is not private')
    for emitted in values(args, '--emit'):
        for part in emitted.split(','):
            if (part.startswith('dep-info=')
                    and not Path(part.split('=', 1)[1]).resolve().is_relative_to(target)):
                raise g.IdentityError('compiler dep-info output is not private')
    deps = []
    seen = {}
    for external in values(args, '--extern'):
        if '=' not in external:
            if external != 'proc_macro' or values(args, '--sysroot'):
                raise g.IdentityError('linked dependency needs an explicit qualified path')
            directory = Path(subprocess.check_output([str(expected), '--print', 'target-libdir'],
                                                      text=True, **g.jobserver()).strip())
            libraries = list(directory.glob('libproc_macro-*.rlib'))
            if len(libraries) != 1:
                raise g.IdentityError('compiler proc_macro library is not uniquely resolved')
            library = libraries[0].resolve(strict=True)
            # Rust 1.96 splits standard-library metadata and link objects. Both
            # exact paths must be supplied; either file alone is insufficient.
            metadata = library.with_suffix('.rmeta')
            if metadata.is_file():
                libraries.append(metadata)
            for library in libraries:
                deps.append(dict(kind='toolchain', path=str(library), sha256=g.file_hash(library)))
            for index, arg in enumerate(args):
                if arg == '--extern=proc_macro':
                    args[index] += '=' + str(libraries[0])
                elif arg == '--extern' and args[index + 1:index + 2] == ['proc_macro']:
                    args[index + 1] += '=' + str(libraries[0])
            for library in libraries[1:]:
                args.extend(['--extern', 'proc_macro=' + str(library)])
            continue
        deps.append(verify(g, external.split('=', 1)[1], source, head, target, seen))
    build = values(args, '--crate-name') == ['build_script_build']
    generation = None
    if env.get('OUT_DIR') and not build:
        builds = g.support('candidate_identity_buildscripts')
        generation = (builds.probe_parent(g, args, outputs, env, source, head, target)
                      if env.get('RMUX_IDENTITY_BUILD_SESSION')
                      else builds.verify_output(g, env, source, head, target))
    contexts = manifests.closure(g, proof, deps, generation, source, head, target)
    binding = contexts[0] if contexts[0]['manifest'] != str(source / 'Cargo.toml') else None
    lock = Path(contexts[0]['lock'])
    graph = dict(head=head, source=str(source), input=proof,
                 lock_sha256=g.file_hash(lock), compiler=str(compiler),
                 compiler_sha256=g.file_hash(compiler), arguments=args,
                 dependencies=deps, generation=generation,
                 lock_context_version=1, lock_contexts=contexts,
                 manifest_scope='standalone' if manifests.binding(g, source, head, target) else 'common')
    if binding:
        graph['compilation_unit'] = binding
    if env.get('RMUX_IDENTITY_DOC_SESSION'):
        ticket = g.generation(source, head, target)
        if ticket.get('kind') == 'compile-consumer':
            graph['consumer'] = ticket
    crate_types = values(args, '--crate-type')
    kind = 'build-script' if build else 'rust-test' if '--test' in args else 'bin' if 'bin' in crate_types else 'library'
    return dict(graph=graph, graph_sha256=digest(graph), kind=kind, outputs=outputs,
                depinfo=target / 'identity-depinfo' / (digest(graph) + '.d'),
                stamps={str(path):g.stamp(path.stat()) for path in outputs if path.is_file()})


def graph_object(g, unit, source, head, target, compiler):
    directory = target / 'identity-graphs'
    directory.mkdir(exist_ok=True)
    sha = unit['graph_sha256']
    code = directory / (sha + '.rs')
    obj = directory / (sha + '.o')
    marker = graph_marker(unit['graph'], sha).decode()
    code.write_text('#![no_std]\n#[used]\n#[no_mangle]\n'
                    f'pub static RMUX_BUILD_GRAPH_V1: [u8; {len(marker)}] = *b"{marker}";\n')
    subprocess.run([str(compiler), '--crate-name', 'rmux_build_graph', '--crate-type', 'lib',
                    '--emit=obj', str(code), '-o', str(obj)], check=True, **g.jobserver())
    keep = {'macos':'-Wl,-u,_RMUX_BUILD_GRAPH_V1', 'linux':'-Wl,-u,RMUX_BUILD_GRAPH_V1',
            'windows':'/INCLUDE:RMUX_BUILD_GRAPH_V1'}[g.host_os()]
    return ['-C', 'link-arg=' + str(obj), '-C', 'link-arg=' + keep]



def with_depinfo(args, unit):
    path = unit['depinfo']
    path.parent.mkdir(exist_ok=True)
    emitted = values(args, '--emit')
    depinfo = [part for value in emitted for part in value.split(',')
               if part == 'dep-info' or part.startswith('dep-info=')]
    # rustc has one destination per emit kind: another dep-info= redirects
    # Cargo's .d rather than adding an output. Observe its original destination.
    if depinfo:
        selected = depinfo[-1]
        if '=' in selected:
            observed = Path(selected.split('=', 1)[1]).resolve()
        elif values(args, '-o'):
            output_types = {part.split('=', 1)[0]: '=' in part
                            for value in emitted for part in value.split(',')}
            observed = Path(values(args, '-o')[-1]).resolve()
            # rustc adapts -o only when several emit kinds lack named paths.
            if sum(not named for named in output_types.values()) > 1:
                observed = observed.with_suffix('.d')
        else:
            extra = next((arg.removeprefix('-C').split('=', 1)[1] for arg in args
                          if arg.removeprefix('-C').startswith('extra-filename=')), '')
            observed = Path(values(args, '--out-dir')[-1]).resolve() / (
                values(args, '--crate-name')[-1] + extra + '.d')
        unit['observed_depinfo'] = observed
        return args
    if not emitted:
        return [*args, '--emit=link,dep-info=' + str(path)]
    result = list(args)
    for i, arg in enumerate(result):
        if arg.startswith('--emit='):
            result[i] = arg + ',dep-info=' + str(path)
        elif arg == '--emit':
            result[i + 1] += ',dep-info=' + str(path)
    return result


def check_depinfo(g, unit, source, head, target):
    if 'observed_depinfo' in unit:
        observed = unit['observed_depinfo'].resolve(strict=True)
        if not observed.is_relative_to(target):
            raise g.IdentityError('compiler dep-info output is not private')
        unit['depinfo'].write_bytes(observed.read_bytes())
    proof = unit['graph']['input']
    generated = unit['graph']['generation']
    if generated:
        generated = g.support('candidate_identity_buildscripts').verify_generation(
            g, generated, source, head, target)
    generated_record = json.loads(Path(generated['receipt']).read_text()) if generated else None
    archive_members = None
    if proof['kind'] == 'registry':
        with tarfile.open(proof['archive']) as archive:
            archive_members = {Path(entry.name).relative_to(Path(entry.name).parts[0]).as_posix():
                hashlib.sha256(archive.extractfile(entry).read()).hexdigest()
                for entry in archive if entry.isfile()}
    paths = []
    for line in unit['depinfo'].read_text().splitlines():
        if line and not line.startswith('#') and line.endswith(':'):
            value = line[:-1].replace('\\ ', ' ').replace('\\#', '#').replace('$$', '$')
            paths.append(Path(value).resolve(strict=True))
    if Path(proof['input']) not in paths:
        raise g.IdentityError('compiler did not expose its complete source dependency list')
    for path in paths:
        if path.is_relative_to(target):
            if proof['kind'] == 'compile-consumer':
                g.support('candidate_identity_consumers').input_path(g, proof['generation'], path)
                continue
            if proof['kind'] == 'doctest' and path == Path(proof['input']):
                continue
            if proof['kind'] == 'final-sink':
                ticket = proof['generation']
                if (path.parent == Path(ticket['directory'])
                        and ticket['files'].get(path.name) == g.file_hash(path)):
                    continue
            if generated_record and path.is_relative_to(Path(generated_record['directory'])):
                relative = path.relative_to(Path(generated_record['directory'])).as_posix()
                if generated_record['files'].get(relative) == g.file_hash(path):
                    continue
            raise g.IdentityError('compiler read an unqualified generated input')
        if path.is_relative_to(source):
            binding = unit['graph'].get('compilation_unit')
            if binding and not path.is_relative_to(Path(binding['manifest']).parent):
                if (proof['kind'] != 'identity-wrapper' or path != Path(proof['input'])
                        or path != source / 'scripts/candidate_identity_windows.rs'):
                    raise g.IdentityError('compiler read a source outside its standalone unit')
            g.tracked_input(path, source, head)
        elif archive_members is not None and path.is_relative_to(Path(proof['manifest']).parent):
            relative = path.relative_to(Path(proof['manifest']).parent).as_posix()
            if archive_members.get(relative) != g.file_hash(path):
                raise g.IdentityError('compiler read an input absent from its locked archive')
        else:
            raise g.IdentityError('compiler read a source outside its qualified package')


def after(g, unit, source, head, target):
    if unit is None:
        return
    check_depinfo(g, unit, source, head, target)
    directory = target / 'identity-units'
    directory.mkdir(exist_ok=True)
    for path in unit['outputs']:
        if not path.is_file() or unit['stamps'].get(str(path)) == g.stamp(path.stat()):
            continue
        sha = g.file_hash(path)
        original = path
        if unit['kind'] == 'build-script':
            # Keep the original compiler path usable on Windows: attribution
            # must not append another long basename to an already bounded path.
            name = sha[:16] + '.payload.exe' if os.name == 'nt' else original.name + '.' + sha[:16] + '.payload'
            path = original.with_name(name)
            os.replace(original, path)
        else:
            path = g.support('candidate_identity_retention').retain(g, original, target, sha)
        record = dict(path=str(path), sha256=sha, compiler_output=str(original),
                      kind=unit['kind'], graph=unit['graph'], graph_sha256=unit['graph_sha256'])
        immutable = preserve(g, directory, record)
        temporary = directory / (str(os.getpid()) + '.json')
        temporary.write_bytes(immutable.read_bytes())
        os.replace(temporary, directory / (receipt_key(path, sha) + '.json'))
        if unit['kind'] == 'library' and receipt_key(original, sha) != receipt_key(path, sha):
            # Empty metadata shares its bytes, but each compiler alias still
            # needs its own current producer; parent graphs pin the receipt.
            temporary.write_bytes(immutable.read_bytes())
            os.replace(temporary, directory / (receipt_key(original, sha) + '.json'))
        if unit['kind'] == 'library' and os.name != 'nt':
            temporary.write_bytes(immutable.read_bytes())
            os.replace(temporary, directory / (compiler_receipt_key(original, sha) + '.json'))
        if unit['kind'] == 'build-script':
            g.support('candidate_identity_buildscripts').install(g, original, path, source, head, target)


def compile_observed(g, args, unit, source, head, target, final):
    # Cargo starts dependent units as soon as rustc emits its metadata notice.
    # Publish those notices only after the completed output has been qualified.
    directory = target / 'identity-compiler-logs'
    directory.mkdir(exist_ok=True)
    import secrets
    log = directory / (str(os.getpid()) + '-' + secrets.token_hex(8) + '.log')
    qualified = False
    before = g.executable_outputs(args) if final else {}
    with log.open('w+b') as diagnostics:
        try:
            status = subprocess.call(args, stderr=diagnostics, **g.jobserver())
            if unit.get('graph', {}).get('consumer'):
                log.with_suffix('.json').write_text(json.dumps(dict(
                    argv=args, graph=unit['graph'], exit=status,
                    completed_output=status == 0), sort_keys=True) + '\n')
            if status == 0:
                g.source_identity(source, head)
                after(g, unit, source, head, target)
                if final:
                    g.compiler_receipts(args, before, source, head, target)
                qualified = True
            return status
        finally:
            diagnostics.seek(0)
            for line in diagnostics:
                if not qualified:
                    try:
                        if json.loads(line).get('$message_type') == 'artifact':
                            continue
                    except (ValueError, AttributeError):
                        pass
                g.sys.stderr.buffer.write(line)
            g.sys.stderr.buffer.flush()
