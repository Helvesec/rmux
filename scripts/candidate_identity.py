#!/usr/bin/env python3
"""Bind native builds/tests (`with -- command ...`) and launches (`run binary ...`)
to a clean HEAD, private target and embedded commit bytes. Standalone locked
tools use `with-manifest MANIFEST -- command ...`; Cargo's runner checks tests."""
from __future__ import annotations

import hashlib
from functools import cache, partial
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import secrets
from types import SimpleNamespace

# Ignored directory under the candidate source that holds private targets.
PRIVATE_ROOT = '.rmux-audit'
MARKER = re.compile(rb'RMUX_BUILD_IDENTITY_V1:([0-9a-f]{40}):([0-9a-f]{64}):(macos|linux|windows):END')


class IdentityError(RuntimeError):
    pass


@cache
def support(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def host_os():
    try:
        return {'darwin': 'macos', 'linux': 'linux', 'win32': 'windows'}[sys.platform]
    except KeyError as error:
        raise IdentityError('unsupported native identity platform') from error


def git(source, *args):
    program = (shutil.which('git') if os.name == 'nt' else
               support('candidate_identity_git').program(SimpleNamespace(**globals())))
    if not program:
        raise IdentityError('git unavailable')
    return subprocess.check_output([program, '-C', str(source), *args], text=True,
                                   stderr=subprocess.DEVNULL).strip()


def source_identity(source, head):
    source = Path(source).resolve(strict=True)
    if not re.fullmatch('[0-9a-f]{40}', head) or git(source, 'rev-parse', 'HEAD') != head:
        raise IdentityError('candidate HEAD differs from pinned commit')
    outputs = [':(exclude).rmux-audit/candidate-target']
    if host_os() == 'windows':
        outputs.append(':(exclude)' + target_directory(source, head).relative_to(source).as_posix())
    if git(source, 'status', '--porcelain', '--untracked-files=normal', '--', '.', *outputs):
        raise IdentityError('candidate source is not clean')
    return source, hashlib.sha256(os.fsencode(str(source))).hexdigest()


def target_directory(source, head, command=None):
    if not re.fullmatch('[0-9a-f]{40}', head):
        raise IdentityError('full candidate commit required')
    if host_os() == 'windows':
        return support('candidate_identity_paths').windows_directory(
            SimpleNamespace(**globals()), source, head, command)
    root = Path(source).resolve() / PRIVATE_ROOT / 'candidate-target' / head / host_os()
    if command is None:
        return root
    if not re.fullmatch('[0-9a-f]{32}', command):
        raise IdentityError('qualified command identity required')
    return root / command


def target_identity(source, head, target):
    # The HEAD/OS root remains readable for historical predicates and fixtures.
    # Executing contexts require a command identity and never publish there.
    root = target_directory(source, head)
    target = Path(target).absolute()
    if target != root:
        if target.parent != root:
            raise IdentityError('shared or foreign identity root')
        if target_directory(source, head, target.name) != target:
            raise IdentityError('noncanonical command identity root')
    if target.resolve() != target:
        raise IdentityError('redirected candidate identity root')
    return target


def stamp(info):
    return (info.st_dev, info.st_ino, info.st_mode, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def verify(binary, source, head, target, _qualification=None):
    """Pure embedded-marker predicate; launches additionally require verify_built."""
    source, source_id = source_identity(source, head)
    target = target_identity(source, head, target)
    binary = Path(binary)
    if not binary.is_absolute():
        raise IdentityError('absolute executable path required')
    path = binary.resolve(strict=True)
    if not path.is_relative_to(target) or not path.is_file():
        raise IdentityError('executable outside candidate target')
    before = path.stat()
    support('candidate_identity_cache').observe(SimpleNamespace(**globals()), binary)
    data = (read_bytes(path) if _qualification is None
            else _qualification.read_bytes(path))
    if stamp(path.stat()) != stamp(before) or binary.resolve(strict=True) != path:
        raise IdentityError('executable changed during verification')
    identities = set(MARKER.findall(data))
    expected = (head.encode(), source_id.encode(), host_os().encode())
    if identities != {expected}:
        raise IdentityError('embedded commit/source/OS absent, ambiguous or different')
    return {'path': str(path),
            'sha256': (hashlib.sha256(data).hexdigest() if _qualification is None
                       else _qualification.file_hash(path)),
            'commit': head, 'source': str(source), 'os': host_os(), 'target': str(target)}


def before_scenario(binaries, source, head, target, scenario):
    records = [verify_built(binary, source, head, target) for binary in binaries]
    if not records:
        raise IdentityError('no executable selected')
    return scenario()


def context():
    try:
        source = Path(os.environ['RMUX_IDENTITY_SOURCE']).resolve()
        head = os.environ['RMUX_IDENTITY_HEAD']
        declared = Path(os.environ['RMUX_IDENTITY_TARGET']).absolute()
        root = target_directory(source, head, declared.name)
        active = Path(os.environ['CARGO_TARGET_DIR']).absolute()
        if (declared != root or target_identity(source, head, root) != root
                or active.resolve() != active or not active.is_relative_to(root)):
            raise IdentityError('shared or foreign target context')
        if host_os() == 'windows' and active.parts[len(root.parts) - 1] != root.name:
            raise IdentityError('noncanonical command identity context')
        return source, head, root
    except KeyError as error:
        raise IdentityError('candidate identity environment missing') from error


def file_hash(path):
    return hashlib.sha256(read_bytes(path)).hexdigest()


def read_bytes(path):
    return support('candidate_identity_cache').read_bytes(SimpleNamespace(**globals()), path)


def tracked_input(path, source, head):
    try:
        support('candidate_identity_cache').observe(SimpleNamespace(**globals()), path)
        path = Path(path).resolve(strict=True)
    except OSError as error:
        raise IdentityError('pinned build input missing') from error
    if not path.is_relative_to(source):
        raise IdentityError('build input outside pinned source tree')
    relative = path.relative_to(source).as_posix()
    try:
        expected = git(source, 'rev-parse', head + ':' + relative)
        actual = git(source, 'hash-object', '--path=' + relative, str(path))
    except subprocess.CalledProcessError as error:
        raise IdentityError('build input is not tracked by pinned HEAD') from error
    if actual != expected:
        raise IdentityError('build input differs from pinned tree')
    return expected


def generation(source, head, target, token=None):
    token = os.environ.get('RMUX_IDENTITY_DOC_SESSION', '') if token is None else token
    if not re.fullmatch('[0-9a-f]{64}', token):
        raise IdentityError('generated input has no active qualified generator')
    ticket = json.loads((target / 'identity-generators' / (token + '.json')).read_text())
    origin = Path(ticket['origin'])
    if (not ticket['active'] or ticket['commit'] != head or ticket['source'] != str(source)
            or tracked_input(origin, source, head) != ticket['blob']
            or file_hash(origin) != ticket['origin_sha256']
            or file_hash(ticket['generator']) != ticket['generator_sha256']):
        raise IdentityError('generated input origin differs from pinned tree')
    if ticket.get('kind') == 'final-sink':
        if host_os() != 'windows':
            raise IdentityError('final-sink generation requires Windows')
        support('candidate_identity_windows').validate_generation(
            SimpleNamespace(**globals()), ticket, source, head, target)
    if ticket.get('kind') == 'compile-consumer':
        support('candidate_identity_consumers').validate(
            SimpleNamespace(**globals()), ticket, source, head, target,
            require_lock=ticket['phase'] == 'frozen')
    return ticket


def jobserver():
    if os.name == 'nt':
        return {}
    match = re.search(r'--jobserver-(?:auth|fds)=(\d+),(\d+)', os.environ.get('CARGO_MAKEFLAGS', ''))
    if not match:
        return {}
    descriptors = tuple(int(value) for value in match.groups())
    for descriptor in descriptors:
        os.fstat(descriptor)
    return {'pass_fds': descriptors}


def verify_built(binary, source, head, target):
    return support('candidate_identity_cache').verify_built(SimpleNamespace(**globals()), binary, source, head, target)


def executable_outputs(args):
    outputs = []
    crate = None
    extra = ''
    for index, arg in enumerate(args):
        if arg == '--crate-name' and index + 1 < len(args):
            crate = args[index + 1]
        elif arg.startswith('--crate-name='):
            crate = arg.split('=', 1)[1]
        option = arg.removeprefix('-C')
        if option.startswith('extra-filename='):
            extra = option.split('=', 1)[1]
    for index, arg in enumerate(args):
        if arg == '-o' and index + 1 < len(args):
            outputs.append(Path(args[index + 1]).resolve())
        elif arg == '--out-dir' and index + 1 < len(args):
            if not crate:
                raise IdentityError('explicit crate name required for directory output')
            outputs.append((Path(args[index + 1]) / (crate + extra + ('.exe' if os.name == 'nt' else ''))).resolve())
    return {path: stamp(path.stat()) for path in outputs if path.is_file()}


def compiler_receipts(args, before, source, head, target):
    return support('candidate_identity_retention').compiler_receipts(SimpleNamespace(**globals()), args, before, source, head, target)


def record_launch(records):
    return support('candidate_identity_retention').record_launch(SimpleNamespace(**globals()), records)


def launch(args, replace=False):
    return support('candidate_identity_retention').launch(SimpleNamespace(**globals()), args, replace)


def compiler_inputs(args, source, target):
    """Refuse attaching candidate metadata to another checkout's final artifact."""
    inputs = [Path(arg).resolve() for arg in args[1:]
              if not arg.startswith('-') and arg.endswith('.rs')]
    if '-' in args or len(inputs) != 1 or not inputs[0].is_relative_to(source):
        raise IdentityError('compiler source is not a file in the candidate')
    if not inputs[0].is_file():
        raise IdentityError('compiler source file missing')
    head = git(source, 'rev-parse', 'HEAD')
    if inputs[0].is_relative_to(target) and os.environ.get('RMUX_IDENTITY_DOC_SESSION'):
        ticket = generation(source, head, target)
        parts = ('doctest-inputs', 'doctest-tmp', 'doctest-binaries')
        if ticket.get('kind') == 'final-sink' and host_os() == 'windows':
            parts += ('fixture-inputs',)
        if ticket.get('kind') == 'compile-consumer':
            support('candidate_identity_consumers').input_path(
                SimpleNamespace(**globals()), ticket, inputs[0])
        elif not any(inputs[0].is_relative_to(target / part) for part in parts):
            raise IdentityError('generated input outside qualified generator output')
    else:
        tracked_input(inputs[0], source, head)
    manifest = os.environ.get('CARGO_MANIFEST_DIR')
    if manifest and not Path(manifest).resolve().is_relative_to(source):
        raise IdentityError('compiler manifest belongs to another candidate')
    if manifest:
        if Path(manifest).resolve().is_relative_to(target):
            ticket = generation(source, head, target)
            if ticket.get('kind') != 'compile-consumer':
                raise IdentityError('generated manifest lacks observed consumer')
            support('candidate_identity_consumers').input_path(
                SimpleNamespace(**globals()), ticket, Path(manifest) / 'Cargo.toml')
        else:
            tracked_input(Path(manifest) / 'Cargo.toml', source, head)
    if (source / 'Cargo.lock').exists():
        tracked_input(source / 'Cargo.lock', source, head)
    outputs = []
    for index, arg in enumerate(args):
        if arg in ('-o', '--out-dir') and index + 1 < len(args):
            outputs.append(Path(args[index + 1]).resolve())
        elif arg.startswith('--out-dir='):
            outputs.append(Path(arg.split('=', 1)[1]).resolve())
    if not outputs or any(not path.is_relative_to(target) for path in outputs):
        raise IdentityError('compiler output is not in the private candidate target')


def rustc_wrapper(args):
    source, head, target = context()
    source_identity(source, head)
    target_identity(source, head, target)
    origin = os.environ.get('RMUX_IDENTITY_DOC_ORIGIN')
    if origin:
        generation(source, head, target)
        expanded = []
        for arg in args:
            if arg.startswith('@'):
                response = Path(arg[1:]).resolve(strict=True)
                if not response.is_relative_to(target / 'doctest-tmp'):
                    raise IdentityError('doctest response file outside generator output')
                expanded.extend(response.read_text().splitlines())
            else:
                expanded.append(arg)
        # This flag enables rustdoc's wrapper hook; it is not a test compiler
        # feature. rustdoc forwards it in its response file, so remove only that
        # harness-owned flag before invoking the unchanged stable test compiler.
        args = [arg for arg in expanded if arg != '-Zunstable-options']
    if '-' in args and origin:
        generation(source, head, target)
        origin = Path(origin).resolve(strict=True)
        if not origin.is_relative_to(source):
            raise IdentityError('doctest origin is outside the candidate')
        generated = target / 'doctest-inputs'
        generated.mkdir(exist_ok=True)
        raw = sys.stdin.buffer.read()
        code = generated / (hashlib.sha256(raw).hexdigest() + '.rs')
        code.write_bytes(raw)
        args = [str(code) if arg == '-' else arg for arg in args]
    selection = support('candidate_identity_windows' if os.name == 'nt' else 'candidate_identity_unix')
    args = [selection.invoked_compiler(args[0], os.environ['RMUX_IDENTITY_RUSTC']), *args[1:]]
    # Cargo calls wrappers for version/cfg probes and dependency build scripts too.
    final_link = '--test' in args or any(
        arg == '--crate-type=bin' or (arg == '--crate-type' and args[i + 1:i + 2] == ['bin'])
        for i, arg in enumerate(args))
    build_script = any(arg == 'build_script_build' for arg in args)
    probe = any(arg.startswith('--print') or arg in ('-vV', '-V', '--version') for arg in args)
    if os.environ.get('RMUX_IDENTITY_DOC_SESSION'):
        ticket = generation(source, head, target)
        if ticket.get('kind') == 'compile-consumer':
            support('candidate_identity_consumers').current(
                SimpleNamespace(**globals()), source, head, target, require_lock=not probe)
            if Path(args[0]).resolve(strict=True) != Path(ticket['compiler']):
                raise IdentityError('consumer invocation selects another compiler')
    witnessed = final_link and not build_script and not probe
    units = support('candidate_identity_units')
    guard = SimpleNamespace(**globals())
    if witnessed:
        compiler_inputs(args, source.resolve(), target)
    unit = None if probe else units.before(guard, args, source, head, target)
    if unit:
        args = units.with_depinfo(args, unit)
    if unit and unit['kind'] in ('bin', 'rust-test', 'build-script'):
        obj = target / 'identity.o'
        if not obj.is_file():
            raise IdentityError('embedded identity object missing')
        keep = {'macos': '-Wl,-u,_RMUX_BUILD_IDENTITY_V1',
                'linux': '-Wl,-u,RMUX_BUILD_IDENTITY_V1',
                'windows': '/INCLUDE:RMUX_BUILD_IDENTITY_V1'}[host_os()]
        args = [*args, '-C', 'link-arg=' + str(obj), '-C', 'link-arg=' + keep,
                *units.graph_object(guard, unit, source, head, target, os.environ['RMUX_IDENTITY_RUSTC'])]
    if unit:
        return units.compile_observed(guard, args, unit, source, head, target, witnessed)
    return subprocess.call(args, **jobserver())


def prepare(compilation_unit=None):
    keys = ('RMUX_IDENTITY_HEAD', 'RMUX_IDENTITY_SOURCE', 'RMUX_IDENTITY_TARGET')
    if any(os.environ.get(key) for key in keys):
        validate_context()
        return dict(os.environ)
    source = Path(git(Path.cwd(), 'rev-parse', '--show-toplevel')).resolve()
    head = git(source, 'rev-parse', 'HEAD')
    source, source_id = source_identity(source, head)
    command = secrets.token_hex(16)
    target = target_directory(source, head, command)
    supplied = os.environ.get('CARGO_TARGET_DIR')
    if supplied and Path(supplied).absolute() != target:
        raise IdentityError('shared/non-candidate CARGO_TARGET_DIR refused')
    if target.resolve() != target:
        raise IdentityError('redirected candidate target refused')
    floor = {'macos': 30, 'linux': 40, 'windows': 50}[host_os()] * 1024**3
    if shutil.disk_usage(source).free < floor:
        raise IdentityError('native build disk floor not met')
    for key in ('RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER'):
        if os.environ.get(key):
            raise IdentityError('existing compiler wrapper requires explicit composition')
    support('candidate_identity_manifest').initialize(SimpleNamespace(**globals()), compilation_unit, source, head, target)
    rustc = support('candidate_identity_windows' if os.name == 'nt'
                    else 'candidate_identity_unix').selected_compiler()
    if not rustc:
        raise IdentityError('rustc unavailable')
    marker = f'RMUX_BUILD_IDENTITY_V1:{head}:{source_id}:{host_os()}:END'
    code = target / 'identity.rs'
    code.write_text('#![no_std]\n#[used]\n#[no_mangle]\n'
                    f'pub static RMUX_BUILD_IDENTITY_V1: [u8; {len(marker)}] = *b"{marker}";\n')
    subprocess.run([rustc, '--crate-name', 'rmux_build_identity', '--crate-type', 'lib',
                    '--emit=obj', str(code), '-o', str(target / 'identity.o')], check=True)
    script = Path(__file__).resolve()
    env = dict(os.environ, RMUX_IDENTITY_SOURCE=str(source), RMUX_IDENTITY_HEAD=head,
               CARGO_TARGET_DIR=str(target), RMUX_IDENTITY_TARGET=str(target),
               RMUX_IDENTITY_PYTHON=sys.executable, RMUX_IDENTITY_SCRIPT=str(script),
               RMUX_IDENTITY_SCRIPT_SHA256=support('candidate_identity_context').direct_file_hash(script),
               RMUX_IDENTITY_RUSTC=rustc)
    rustdoc = shutil.which(os.environ.get('RUSTDOC', 'rustdoc'))
    if not rustdoc:
        raise IdentityError('rustdoc unavailable')
    env['RMUX_IDENTITY_RUSTDOC'] = rustdoc
    if os.name == 'nt':
        support('candidate_identity_windows').install(SimpleNamespace(**globals()), env, source, head, target)
        runner = subprocess.list2cmdline([sys.executable, *python_startup_flags(), str(script), 'run'])
    else:
        runner = support('candidate_identity_unix').install(env, sys.executable, script, rustdoc, target)
    version = subprocess.check_output([rustc, '-vV'], text=True)
    triple = next(line.removeprefix('host: ') for line in version.splitlines() if line.startswith('host: '))
    key = 'CARGO_TARGET_' + triple.upper().replace('-', '_') + '_RUNNER'
    if env.get(key):
        raise IdentityError('existing test runner requires explicit composition')
    env[key] = runner
    # Both layouts are used by Cargo with/without --target.
    directories = [target / 'debug', target / triple / 'debug']
    env['PATH'] = os.pathsep.join([*(str(path) for path in directories), env.get('PATH', '')])
    support('candidate_identity_context').write_context_bindings(env, target, key)
    return env


def python_startup_flags():
    return ['-I', '-S', '-B'] if host_os() == 'windows' and os.environ.get('RMUX_NATIVE_INPUT_MANIFEST') else []


def rustdoc_wrapper(args):
    if '--test' not in args:
        return subprocess.call(args, **jobserver())
    source, head, target = context()
    source_identity(source, head)
    inputs = [Path(arg).resolve() for arg in args[1:]
              if not arg.startswith('-') and arg.endswith('.rs')]
    if len(inputs) != 1 or not inputs[0].is_relative_to(source):
        raise IdentityError('doctest source is outside the candidate')
    blob = tracked_input(inputs[0], source, head)
    runner = ['--test-runtool', sys.executable,
              *(value for flag in python_startup_flags() for value in ('--test-runtool-arg', flag)),
              '--test-runtool-arg', str(Path(__file__).resolve()),
              '--test-runtool-arg', 'run']
    supplied = []
    filtered = []
    index = 0
    while index < len(args):
        arg = args[index]
        if arg in ('--test-runtool', '--test-runtool-arg') and index + 1 < len(args):
            supplied.extend(args[index:index + 2])
            index += 2
            continue
        if arg.startswith('--test-runtool') or arg.startswith('--test-builder'):
            raise IdentityError('foreign doctest runner or builder')
        filtered.append(arg)
        index += 1
    if supplied and supplied != runner:
        raise IdentityError('foreign doctest runner or builder')
    tmp = target / 'doctest-tmp'
    tmp.mkdir(exist_ok=True)
    token = secrets.token_hex(32)
    tickets = target / 'identity-generators'
    tickets.mkdir(exist_ok=True)
    ticket_file = tickets / (token + '.json')
    generator = Path(args[0]).resolve(strict=True)
    ticket = dict(active=True, commit=head, source=str(source), origin=str(inputs[0]), blob=blob,
                  origin_sha256=file_hash(inputs[0]), generator=str(generator),
                  generator_sha256=file_hash(generator), session=token)
    ticket_file.write_text(json.dumps(ticket) + '\n')
    crate = args[args.index('--crate-name') + 1] if '--crate-name' in args else inputs[0].stem.replace('-', '_')
    windows = host_os() == 'windows'
    if windows:
        # rustdoc includes the input name in persisted paths; MSVC still has
        # a bounded output path even when Python/Rust accept longer paths.
        filtered = [os.path.relpath(inputs[0]) if arg == str(inputs[0]) else arg for arg in filtered]
    binaries = target / 'doctest-binaries' / (token[:16] if windows else token)
    binaries.mkdir(parents=True)
    # rustdoc 1.96 keeps its wrapper hook unstable. Limit that opt-in to the
    # documented crate; ordinary Cargo/rustc invocations retain their toolchain.
    env = dict(os.environ, TMPDIR=str(tmp), TEMP=str(tmp), TMP=str(tmp),
               RMUX_IDENTITY_DOC_ORIGIN=str(inputs[0]), RMUX_IDENTITY_DOC_SESSION=token,
               RUSTC_BOOTSTRAP=crate)
    try:
        return subprocess.call([*filtered, '-Z', 'unstable-options', '--persist-doctests', str(binaries),
                                '--test-builder-wrapper', env['RUSTC_WRAPPER'], *runner], env=env, **jobserver())
    finally:
        ticket['active'] = False
        ticket_file.write_text(json.dumps(ticket) + '\n')


def validate_context(gate=False):
    source, head, target = context()
    source_identity(source, head)
    actual = Path(git(Path.cwd(), 'rev-parse', '--show-toplevel')).resolve()
    if actual != source:
        raise IdentityError('gate belongs to a different candidate source')
    context_guard = support('candidate_identity_context')
    guard = SimpleNamespace(**globals())
    bindings = context_guard.context_bindings(guard, source, head, target, gate=gate)
    script = Path(__file__).resolve()
    active_script = Path(bindings['RMUX_IDENTITY_SCRIPT']).resolve(strict=True)
    if context_guard.direct_file_hash(active_script) != bindings['RMUX_IDENTITY_SCRIPT_SHA256']:
        raise IdentityError('identity implementation differs')
    if active_script != script:
        candidate_script = source / 'scripts' / 'candidate_identity.py'
        if not gate or script != candidate_script.resolve(strict=True):
            raise IdentityError('identity implementation differs')
        context_guard.tracked_clean_file(guard, candidate_script, source, head)
    elif os.environ.get('RMUX_IDENTITY_SCRIPT') != str(script):
        raise IdentityError('identity implementation differs')
    python = bindings['RMUX_IDENTITY_PYTHON']
    if not python or Path(python).resolve() != Path(sys.executable).resolve():
        raise IdentityError('identity interpreter differs')
    if os.environ.get('RUSTC_WORKSPACE_WRAPPER'):
        raise IdentityError('workspace compiler wrapper differs')
    if os.name == 'nt':
        support('candidate_identity_windows').validate(SimpleNamespace(**globals()), source, head, target)
    else:
        # The legacy wrapper form belongs to the pinned 043 bytes, verified above.
        pinned_043 = (bindings['RMUX_IDENTITY_SCRIPT_SHA256']
                      == support('candidate_identity_harness').SCRIPT_SHA256)
        launchers = partial(support('candidate_identity_unix').accepted_launchers, pinned_043)
        wrapper = target / 'rustc-wrapper'
        if (os.environ.get('RUSTC_WRAPPER') != str(wrapper) or not wrapper.is_file()
                or wrapper.read_text() not in launchers([python, str(active_script), 'rustc'])):
            raise IdentityError('compiler wrapper missing or different')
    rustc = bindings['RMUX_IDENTITY_RUSTC']
    selected = support('candidate_identity_windows' if os.name == 'nt'
                       else 'candidate_identity_unix').selected_compiler()
    if not rustc or selected != rustc:
        raise IdentityError('compiler context differs')
    version = subprocess.check_output([rustc, '-vV'], text=True)
    triple = next(line.removeprefix('host: ') for line in version.splitlines() if line.startswith('host: '))
    runner = (subprocess.list2cmdline([python, str(active_script), 'run']) if os.name == 'nt'
              else shlex.join([python, str(active_script), 'run']))
    if os.environ.get('CARGO_TARGET_' + triple.upper().replace('-', '_') + '_RUNNER') != runner:
        raise IdentityError('test runner missing or different')
    if os.name != 'nt':
        doc_wrapper = target / 'rustdoc-wrapper'
        doc_command = [python, str(active_script), 'rustdoc', bindings['RMUX_IDENTITY_RUSTDOC']]
        if (os.environ.get('RUSTDOC') != str(doc_wrapper) or not doc_wrapper.is_file()
                or doc_wrapper.read_text() not in launchers(doc_command)):
            raise IdentityError('doctest wrapper missing or different')


def main():
    try:
        mode, *args = sys.argv[1:]
        if mode == 'with-manifest':
            return support('candidate_identity_manifest').action(sys.modules[__name__], args)
        if mode == 'with':
            if args[:1] != ['--'] or len(args) < 2:
                raise IdentityError('usage: candidate_identity.py with -- command ...')
            return subprocess.call(args[1:], env=prepare())
        if mode == 'validate':
            validate_context()
            return 0
        if mode == 'validate-gate':
            validate_context(gate=True)
            return 0
        if mode == 'rustdoc':
            return rustdoc_wrapper(args)
        if mode == 'check':
            source, head, target = context()
            selected = args[0] if Path(args[0]).is_absolute() else shutil.which(args[0])
            if selected is None or not Path(selected).exists():
                return 89
            record = support('candidate_conpty_runtime' if os.name == 'nt' else 'candidate_identity_retention').select(
                SimpleNamespace(**globals()), Path(selected).absolute(), source, head, target)
            record_launch([record])
            print(record['path'])
            return 0
        if mode == 'build-script':
            return support('candidate_identity_buildscripts').run(SimpleNamespace(**globals()), args)
        if mode == 'build-exec':
            return support('candidate_identity_retention').exec_build_script(SimpleNamespace(**globals()), args)
        if mode == 'rustc':
            return rustc_wrapper(args)
        if mode == 'fixture':
            if args[:1] in (['final-sink-build'], ['final-sink-check']):
                return support('candidate_identity_windows').final_sink(args, sys.modules[__name__])
            from candidate_identity_fixtures import certify
            return certify(args, sys.modules[__name__])
        if mode in ('exec', 'exec-argv0'):
            return support('candidate_identity_retention').exec_action(SimpleNamespace(**globals()), args, mode)
        if mode == 'consumer':
            return support('candidate_identity_consumers').action(args, sys.modules[__name__])
        if mode == 'rust-consumer-before':
            return support('native_input_rust_consumer').before()
        if mode == 'run':
            return launch(args)
        raise IdentityError('unknown identity action')
    except (IdentityError, OSError, subprocess.CalledProcessError) as error:
        # No environment, command arguments, credentials, or full subprocess output.
        print('candidate identity FAIL: ' + str(error), file=sys.stderr)
        return 86


if __name__ == '__main__':
    if os.name == 'nt' and os.environ.get('RMUX_NATIVE_INPUT_MANIFEST'):
        sys.path.insert(0, str(Path(__file__).resolve().parent))
        support('native_input_windows_process').install()
    sys.exit(main())
