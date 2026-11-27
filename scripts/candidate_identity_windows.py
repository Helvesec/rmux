"""Build and validate the native Windows argument transport in the private target."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import ctypes

_file_info_api = None


class FileBasicInfo(ctypes.Structure):
    _fields_ = [('CreationTime', ctypes.c_int64), ('LastAccessTime', ctypes.c_int64),
                ('LastWriteTime', ctypes.c_int64), ('ChangeTime', ctypes.c_int64),
                ('FileAttributes', ctypes.c_uint32)]


def change_time(path):
    """Python's Windows st_ctime is birth time; use the native metadata clock."""
    import msvcrt
    global _file_info_api
    if _file_info_api is None:
        kernel = ctypes.WinDLL('kernel32', use_last_error=True)
        _file_info_api = kernel.GetFileInformationByHandleEx
        _file_info_api.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
        _file_info_api.restype = ctypes.c_int
    info = FileBasicInfo()
    with Path(path).open('rb') as stream:
        handle = msvcrt.get_osfhandle(stream.fileno())
        if not _file_info_api(handle, 0, ctypes.byref(info), ctypes.sizeof(info)) or info.ChangeTime <= 0:
            raise OSError('native Windows file change time unavailable')
    return info.ChangeTime * 100



def selected_compiler(selected=None):
    """Return the compiler binary that a ``rustc`` selection runs.

    rustup installs ``rustc.exe`` as a hard link to, or a copy of, its
    multi-call proxy, so no path comparison separates the proxy from a
    compiler; and Cargo started under rustup bypasses the proxy to the
    toolchain binary. The selection's sysroot names that binary. A compiler
    that is its own sysroot binary keeps its spelling.
    """
    selected = selected or shutil.which(os.environ.get('RUSTC', 'rustc'))
    if not selected:
        return selected
    sysroot = subprocess.check_output([selected, '--print', 'sysroot'], text=True).strip()
    toolchain = Path(sysroot) / 'bin' / Path(selected).name
    if not toolchain.is_file() or os.path.samefile(toolchain, selected):
        return selected
    return str(toolchain.resolve(strict=True))


def invoked_compiler(spelling, bound):
    """Return the compiler binary that the wrapper runs for Cargo's ``spelling``.

    An explicit RUSTC naming the proxy reaches the wrapper verbatim: identify
    and run its toolchain binary, which ``prepare()`` bound.
    """
    if not Path(spelling).is_absolute() or Path(spelling).resolve() == Path(bound).resolve():
        return spelling
    return selected_compiler(spelling)


def install(guard, env, source, head, target):
    code = (Path(source) / 'scripts' / 'candidate_identity_windows.rs').resolve()
    guard.tracked_input(code, source, head)
    wrapper = target / 'rustc-wrapper.exe'
    args = [env['RMUX_IDENTITY_RUSTC'], '--crate-name', 'candidate_identity_windows',
            '--crate-type', 'bin', '--edition=2021', str(code), '-o', str(wrapper),
            '-C', 'link-arg=' + str(target / 'identity.o'),
            '-C', 'link-arg=/INCLUDE:RMUX_BUILD_IDENTITY_V1']
    units = guard.support('candidate_identity_units')
    unit = units.before(guard, args, source, head, target, env)
    args = units.with_depinfo(args, unit)
    args.extend(units.graph_object(guard, unit, source, head, target, env['RMUX_IDENTITY_RUSTC']))
    subprocess.run(args, env=env, check=True)
    units.after(guard, unit, source, head, target)
    guard.compiler_receipts(args, {}, source, head, target)
    rustdoc = target / 'rustdoc-wrapper.exe'
    shutil.copyfile(wrapper, rustdoc)
    env.update(RUSTC_WRAPPER=str(wrapper), RUSTDOC=str(rustdoc))
    bindings = {key: env[key] for key in ('RMUX_IDENTITY_PYTHON', 'RMUX_IDENTITY_SCRIPT', 'RMUX_IDENTITY_RUSTDOC')}
    (target / 'native-wrapper-bindings.json').write_text(json.dumps(dict(
        bindings=bindings, sha256=guard.file_hash(wrapper))) + '\n')


def validate(guard, source, head, target):
    bindings = json.loads((target / 'native-wrapper-bindings.json').read_text())
    for key, value in bindings['bindings'].items():
        if os.environ.get(key) != value:
            raise guard.IdentityError('native wrapper bindings differ')
    for key, name in (('RUSTC_WRAPPER', 'rustc-wrapper.exe'), ('RUSTDOC', 'rustdoc-wrapper.exe')):
        path = target / name
        if os.environ.get(key) != str(path):
            raise guard.IdentityError('native wrapper path differs')
        record = guard.verify_built(path, source, head, target)
        if record['sha256'] != bindings['sha256']:
            raise guard.IdentityError('native wrapper bytes differ')


def parent_executable():
    """Resolve the actual Rust generator from the Windows kernel."""
    import ctypes
    from ctypes import wintypes

    class ProcessBasicInformation(ctypes.Structure):
        _fields_ = [('reserved', ctypes.c_void_p), ('peb', ctypes.c_void_p),
                    ('reserved2', ctypes.c_void_p * 2), ('pid', ctypes.c_size_t),
                    ('parent', ctypes.c_size_t)]

    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    native = ctypes.WinDLL('ntdll')
    kernel.GetCurrentProcess.restype = wintypes.HANDLE
    native.NtQueryInformationProcess.argtypes = [wintypes.HANDLE, wintypes.ULONG,
                                                ctypes.c_void_p, wintypes.ULONG, ctypes.c_void_p]
    info = ProcessBasicInformation()
    if native.NtQueryInformationProcess(kernel.GetCurrentProcess(), 0, ctypes.byref(info),
                                       ctypes.sizeof(info), None) != 0:
        raise OSError('Windows fixture parent unavailable')
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.QueryFullProcessImageNameW.argtypes = [wintypes.HANDLE, wintypes.DWORD,
                                                wintypes.LPWSTR, ctypes.POINTER(wintypes.DWORD)]
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    handle = kernel.OpenProcess(0x1000, False, info.parent)
    if not handle:
        raise OSError('Windows fixture parent cannot be opened')
    try:
        buffer = ctypes.create_unicode_buffer(32768)
        length = wintypes.DWORD(len(buffer))
        if not kernel.QueryFullProcessImageNameW(handle, 0, buffer, ctypes.byref(length)):
            raise OSError('Windows fixture parent image unavailable')
        return Path(buffer.value).resolve(strict=True)
    finally:
        kernel.CloseHandle(handle)


def validate_generation(guard, ticket, source, head, target):
    producer = guard.verify_built(Path(ticket['generator']), source, head, target)
    if producer['sha256'] != ticket['generator_sha256'] or producer['unit']['kind'] != 'rust-test':
        raise guard.IdentityError('final-sink generator differs from verified Rust test')
    directory = Path(ticket['directory']).resolve(strict=True)
    if not directory.is_relative_to(target / 'fixture-inputs'):
        raise guard.IdentityError('final-sink inputs outside private target')
    for name, sha in ticket['files'].items():
        path = directory / name
        if name not in ('main.rs', 'byte_observer.rs') or path.is_symlink() or guard.file_hash(path) != sha:
            raise guard.IdentityError('final-sink generated input changed')
    for path, sha in ticket['sources'].items():
        guard.tracked_input(Path(path), source, head)
        if guard.file_hash(path) != sha:
            raise guard.IdentityError('final-sink source changed')


def final_sink(args, guard):
    source, head, target = guard.context()
    source, _ = guard.source_identity(source, head)
    if guard.host_os() != 'windows':
        raise guard.IdentityError('final-sink compiled fixture requires Windows')
    action, *args = args
    if action == 'final-sink-check' and len(args) == 1:
        record = guard.verify_built(Path(args[0]), source, head, target)
        guard.record_launch([record])
        return 0
    if action != 'final-sink-build' or len(args) < 2:
        raise guard.IdentityError('unknown final-sink fixture action')
    directory = Path(args[0]).resolve(strict=True)
    compiler = Path(args[1]).resolve(strict=True)
    if (not directory.is_relative_to(target / 'fixture-inputs')
            or compiler != Path(os.environ['RMUX_IDENTITY_RUSTC']).resolve(strict=True)):
        raise guard.IdentityError('final-sink compiler or directory differs')
    producer = guard.verify_built(parent_executable(), source, head, target)
    if producer['unit']['kind'] != 'rust-test':
        raise guard.IdentityError('only a verified Rust test may generate final-sink')
    origin = source / 'crates/rmux-server/src/test_shell/final_sink/windows_byte_child.rs'
    observer = origin.with_name('byte_observer.rs')
    for path in (origin, observer):
        guard.tracked_input(path, source, head)
    text = origin.read_text(encoding='utf-8')
    main = text.split('CHILD_MAIN_SOURCE: &str = r##"', 1)[1].split('"##;', 1)[0].encode()
    inputs = {'main.rs': main, 'byte_observer.rs': observer.read_bytes()}
    for name, expected in inputs.items():
        path = directory / name
        if path.is_symlink() or path.read_bytes() != expected:
            raise guard.IdentityError('final-sink inputs differ from pinned generator')
    import secrets
    token = secrets.token_hex(32)
    tickets = target / 'identity-generators'
    tickets.mkdir(exist_ok=True)
    ticket_file = tickets / (token + '.json')
    ticket = dict(active=True, kind='final-sink', commit=head, source=str(source),
                  origin=str(origin), blob=guard.tracked_input(origin, source, head),
                  origin_sha256=guard.file_hash(origin), generator=producer['path'],
                  generator_sha256=producer['sha256'], session=token, directory=str(directory),
                  files={name:guard.file_hash(directory / name) for name in inputs},
                  sources={str(path):guard.file_hash(path) for path in (origin, observer)})
    ticket_file.write_text(json.dumps(ticket, sort_keys=True) + '\n')
    os.environ.update(RMUX_IDENTITY_DOC_ORIGIN=str(origin), RMUX_IDENTITY_DOC_SESSION=token)
    try:
        partial = directory / 'rmux-final-sink-child.partial.exe'
        program = directory / 'rmux-final-sink-child.exe'
        retained = directory / 'rmux-final-sink-child.compiler.exe'
        if partial.exists() or program.exists() or retained.exists():
            raise guard.IdentityError('final-sink build refuses an existing executable')
        expected = [str(compiler), '--edition', '2021', '--crate-name', 'rmux_final_sink_child',
                    '--crate-type', 'bin', '-o', str(partial), str(directory / 'main.rs')]
        if args[1:] != expected:
            raise guard.IdentityError('final-sink compilation arguments differ')
        # Witness the compiler at a permanent pathname. The partial name is
        # only a publication link; renaming it cannot invalidate that history.
        args = list(expected)
        args[args.index('-o') + 1] = str(retained)
        units = guard.support('candidate_identity_units')
        env = dict(os.environ)
        env.pop('OUT_DIR', None)
        unit = units.before(guard, args, source, head, target, env)
        unit['graph']['dependencies'].append(producer['unit'])
        # std is implicit in this direct rustc invocation. Bind its pinned
        # toolchain libraries too, using the existing toolchain dependency form.
        libdir = Path(subprocess.check_output(
            [str(compiler), '--print', 'target-libdir'], text=True).strip()).resolve(strict=True)
        libraries = sorted(path.resolve(strict=True) for path in libdir.iterdir()
                           if path.suffix in ('.rlib', '.rmeta', '.lib', '.dll'))
        libraries.extend(sorted(compiler.parent.glob('*.dll')))
        if not libraries:
            raise guard.IdentityError('final-sink compiler libraries unavailable')
        unit['graph']['dependencies'].extend(
            dict(kind='toolchain', path=str(path), sha256=guard.file_hash(path)) for path in libraries)
        unit['graph_sha256'] = units.digest(unit['graph'])
        unit['depinfo'] = target / 'identity-depinfo' / (unit['graph_sha256'] + '.d')
        args = units.with_depinfo(args, unit)
        args.extend(['-C', 'link-arg=' + str(target / 'identity.o'),
                     '-C', 'link-arg=/INCLUDE:RMUX_BUILD_IDENTITY_V1'])
        args.extend(units.graph_object(guard, unit, source, head, target, str(compiler)))
        (directory / 'rustc-observed-argv.json').write_text(json.dumps(args) + '\n')
        status = units.compile_observed(guard, args, unit, source, head, target, True)
        if status:
            return status
        record = guard.verify_built(retained, source, head, target)
        publish(guard, retained, program, record, source, head, target, staging=partial)
        return 0
    finally:
        ticket['active'] = False
        ticket_file.write_text(json.dumps(ticket, sort_keys=True) + '\n')


def publish(guard, partial, program, record, source, head, target, staging=None):
    # Both names retain the same witnessed compiler output. Removing the
    # compiler pathname or editing its immutable receipt invalidates history.
    os.link(partial, staging if staging is not None else program)
    if staging is not None:
        # Windows rename refuses an occupied destination. Keep the original
        # compiler product even if linking or atomic publication fails.
        os.rename(staging, program)
    if (guard.file_hash(program) != record['sha256']
            or guard.file_hash(partial) != record['sha256']):
        raise guard.IdentityError('final-sink output changed during publication')
    units = guard.support('candidate_identity_units')
    for receipt in (Path(record['build']), Path(record['unit']['receipt'])):
        raw = json.loads(receipt.read_text())
        units.preserve(guard, receipt.parent, raw)
        raw['path'] = str(program)
        raw['compiler_output'] = str(partial)
        published = units.preserve(guard, receipt.parent, raw)
        # Publish a current selector only; the original immutable observation
        # and its compiler output remain available to prior dependencies.
        selector = receipt.parent / (record['sha256'] + '.json')
        import secrets
        temporary = selector.with_name('.' + secrets.token_hex(16) + '.json')
        temporary.write_bytes(published.read_bytes())
        os.replace(temporary, selector)
    guard.verify_built(program, source, head, target)
