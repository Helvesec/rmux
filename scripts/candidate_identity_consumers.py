"""Witness generated compile-fail consumers without replacing their diagnostics."""
import json
import os
from pathlib import Path
import secrets
import subprocess


def parent_executable(g):
    if os.name != 'nt':
        return g.support('candidate_identity_fixtures').parent_executable()
    import ctypes
    from ctypes import wintypes

    class ProcessInfo(ctypes.Structure):
        _fields_ = [('reserved', ctypes.c_void_p), ('peb', ctypes.c_void_p),
                    ('reserved2', ctypes.c_void_p * 2), ('pid', ctypes.c_size_t),
                    ('parent', ctypes.c_size_t)]

    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    native = ctypes.WinDLL('ntdll')
    kernel.GetCurrentProcess.restype = wintypes.HANDLE
    native.NtQueryInformationProcess.argtypes = [wintypes.HANDLE, wintypes.ULONG,
                                                ctypes.c_void_p, wintypes.ULONG, ctypes.c_void_p]
    info = ProcessInfo()
    if native.NtQueryInformationProcess(kernel.GetCurrentProcess(), 0, ctypes.byref(info),
                                       ctypes.sizeof(info), None):
        raise OSError('consumer producer unavailable')
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.QueryFullProcessImageNameW.argtypes = [wintypes.HANDLE, wintypes.DWORD,
                                                wintypes.LPWSTR, ctypes.POINTER(wintypes.DWORD)]
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    handle = kernel.OpenProcess(0x1000, False, info.parent)
    if not handle:
        raise OSError('consumer producer cannot be opened')
    try:
        buffer = ctypes.create_unicode_buffer(32768)
        size = wintypes.DWORD(len(buffer))
        if not kernel.QueryFullProcessImageNameW(handle, 0, buffer, ctypes.byref(size)):
            raise OSError('consumer producer image unavailable')
        return Path(buffer.value).resolve(strict=True)
    finally:
        kernel.CloseHandle(handle)


def input_path(g, ticket, path):
    directory = Path(ticket['directory']).resolve(strict=True)
    path = Path(path)
    resolved = path.resolve(strict=True)
    if path.is_symlink() or not resolved.is_relative_to(directory):
        raise g.IdentityError('consumer input outside observed project')
    name = resolved.relative_to(directory).as_posix()
    g.support('native_input_windows_consumer').verify(g, ticket, path)
    if ticket['files'].get(name) != g.file_hash(resolved):
        raise g.IdentityError('consumer generated input changed or unobserved')


def validate(g, ticket, source, head, target, require_lock=True):
    if ticket['commit'] != head or ticket['source'] != str(source):
        raise g.IdentityError('consumer ticket belongs to another candidate')
    directory = Path(ticket['directory'])
    if (directory.resolve(strict=True) != directory or directory.is_symlink()
            or not directory.is_relative_to(target / 'consumer-inputs')):
        raise g.IdentityError('consumer project outside private target')
    producer = g.verify_built(Path(ticket['generator']), source, head, target)
    if producer['sha256'] != ticket['generator_sha256'] or producer['unit']['kind'] != 'rust-test':
        raise g.IdentityError('consumer producer differs from observed Rust test')
    g.support('native_input_windows_consumer').verify(g, ticket)
    for name in ticket['files']:
        input_path(g, ticket, directory / name)
    manifest = directory / 'Cargo.toml'
    input_path(g, ticket, manifest)
    dependency = Path(ticket['dependency'])
    g.tracked_input(dependency / 'Cargo.toml', source, head)
    data = g.support('toml_reader').loads(manifest.read_text())
    if data.get('dependencies') != {ticket['package']: {'path': str(dependency)}}:
        raise g.IdentityError('consumer dependency differs from pinned candidate')
    for key in ('cargo', 'compiler'):
        if g.file_hash(ticket[key]) != ticket[key + '_sha256']:
            raise g.IdentityError('consumer tool bytes changed')
    if require_lock:
        if ticket['phase'] != 'frozen':
            raise g.IdentityError('consumer lock is not resolved and frozen')
        input_path(g, ticket, directory / 'Cargo.lock')
        pinned = g.support('toml_reader').loads((source / 'Cargo.lock').read_text())['package']
        allowed = {(p['name'], p['version'], p.get('source'), p.get('checksum')) for p in pinned}
        for package in g.support('toml_reader').loads((directory / 'Cargo.lock').read_text())['package']:
            if package.get('source') and (package['name'], package['version'],
                    package.get('source'), package.get('checksum')) not in allowed:
                raise g.IdentityError('consumer dependency absent from candidate lock')


def current(g, source, head, target, require_lock=True):
    ticket = g.generation(source, head, target)
    if ticket.get('kind') != 'compile-consumer':
        raise g.IdentityError('consumer generation ticket missing')
    g.support('native_input_windows_consumer').verify(g, ticket)
    validate(g, ticket, source, head, target, require_lock)
    compiler = Path(os.environ['RMUX_IDENTITY_RUSTC']).resolve(strict=True)
    if compiler != Path(ticket['compiler']):
        raise g.IdentityError('consumer compiler context changed')
    active = Path(os.environ['CARGO_TARGET_DIR']).resolve()
    if active != Path(ticket['directory']) / 'target':
        raise g.IdentityError('consumer output target differs')
    return ticket


def action(args, g):
    source, head, target = g.context()
    g.source_identity(source, head)
    producer = g.verify_built(parent_executable(g), source, head, target)
    if producer['unit']['kind'] != 'rust-test':
        raise g.IdentityError('consumer caller is not an observed Rust test')
    if args[:1] == ['check'] and len(args) == 1:
        ticket = current(g, source, head, target)
        if producer['path'] != ticket['generator'] or producer['sha256'] != ticket['generator_sha256']:
            raise g.IdentityError('consumer caller differs from original generator')
        return 0
    if args[:1] != ['prepare'] or len(args) != 4:
        raise g.IdentityError('invalid consumer attribution action')
    _, directory, dependency, package = args
    directory = Path(directory).resolve(strict=True)
    dependency = Path(dependency).resolve(strict=True)
    if package not in ('rmux-sdk', 'rmux-proto') or dependency != source / 'crates' / package:
        raise g.IdentityError('consumer dependency is not an allowed candidate package')
    build = json.loads(Path(producer['build']).read_text())
    if Path(build['input']) != dependency / 'tests/public_api_extensibility.rs':
        raise g.IdentityError('consumer generator is not the pinned public API test')
    if not directory.is_relative_to(target / 'consumer-inputs'):
        raise g.IdentityError('consumer project outside private target')
    origin = source / 'tests/support/public_api_compile_fail.rs'
    blob = g.tracked_input(origin, source, head)
    cargo = Path(os.environ['CARGO']).resolve(strict=True)
    compiler = Path(os.environ['RMUX_IDENTITY_RUSTC']).resolve(strict=True)
    files = [directory / 'Cargo.toml', *sorted((directory / 'src/bin').glob('*.rs'))]
    if len(files) < 2:
        raise g.IdentityError('consumer source population empty')
    token = secrets.token_hex(32)
    ticket = dict(active=True, kind='compile-consumer', commit=head, source=str(source),
                  origin=str(origin), blob=blob, origin_sha256=g.file_hash(origin),
                  generator=producer['path'], generator_sha256=producer['sha256'],
                  session=token, directory=str(directory), dependency=str(dependency), package=package,
                  cargo=str(cargo), cargo_sha256=g.file_hash(cargo),
                  compiler=str(compiler), compiler_sha256=g.file_hash(compiler), phase='resolving',
                  files={})
    retention = g.support('native_input_windows_consumer')
    if retention.enabled(g):
        g.tracked_input(source / 'Cargo.lock', source, head)
        lock = directory / 'Cargo.lock'
        if lock.exists() or lock.is_symlink():
            raise g.IdentityError('consumer seed lock already exists')
        lock.write_bytes((source / 'Cargo.lock').read_bytes())
        files.append(lock)
        retention.retain(g, ticket, files, 'initial')
    ticket['files'] = {p.relative_to(directory).as_posix(): g.file_hash(p) for p in files}
    validate(g, ticket, source, head, target, require_lock=False)
    tickets = target / 'identity-generators'
    tickets.mkdir(exist_ok=True)
    record = tickets / (token + '.json')
    record.write_text(json.dumps(ticket, sort_keys=True) + '\n')
    env = dict(os.environ, CARGO_TARGET_DIR=str(directory / 'target'),
               RMUX_IDENTITY_DOC_SESSION=token)
    env.pop('RMUX_IDENTITY_DOC_ORIGIN', None)
    # Cargo metadata extends the actual candidate lock for this new workspace;
    # generate-lockfile would independently choose newer cached dependencies.
    g.tracked_input(source / 'Cargo.lock', source, head)
    if not retention.enabled(g):
        (directory / 'Cargo.lock').write_bytes((source / 'Cargo.lock').read_bytes())
    else:
        retention.verify(g, ticket)
    argv = [str(cargo), 'metadata', '--offline', '--format-version', '1']
    with (directory / 'lock-resolution.log').open('xb') as log:
        status = subprocess.call(argv,
                                 cwd=directory, env=env, stdout=log, stderr=log)
    (directory / 'lock-resolution-observation.json').write_text(json.dumps(
        dict(ticket=ticket, status=status, argv=argv,
             seed_lock_sha256=g.file_hash(source / 'Cargo.lock')),
        sort_keys=True) + '\n')
    if status:
        raise g.IdentityError('consumer offline lock resolution failed')
    if retention.enabled(g):
        for path in files:
            if path.name != 'Cargo.lock':
                retention.verify(g, ticket, path)
    retention.retain(g, ticket, [directory / name for name in ticket['files']], 'resolved')
    ticket['files']['Cargo.lock'] = g.file_hash(directory / 'Cargo.lock')
    ticket['phase'] = 'frozen'
    validate(g, ticket, source, head, target)
    record.write_text(json.dumps(ticket, sort_keys=True) + '\n')
    print(token)
    return 0
