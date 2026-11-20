"""Compile the Unix argv transport once, with immutable recipe-bound bytes."""
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile


def literal(value):
    return 'b"' + ''.join(f'\\x{byte:02x}' for byte in os.fsencode(value)) + '"'


def shebang(interpreter):
    """Return the line that starts a shell-free wrapper with ``interpreter``.

    Cargo exports names such as ``CARGO_BIN_EXE_rmux-daemon``. dash, the
    Debian/Ubuntu ``/bin/sh``, drops every environment name that is not a shell
    identifier before a ``#!/bin/sh`` script runs, so ``env!`` loses them.
    """
    line = '#!' + interpreter + '\n'
    if (not os.path.isabs(interpreter) or any(character.isspace() for character in interpreter)
            or len(os.fsencode(line)) > 255):
        raise OSError('identity interpreter path cannot start a shell-free wrapper')
    return line


def script_launcher(command, forward_argv0=False):
    """Return a shell-free wrapper that execs ``command`` with the caller's arguments."""
    interpreter = command[0]
    first = 0 if forward_argv0 else 1
    return (shebang(interpreter) + 'import os, sys\n'
            + f'os.execv({interpreter!r}, {list(command)!r} + sys.argv[{first}:])\n')


def accepted_launchers(pinned_043, command):
    """Return every wrapper text the gate accepts for ``command``.

    The candidate's own harness writes only the shell-free form. The pinned 043
    harness, identified by its script hash, predates that form and keeps the
    exact ``#!/bin/sh`` wrapper it was frozen with.
    """
    forms = {script_launcher(command)}
    if pinned_043:
        forms.add('#!/bin/sh\nexec ' + shlex.join(command) + ' "$@"\n')
    return forms



def selected_compiler(selected=None):
    """Return the compiler binary that a ``rustc`` selection runs.

    rustup and snap install ``rustc`` as a link to one multi-call proxy, so its
    realpath names the dispatcher, and Cargo itself bypasses the proxy. Only a
    link is asked for its sysroot, which names the toolchain binary; an
    ordinary compiler keeps its spelling, so its identity is unchanged.
    """
    selected = selected or shutil.which(os.environ.get('RUSTC', 'rustc'))
    resolved = selected and Path(selected).resolve(strict=True)
    if not selected or resolved == Path(selected).absolute():
        return selected
    sysroot = subprocess.check_output([selected, '--print', 'sysroot'], text=True).strip()
    toolchain = Path(sysroot) / 'bin' / Path(selected).name
    if not toolchain.is_file() or toolchain.resolve() == resolved:
        return selected
    return str(toolchain.resolve(strict=True))


def invoked_compiler(spelling, bound):
    """Return the compiler binary that the wrapper runs for Cargo's ``spelling``.

    Without RUSTC, Cargo passes a bare ``rustc``: a PATH lookup, as the
    unwrapped call would make, never a file in the build's working directory.
    A proxy link (rustup, snap) runs its toolchain binary: identify and run it.
    """
    if os.sep not in spelling:
        spelling = shutil.which(spelling) or spelling
    if Path(spelling).resolve() == Path(bound).resolve():
        return spelling
    return selected_compiler(spelling)


def install(env, python, script, rustdoc, target):
    """Install the shell-free compiler and doctest wrappers; return the test runner."""
    wrapper = target / 'rustc-wrapper'
    wrapper.write_text(script_launcher([python, str(script), 'rustc']))
    wrapper.chmod(0o755)
    env['RUSTC_WRAPPER'] = str(wrapper)
    doc_wrapper = target / 'rustdoc-wrapper'
    doc_wrapper.write_text(script_launcher([python, str(script), 'rustdoc', rustdoc]))
    doc_wrapper.chmod(0o755)
    env['RUSTDOC'] = str(doc_wrapper)
    return shlex.join([python, str(script), 'run'])


def launcher_source(g, binary, source, head, target):
    context = dict(RMUX_IDENTITY_SOURCE=str(source), RMUX_IDENTITY_HEAD=head,
                   RMUX_IDENTITY_TARGET=str(target))
    marker = (f'RMUX_BUILD_IDENTITY_V1:{head}:'
              + hashlib.sha256(os.fsencode(str(source))).hexdigest()
              + f':{g.host_os()}:END')
    settings = ''.join(f'    command.env("{key}", OsStr::from_bytes({literal(value)}));\n'
                       for key, value in context.items())
    return ('''use std::ffi::OsStr;
use std::os::unix::{ffi::OsStrExt, process::CommandExt};
use std::process::Command;
fn main() {
'''
            # Keep the producer in the transport's bytes: equal native code for
            # different retained images must never alias a build receipt.
            + f'    std::hint::black_box({literal(str(binary))});\n'
            + f'    std::hint::black_box({literal(marker)});\n'
            + f'    let mut command = Command::new(OsStr::from_bytes({literal(g.sys.executable)}));\n'
            + settings
            + '    if std::env::var_os("CARGO_TARGET_DIR").is_none_or(|v| v.is_empty()) {\n'
            + f'        command.env("CARGO_TARGET_DIR", OsStr::from_bytes({literal(str(target))}));\n'
            + '    }\n'
            + f'    command.arg(OsStr::from_bytes({literal(str(Path(g.__file__).resolve()))}));\n'
            + '''    command.arg("exec-argv0");
    command.arg(std::env::current_exe().expect("identity launcher path"));
    command.args(std::env::args_os());
    let _error = command.exec();
    eprintln!("candidate identity: native argument transport failed");
    std::process::exit(86);
}
''').encode()


def launcher_bytes(g, binary, source, head, target):
    code = launcher_source(g, binary, source, head, target)
    recipe = hashlib.sha256(code).hexdigest()

    def compile_transport():
        # The same pinned native compiler already produced identity.o. This
        # bootstrap transport, like identity.o, is generated by the harness.
        bindings = json.loads(g.read_bytes(target / 'identity-context.json'))
        compiler = Path(bindings['RMUX_IDENTITY_RUSTC'])
        compiler_sha = g.file_hash(compiler)
        g.file_hash(Path(__file__).resolve())
        g.file_hash(Path(g.__file__).resolve())
        directory = target / 'identity-native' / recipe
        directory.mkdir(parents=True, exist_ok=True, mode=0o700)
        if directory.resolve() != directory:
            raise g.IdentityError('redirected native argument transport store')
        input_path = directory / 'launcher.rs'
        _publish(g, input_path, code)
        with tempfile.TemporaryDirectory(dir=directory) as temporary:
            output = Path(temporary) / 'executable'
            args = [str(compiler), '--crate-name', 'identity_argv0', '--crate-type', 'bin',
                    '--edition=2021', '-C', 'opt-level=s', '-C', 'strip=debuginfo',
                    str(input_path), '-o', str(output)]
            subprocess.run(args, check=True, env={'PATH': os.defpath}, **g.jobserver())
            data = output.read_bytes()
        retained = g.support('candidate_identity_retention').retain(
            g, _publish(g, directory / 'executable', data), target, hashlib.sha256(data).hexdigest())
        g.read_bytes(input_path)
        record = g.verify(retained, source, head, target)
        record.update(kind='unix-argument-transport', input=str(input_path), input_sha256=recipe,
                      generator=str(Path(__file__).resolve()), generator_sha256=g.file_hash(__file__),
                      compiler=str(compiler), compiler_sha256=compiler_sha, arguments=args, exit=0)
        observation = g.support('candidate_identity_units').preserve(
            g, target / 'identity-native-builds', record)
        g.read_bytes(observation)
        record['observation'] = str(observation)
        return record

    record = g.support('candidate_identity_cache').cached(
        g, 'argv0-' + recipe, binary, source, head, target, compile_transport)
    data = g.read_bytes(Path(record['path']))
    if hashlib.sha256(data).hexdigest() != record['sha256']:
        raise g.IdentityError('native argument transport image changed')
    return data


def _publish(g, path, data):
    # Publishing by link keeps concurrent selectors from observing partial bytes.
    with tempfile.NamedTemporaryFile(dir=path.parent) as temporary:
        temporary.write(data)
        temporary.flush()
        os.fchmod(temporary.fileno(), 0o500)
        try:
            os.link(temporary.name, path)
        except FileExistsError:
            pass
        if path.is_symlink() or path.read_bytes() != data:
            raise g.IdentityError('native argument transport compiler input or output changed')
    return path
