"""Attribute the two existing Unix launcher templates to their Rust test producer."""
import ctypes
import hashlib
import json
import os
from pathlib import Path
import re
import sys


# The Rust tests write these #!/bin/sh templates. Certification replaces each
# with a shell-free equivalent: dash drops every environment name that is not a
# shell identifier, such as CARGO_BIN_EXE_rmux-daemon, and shells rewrite PWD,
# OLDPWD, SHLVL and _. The runner receives the caller's env and argv unchanged.
SDK_TEMPLATE = b'#!/bin/sh\ncd / || exit 1\nexec "$RMUX_SDK_REAL_DAEMON" "$@"\n'
CLI_TEMPLATE = re.compile(rb"#!/bin/sh\n'([^'\n]+)' \"\$@\" >/dev/null 2>&1 &\necho \$! > '([^'\n]+)'\nexit 0\n")


def _prologue(command):
    return ('import os, sys\n'
            f'COMMAND = {[os.fsencode(part) for part in command]!r}\n'
            'ARGS = [os.fsencode(arg) for arg in sys.argv[1:]]\n')


def sdk_launcher(runner):
    return (_prologue(runner) + "os.chdir('/')\n"
            "os.execv(COMMAND[0], COMMAND + [os.environb.get(b'RMUX_SDK_REAL_DAEMON', b'')] + ARGS)\n")


def cli_launcher(command, pid_path):
    # Behave as the template's background job: same PID through exec, input
    # from /dev/null, output discarded, SIGINT and SIGQUIT ignored.
    return (_prologue(command) + f'PID_PATH = {os.fsencode(pid_path)!r}\n' + '''import signal
pid = os.fork()
if pid == 0:
    try:
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        signal.signal(signal.SIGQUIT, signal.SIG_IGN)
        os.dup2(os.open('/dev/null', os.O_RDONLY), 0)
        os.dup2(os.open('/dev/null', os.O_WRONLY), 1)
        os.dup2(1, 2)
        os.execv(COMMAND[0], COMMAND + ARGS)
    finally:
        os._exit(127)
with open(PID_PATH, 'w') as pid_file:
    pid_file.write('%d\\n' % pid)
''')


def parent_executable():
    parent = os.getppid()
    if sys.platform == 'linux':
        return Path(f'/proc/{parent}/exe').resolve(strict=True)
    if sys.platform == 'darwin':
        library = ctypes.CDLL('/usr/lib/libproc.dylib')
        library.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
        buffer = ctypes.create_string_buffer(4096)
        if library.proc_pidpath(parent, buffer, len(buffer)) <= 0:
            raise OSError('fixture producer unavailable')
        return Path(os.fsdecode(buffer.value)).resolve(strict=True)
    raise OSError('generated shell fixture requires Unix')


def certify(args, guard):
    source, head, target = guard.context()
    source, source_id = guard.source_identity(source, head)
    # Resolve the real caller from the kernel, never from a supplied producer path.
    producer = guard.verify_built(parent_executable(), source, head, target)
    producer_build = json.loads(Path(producer['build']).read_text())
    if producer_build.get('kind') != 'rust-test':
        raise guard.IdentityError('only a verified Rust test may generate a fixture')
    path = Path(args[0]).absolute()
    if path.is_symlink() or not path.is_file():
        raise guard.IdentityError('fresh regular fixture required')
    before = guard.stamp(path.stat())
    data = path.read_bytes()
    runner = [sys.executable, str(Path(guard.__file__).resolve()), 'exec']
    if args[1] == 'sdk':
        if data != SDK_TEMPLATE:
            raise guard.IdentityError('SDK fixture differs from its frozen template')
        body = sdk_launcher(runner)
    elif args[1] == 'cli':
        match = CLI_TEMPLATE.fullmatch(data)
        if not match:
            raise guard.IdentityError('CLI fixture differs from its frozen template')
        guard.verify_built(Path(os.fsdecode(match[1])), source, head, target)
        body = cli_launcher([*runner, os.fsdecode(match[1])], os.fsdecode(match[2]))
    else:
        raise guard.IdentityError('unknown generated fixture kind')
    marker = f'# RMUX_BUILD_IDENTITY_V1:{head}:{source_id}:{guard.host_os()}:END\n'
    data = (guard.support('candidate_identity_unix').shebang(sys.executable) + marker + body).encode()
    directory = target / 'generated-fixtures' / producer['sha256']
    directory.mkdir(parents=True, exist_ok=True)
    binary = directory / hashlib.sha256(data).hexdigest()
    binary.write_bytes(data)
    binary.chmod(0o700)
    record = guard.verify(binary, source, head, target)
    binary = guard.support('candidate_identity_retention').retain(
        guard, binary, target, record['sha256'])
    record['path'] = str(binary)
    record.update(input=producer_build['input'], input_sha256=producer_build['input_sha256'],
                  compiler=producer_build['compiler'], compiler_sha256=producer_build['compiler_sha256'],
                  exit=0, kind='generated-fixture', producer=producer['path'], producer_sha256=producer['sha256'])
    guard.support('candidate_identity_retention').publish(guard, target, record)
    if guard.stamp(path.stat()) != before:
        raise guard.IdentityError('fixture changed during attribution')
    # Keep the test's original pathname and arguments; only its backing artifact
    # moves into the private target. exec preserves the original launcher PID.
    path.unlink()
    path.symlink_to(binary)
    return 0
