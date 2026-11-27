"""The metadata wrapper must bind compiler inputs, not merely label its output."""
import importlib.util
import hashlib
import json
import os
import shlex
import shutil
import sys
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


@unittest.skipIf(os.name == 'nt', 'Unix daemon selection regression')
class SelectedDaemonRetention(unittest.TestCase):
    def test_selected_fixture_with_inherited_gate_runners(self):
        """A fresh child fixture must survive a parent gate's native runners."""
        inherited = {
            'CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER': 'parent-linux-runner',
            'CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER': 'parent-macos-arm-runner',
            'CARGO_TARGET_X86_64_APPLE_DARWIN_RUNNER': 'parent-macos-intel-runner',
            'CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUNNER': 'parent-windows-runner',
        }
        with patch.dict(os.environ, inherited):
            self.test_selected_image_survives_recompile_and_each_consumer_is_rechecked()
            for key, value in inherited.items():
                self.assertEqual(os.environ[key], value, 'parent gate environment changed')

    def test_selected_image_survives_recompile_and_each_consumer_is_rechecked(self):
        """Real compiler/selection/spawn A, overwrite with B, then verify A's bytes."""
        script = Path(__file__).with_name('candidate_identity.py').resolve()
        spec = importlib.util.spec_from_file_location('identity', script)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        audit = script.parent.parent / '.rmux-audit' / 's2-034' / 'acceptance'
        audit.mkdir(parents=True, exist_ok=True)
        source = Path(tempfile.mkdtemp(prefix='selected-daemon-', dir=audit)).resolve()
        (source / '.gitignore').write_text('.rmux-audit/\n')
        (source / 'Cargo.toml').write_text('[package]\nname="retention"\nversion="0.1.0"\nedition="2021"\n')
        (source / 'Cargo.lock').write_text('version = 4\n')
        code = source / 'main.rs'
        code.write_text('extern crate retention_dependency;\nfn main() { println!("{} {} {}", if cfg!(first) { "A" } else { "B" }, std::process::id(), retention_dependency::value()); }\n')
        library_code = source / 'library.rs'
        library_code.write_text('pub fn value() -> &\'static str { if cfg!(first) { "A" } else { "B" } }\n')
        git = ['git', '-C', str(source)]
        subprocess.run(git + ['init', '-q'], check=True)
        subprocess.run(git + ['add', '.'], check=True)
        subprocess.run(git + ['-c', 'user.name=Identity Test', '-c',
                       'user.email=identity@example.invalid', 'commit', '-qm', 'fixture'], check=True)
        head = guard.git(source, 'rev-parse', 'HEAD')
        rustc = Path(os.environ.get('RMUX_RETENTION_TEST_RUSTC',
                     Path.home() / '.rustup/toolchains/1.96.1-aarch64-apple-darwin/bin/rustc'))
        if not rustc.is_file():
            import shutil
            rustc = Path(shutil.which('rustc') or '')
        self.assertTrue(rustc.is_file(), 'native rustc required')
        env = dict(os.environ, RUSTC=str(rustc), RUSTDOC=str(rustc.with_name('rustdoc')))
        for key in tuple(env):
            if (key.startswith('RMUX_IDENTITY_')
                    or (key.startswith('CARGO_TARGET_') and key.endswith('_RUNNER'))
                    or key in ('CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER')):
                env.pop(key)
        previous = Path.cwd()
        try:
            os.chdir(source)
            with patch.dict(os.environ, env, clear=True):
                qualified = guard.prepare()
        finally:
            os.chdir(previous)
        qualified['CARGO_MANIFEST_DIR'] = str(source)
        target = Path(qualified['RMUX_IDENTITY_TARGET'])
        binary = target / 'debug' / 'rmux'
        binary.parent.mkdir()
        library = binary.with_name('libretention_dependency.rlib')
        library_args = [str(rustc), '--crate-name', 'retention_dependency', '--crate-type', 'rlib',
                        str(library_code), '-o', str(library)]
        external = ['--extern', 'retention_dependency=' + str(library)]
        base = [str(rustc), '--crate-name', 'retention', '--crate-type', 'bin', str(code), '-o', str(binary), *external]
        def compile_image(first):
            with patch.dict(os.environ, qualified, clear=True):
                flags = ['--cfg', 'first'] if first else []
                self.assertEqual(guard.rustc_wrapper(library_args + flags), 0)
                self.assertEqual(guard.rustc_wrapper(base + flags), 0)
        def select_and_spawn():
            selected = subprocess.check_output([qualified['RMUX_IDENTITY_PYTHON'], str(script),
                                               'check', str(binary)], env=qualified, text=True).strip()
            output = subprocess.check_output([qualified['RMUX_IDENTITY_PYTHON'], str(script),
                                             'exec', selected], env=qualified, text=True).strip()
            return Path(selected), output
        compile_image(True)
        image_a = binary.read_bytes()
        library_a = library.read_bytes()
        selected_a, output_a = select_and_spawn()
        self.assertEqual(output_a.split()[0], 'A')
        self.assertEqual(output_a.split()[2], 'A')
        compile_image(False)
        self.assertNotEqual(binary.read_bytes(), image_a)
        selected_b, output_b = select_and_spawn()
        self.assertEqual(output_b.split()[0], 'B')
        self.assertEqual(output_b.split()[2], 'B')
        self.assertNotEqual(library.read_bytes(), library_a)
        self.assertNotEqual(output_a.split()[1], output_b.split()[1])
        # This is the 033 loss: both debug/rmux and its compiler output now hold B.
        selected_record = guard.verify_built(selected_a, source, head, target)
        first = selected_record.get('producer', selected_record)
        self.assertEqual(Path(first['path']).read_bytes(), image_a,
                         'selected A was lost when the same compiler path became B')
        self.assertEqual(first['sha256'], hashlib.sha256(image_a).hexdigest())
        self.assertEqual(first['commit'], head)
        self.assertTrue(first['unit']['graph_sha256'])
        dependency_a = json.loads(Path(first['unit']['receipt']).read_text())['graph']['dependencies'][0]
        self.assertEqual(Path(dependency_a['path']).read_bytes(), library_a,
                         'linked producer A was lost when its compiler path became B')
        self.assertEqual(dependency_a['sha256'], hashlib.sha256(library_a).hexdigest())
        replay_a = subprocess.check_output([str(selected_a)], env={'PATH': os.defpath}, text=True).strip()
        self.assertEqual(replay_a.split()[0::2], ['A', 'A'])
        # A rustup/snap rustc links to a multi-call proxy; the record names the binary it runs.
        sysroot = subprocess.check_output([str(rustc), '--print', 'sysroot'], text=True).strip()
        self.assertEqual(json.loads(Path(first['build']).read_text())['compiler'],
                         str((Path(sysroot) / 'bin' / rustc.name).resolve(strict=True)))
        launches = [json.loads(p.read_text()) for p in (target / 'identity-launches').glob('*.json')]
        for output, expected in ((output_a, first['sha256']), (replay_a, first['sha256']),
                                 (output_b, guard.file_hash(binary))):
            consumers = [row for row in launches if row.get('event') == 'exec'
                         and row['executables'][0]['sha256'] == expected
                         and row['consumer']['pid'] == int(output.split()[1])]
            self.assertEqual(len(consumers), 1, 'each actual image/PID needs its own before-exec receipt')
            self.assertTrue(consumers[0]['consumer']['birth'])
        # A selected launcher also protects direct SDK spawns after env_clear.
        direct = subprocess.check_output([str(selected_b)], env={'PATH': os.defpath}, text=True).strip()
        self.assertEqual(direct.split()[0], 'B')
        self.assertNotEqual(direct.split()[1], output_b.split()[1])
        # Cargo build-script wrappers spawn a different native producer PID.
        build_path = target / 'build-script' / 'build_script_build'
        build_path.parent.mkdir()
        generated = target / 'generated-output'
        generated.mkdir()
        build_env = dict(qualified, OUT_DIR=str(generated))
        build_args = [str(rustc), '--crate-name', 'build_script_build', '--crate-type', 'bin',
                      str(code), '-o', str(build_path), *external]
        build_consumers = []
        for first_build in (True, False):
            with patch.dict(os.environ, build_env, clear=True):
                self.assertEqual(guard.rustc_wrapper(
                    build_args + (['--cfg', 'first'] if first_build else [])), 0)
            output = subprocess.check_output([str(build_path)], env=build_env, text=True).strip()
            self.assertEqual(output.split()[0], 'A' if first_build else 'B')
            rows = [json.loads(p.read_text()) for p in (target / 'identity-launches').glob('*.json')]
            matched = [row for row in rows if row.get('event') == 'exec'
                       and row['consumer']['pid'] == int(output.split()[1])
                       and row['executables'][0].get('unit', {}).get('kind') == 'build-script']
            self.assertEqual(len(matched), 1, 'actual build-script child needs its own before-exec receipt')
            build_consumers.append(matched[0]['executables'][0])
        self.assertNotEqual(build_consumers[0]['sha256'], build_consumers[1]['sha256'])
        self.assertEqual(guard.file_hash(build_consumers[0]['path']), build_consumers[0]['sha256'])
        dirty = code.read_text()
        code.write_text(dirty + '// uncommitted source\n')
        refused_source = subprocess.run([str(selected_a)], env={'PATH': os.defpath},
                                        capture_output=True, text=True)
        self.assertEqual(refused_source.returncode, 86)
        self.assertEqual(refused_source.stdout, '')
        code.write_text(dirty)
        # A successful first PID never permits a later spawn with corrupted bytes.
        native_b = Path(guard.verify_built(selected_b, source, head, target)['producer']['path'])
        native_b.chmod(0o755)
        native_b.write_bytes(b'corrupted after first consumer')
        rejected = subprocess.run([qualified['RMUX_IDENTITY_PYTHON'], str(script), 'exec', str(selected_b)],
                                  env=qualified, capture_output=True, text=True)
        self.assertEqual(rejected.returncode, 86)
        self.assertEqual(rejected.stdout, '')
        linked_a = Path(dependency_a['path'])
        linked_a.chmod(0o700)
        linked_a.write_bytes(b'corrupted linked producer after first consumer')
        rejected_dependency = subprocess.run([str(selected_a)], env={'PATH': os.defpath},
                                             capture_output=True, text=True)
        self.assertEqual(rejected_dependency.returncode, 86)
        self.assertEqual(rejected_dependency.stdout, '')


class CandidateBuildIdentity(unittest.TestCase):
    def test_reject_other_source_or_output_before_compiler(self):
        path = Path(__file__).with_name('candidate_identity.py')
        spec = importlib.util.spec_from_file_location('candidate_identity', path)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source = root / 'candidate'
            source.mkdir()
            crate = source / 'main.rs'
            crate.write_text('fn main() {}\n')
            subprocess.run(['git', '-C', str(source), 'init', '-q'], check=True)
            subprocess.run(['git', '-C', str(source), 'add', 'main.rs'], check=True)
            subprocess.run(['git', '-C', str(source), '-c', 'user.name=Identity Test',
                            '-c', 'user.email=identity@example.invalid', 'commit',
                            '-qm', 'candidate'], check=True)
            head = guard.git(source, 'rev-parse', 'HEAD')
            target = guard.target_directory(source, head, '0' * 32)
            target.mkdir(parents=True)
            (target / 'identity.o').write_bytes(b'compiler mock input')
            base = root / 'base.rs'
            base.write_text('fn main() {}\n')
            env = dict(RMUX_IDENTITY_SOURCE=str(source), RMUX_IDENTITY_HEAD=head,
                       RMUX_IDENTITY_TARGET=str(target),
                       CARGO_TARGET_DIR=str(target), CARGO_MANIFEST_DIR=str(source))
            for defect in ('foreign-source', 'foreign-output', 'foreign-manifest', 'stdin'):
                with self.subTest(defect=defect), patch.dict(os.environ, env, clear=True):
                    input_path = base if defect == 'foreign-source' else crate
                    output = root / 'shared' if defect == 'foreign-output' else target / 'probe'
                    if defect == 'foreign-manifest':
                        os.environ['CARGO_MANIFEST_DIR'] = str(root)
                    args = ['rustc', '--crate-type', 'bin',
                            '-' if defect == 'stdin' else str(input_path), '-o', str(output)]
                    with patch.object(guard.subprocess, 'call') as compiler:
                        with self.assertRaises(guard.IdentityError):
                            guard.rustc_wrapper(args)
                        compiler.assert_not_called()


SCRIPT = Path(__file__).with_name('candidate_identity.py').resolve()
ROOT = SCRIPT.parent.parent
PROBE = '''
import importlib.util, json, os, sys
from pathlib import Path
spec = importlib.util.spec_from_file_location('guard', sys.argv[1])
g = importlib.util.module_from_spec(spec)
spec.loader.exec_module(g)
read = Path.read_bytes
blocked = {Path(p) for p in json.loads(os.environ.get('SELECTION_BLOCK_READS', '[]'))}
def checked_read(path):
    if path in blocked:
        Path(os.environ['SELECTION_READ_FAILURE']).write_text(str(path))
        raise AssertionError('unchanged producer requalified after alias selection: ' + str(path))
    return read(path)
Path.read_bytes = checked_read
sys.argv = [sys.argv[1], *sys.argv[2:]]
sys.exit(g.main())
'''


@unittest.skipIf(os.name == 'nt', 'Unix retained launcher selection')
class SelectedProgramStartup(unittest.TestCase):
    def test_cold_input_reuse_rechecks_bytes_with_equal_stamps(self):
        """A same-size write may share the native clock tick of its witness."""
        from types import SimpleNamespace
        spec = importlib.util.spec_from_file_location('guard', SCRIPT)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        audit = ROOT / '.rmux-audit' / 'selected-program-inputs'
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as temporary:
            source = Path(temporary).resolve()
            code = source / 'main.rs'
            code.write_text('fn main() {}\n')
            git = [shutil.which('git'), '-C', str(source)]
            for args in (['init', '-q'], ['add', '.'], ['-c', 'user.name=Identity Test',
                         '-c', 'user.email=identity@example.invalid', 'commit', '-qm', 'fixture']):
                subprocess.run(git + args, check=True)
            head = guard.git(source, 'rev-parse', 'HEAD')
            stamp = guard.stamp
            with patch.object(guard, 'stamp', lambda info: (*stamp(info)[:4], 1, 1)):
                g = guard.support('candidate_identity_qualification').scope(
                    SimpleNamespace(**vars(guard)))
                g.tracked_input(code, source, head)
                code.write_text('fn main() { }')
                with self.assertRaises(guard.IdentityError):
                    g.tracked_input(code, source, head)

    def test_cold_qualification_reuses_pinned_input_checks(self):
        """Shared lock roots cost one Git proof each within a cold graph walk."""
        from types import SimpleNamespace
        spec = importlib.util.spec_from_file_location('guard', SCRIPT)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        audit = ROOT / '.rmux-audit' / 'selected-program-inputs'
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as temporary:
            source = Path(temporary).resolve()
            manifest, lock = source / 'Cargo.toml', source / 'Cargo.lock'
            manifest.write_text('[workspace]\n')
            lock.write_text('version = 4\n')
            git = [shutil.which('git'), '-C', str(source)]
            for args in (['init', '-q'], ['add', '.'], ['-c', 'user.name=Identity Test',
                         '-c', 'user.email=identity@example.invalid', 'commit', '-qm', 'fixture']):
                subprocess.run(git + args, check=True)
            head = guard.git(source, 'rev-parse', 'HEAD')
            # These are real Git comparisons, counted across repeated graph roots.
            for consumer in range(2):
                with patch.object(guard, 'tracked_input', wraps=guard.tracked_input) as checks:
                    g = guard.support('candidate_identity_qualification').scope(
                        SimpleNamespace(**vars(guard)))
                    manifests = guard.support('candidate_identity_manifest')
                    first = manifests.root_binding(g, source, head)
                    for _ in range(3):
                        self.assertEqual(manifests.root_binding(g, source, head), first)
                    g._qualification.validate()
                    self.assertEqual(checks.call_count, 2,
                                     f'redundant pinned-input work in cold consumer {consumer}')
                # Reuse is limited to this exact source/head and unchanged bytes.
                with self.assertRaises(guard.IdentityError):
                    g.tracked_input(lock, source, '0' * 40)
                with self.assertRaises(guard.IdentityError):
                    g.tracked_input(lock, source / 'foreign', head)
                data, info = lock.read_bytes(), lock.stat()
                lock.write_bytes(b'version = 3\n')
                os.utime(lock, ns=(info.st_atime_ns, info.st_mtime_ns))
                with self.assertRaises(guard.IdentityError):
                    g.tracked_input(lock, source, head)
                with self.assertRaises(guard.IdentityError):
                    g._qualification.validate()
                fresh = guard.support('candidate_identity_qualification').scope(
                    SimpleNamespace(**vars(guard)))
                with self.assertRaises(guard.IdentityError):
                    fresh.tracked_input(lock, source, head)
                lock.write_bytes(data)

    def test_alias_selection_preserves_warm_producer_proof(self):
        spec = importlib.util.spec_from_file_location('guard', SCRIPT)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        audit = ROOT / '.rmux-audit' / 's2-086'
        audit.mkdir(parents=True, exist_ok=True)
        source = Path(tempfile.mkdtemp(prefix='selection-', dir=audit)).resolve()
        (source / '.gitignore').write_text('.rmux-audit/\n')
        (source / 'Cargo.toml').write_text('[package]\nname="selection"\nversion="0.1.0"\n')
        (source / 'Cargo.lock').write_text('version = 4\n')
        code = source / 'main.rs'
        code.write_text('mod selected_program_impl;\n'
                        'fn main() { for program in std::env::args_os().skip(1) {\n'
                        'let path = selected_program_impl::resolve(&program).expect("verified selection");\n'
                        'println!("{}", path.display()); } }\n')
        (source / 'selected_program_impl.rs').write_bytes(
            (ROOT / 'crates/rmux-os/src/build_identity/selected_program.rs').read_bytes())
        git = [shutil.which('git'), '-C', str(source)]
        for args in (['init', '-q'], ['add', '.'], ['-c', 'user.name=Identity Test',
                     '-c', 'user.email=identity@example.invalid', 'commit', '-qm', 'fixture']):
            subprocess.run(git + args, check=True)
        rustc = Path(os.environ.get('RMUX_RETENTION_TEST_RUSTC') or subprocess.check_output(
            [shutil.which('rustup'), 'which', 'rustc'], text=True).strip()).resolve()
        env = dict(os.environ, RUSTC=str(rustc), RUSTDOC=str(rustc.with_name('rustdoc')))
        for key in tuple(env):
            if (key.startswith('RMUX_IDENTITY_')
                    or (key.startswith('CARGO_TARGET_') and key.endswith('_RUNNER'))
                    or key in ('CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER')):
                env.pop(key)
        previous = Path.cwd()
        try:
            os.chdir(source)
            with patch.dict(os.environ, env, clear=True):
                env = guard.prepare()
        finally:
            os.chdir(previous)
        target = Path(env['RMUX_IDENTITY_TARGET'])
        probe = target / 'verifier.py'
        probe.write_text(PROBE.replace('sys.argv[1]', repr(str(SCRIPT)), 2)
                         .replace('sys.argv[2:]', 'sys.argv[1:]'))
        env.update(CARGO_MANIFEST_DIR=str(source), RMUX_IDENTITY_SCRIPT=str(probe))
        binary = target / 'selection'
        with patch.dict(os.environ, env, clear=True):
            self.assertEqual(guard.rustc_wrapper([str(rustc), '--crate-name', 'selection',
                             '--crate-type', 'bin', str(code), '-o', str(binary)]), 0)
            record = guard.verify_built(binary, source, env['RMUX_IDENTITY_HEAD'], target)
        retained = Path(json.loads(Path(record['build']).read_text())['path'])
        # The actual Rust resolve invokes the actual Python check/select path.
        warm = subprocess.run([str(retained), str(binary), str(retained)],
                              env=env, capture_output=True, text=True)
        self.assertEqual(warm.returncode, 0, warm.stderr)
        selected = warm.stdout.splitlines()
        self.assertEqual(len(set(selected)), 1, 'aliases must select the same immutable launcher')
        env['SELECTION_BLOCK_READS'] = json.dumps([str(binary), str(retained), str(rustc)])
        failure = target / 'unexpected-read'
        env['SELECTION_READ_FAILURE'] = str(failure)
        hot = subprocess.run([str(retained), str(binary), str(retained), str(binary)],
                             env=env, capture_output=True, text=True)
        self.assertEqual(hot.returncode, 0, hot.stderr + (failure.read_text() if failure.exists() else ''))
        self.assertEqual(hot.stdout.splitlines(), selected[:1] * 3)
        # Warm proofs still fail closed when either the alias or retained bytes change.
        env.pop('SELECTION_BLOCK_READS')
        original = binary.read_bytes()
        binary.chmod(0o700)
        binary.write_bytes(b'foreign executable')
        rejected = subprocess.run([str(retained), str(binary)], env=env,
                                  capture_output=True, text=True)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertEqual(rejected.stdout, '')
        binary.write_bytes(original)
        retained.chmod(0o700)
        retained.write_bytes(b'replaced retained producer')
        rejected = subprocess.run([sys.executable, str(SCRIPT), 'check', selected[0]],
                                  env=env, capture_output=True, text=True)
        self.assertEqual(rejected.returncode, 86)
        self.assertEqual(rejected.stdout, '')



@unittest.skipIf(os.name == 'nt', 'Unix compiler links')
class MultiCallCompilerProxy(unittest.TestCase):
    def test_proxy_link_binds_the_toolchain_compiler_it_runs(self):
        """rustup/snap link rustc to one multi-call proxy, and Cargo bypasses that proxy."""
        script = Path(__file__).with_name('candidate_identity.py').resolve()
        spec = importlib.util.spec_from_file_location('identity', script)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        audit = script.parent.parent / '.rmux-audit' / 'compiler-proxy' / 'acceptance'
        audit.mkdir(parents=True, exist_ok=True)
        source = Path(tempfile.mkdtemp(prefix='source-', dir=audit)).resolve()
        (source / '.gitignore').write_text('.rmux-audit/\n')
        (source / 'Cargo.toml').write_text('[package]\nname="proxy"\nversion="0.1.0"\nedition="2021"\n')
        (source / 'Cargo.lock').write_text('version = 4\n')
        code = source / 'main.rs'
        code.write_text('fn main() { println!("{}", cfg!(second)); }\n')
        git = ['git', '-C', str(source)]
        for args in (['init', '-q'], ['add', '.'], ['-c', 'user.name=Identity Test',
                     '-c', 'user.email=identity@example.invalid', 'commit', '-qm', 'fixture']):
            subprocess.run(git + args, check=True)
        selected = os.environ.get('RMUX_RETENTION_TEST_RUSTC') or shutil.which('rustc')
        sysroot = subprocess.check_output([selected, '--print', 'sysroot'], text=True).strip()
        toolchain = (Path(sysroot) / 'bin' / 'rustc').resolve(strict=True)
        # Like /snap/bin/rustc -> /usr/bin/snap: the link target dispatches on argv[0].
        links = Path(tempfile.mkdtemp(prefix='links-', dir=audit)).resolve()
        (links / 'multicall').write_text(
            '#!/bin/sh\ncase "${0##*/}" in rustc) exec ' + shlex.quote(str(toolchain))
            + ' "$@";; esac\necho "multicall: unknown applet" >&2\nexit 64\n')
        (links / 'multicall').chmod(0o755)
        (links / 'rustc').symlink_to('multicall')
        env = {key: value for key, value in os.environ.items()
               if not (key.startswith('RMUX_IDENTITY_')
                       or (key.startswith('CARGO_TARGET_') and key.endswith('_RUNNER'))
                       or key in ('CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER'))}
        env.update(RUSTC=str(links / 'rustc'), RUSTDOC=str(toolchain.with_name('rustdoc')))
        previous = Path.cwd()
        try:
            os.chdir(source)
            with patch.dict(os.environ, env, clear=True):
                qualified = guard.prepare()
        finally:
            os.chdir(previous)
        self.assertEqual(qualified['RMUX_IDENTITY_RUSTC'], str(toolchain))
        qualified['CARGO_MANIFEST_DIR'] = str(source)
        target = Path(qualified['RMUX_IDENTITY_TARGET'])
        # Cargo invokes the toolchain binary; an explicit RUSTC keeps the proxy link.
        for index, compiler in enumerate((toolchain, links / 'rustc')):
            binary = target / f'proxy-{index}'
            flags = ['--cfg', 'second'] if index else []
            with patch.dict(os.environ, qualified, clear=True):
                self.assertEqual(guard.rustc_wrapper([str(compiler), '--crate-name', 'proxy', '--crate-type',
                                                      'bin', str(code), '-o', str(binary), *flags]), 0)
                record = guard.verify_built(binary, source, qualified['RMUX_IDENTITY_HEAD'], target)
            self.assertEqual(json.loads(Path(record['build']).read_text())['compiler'], str(toolchain))
        # Without RUSTC, Cargo passes a bare `rustc`: a PATH lookup, never a file in the
        # working directory, whether PATH names the toolchain binary or its proxy link.
        decoy = Path(tempfile.mkdtemp(prefix='cwd-', dir=audit)).resolve()
        (decoy / 'rustc').write_text('#!/bin/sh\nexit 97\n')
        (decoy / 'rustc').chmod(0o755)
        for index, (directory, cwd) in enumerate(((toolchain.parent, source), (links, decoy)), start=2):
            binary = target / f'proxy-{index}'
            try:
                os.chdir(cwd)
                with patch.dict(os.environ, dict(qualified, PATH=f'{directory}{os.pathsep}{qualified["PATH"]}'),
                                clear=True):
                    self.assertEqual(guard.rustc_wrapper(['rustc', '--crate-name', 'proxy', '--crate-type',
                                                          'bin', str(code), '-o', str(binary)]), 0)
                    record = guard.verify_built(binary, source, qualified['RMUX_IDENTITY_HEAD'], target)
            finally:
                os.chdir(previous)
            self.assertEqual(json.loads(Path(record['build']).read_text())['compiler'], str(toolchain))
        # An ordinary compiler, or an ordinary link to it, keeps its selected spelling.
        (links / 'ordinary').mkdir()
        (links / 'ordinary' / 'rustc').symlink_to(toolchain)
        unix = guard.support('candidate_identity_unix')
        for ordinary in (str(toolchain), str(links / 'ordinary' / 'rustc')):
            self.assertEqual(unix.selected_compiler(ordinary), ordinary)

@unittest.skipIf(os.name == 'nt', 'Portable fixture of the Windows proxy layout')
class HardLinkedCompilerProxy(unittest.TestCase):
    def test_windows_prepare_binds_the_toolchain_behind_a_hard_linked_proxy(self):
        """rustup on Windows hard-links rustc.exe to its proxy; Cargo runs the toolchain binary."""
        script = Path(__file__).with_name('candidate_identity.py').resolve()
        spec = importlib.util.spec_from_file_location('identity', script)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        guard.os = SimpleNamespace(**vars(os))
        guard.os.name = 'nt'
        guard.host_os = lambda: 'windows'
        audit = script.parent.parent / '.rmux-audit' / 'compiler-proxy' / 'hard-link'
        audit.mkdir(parents=True, exist_ok=True)
        source = Path(tempfile.mkdtemp(prefix='source-', dir=audit)).resolve()
        (source / '.gitignore').write_text('.rmux-audit/\n')
        (source / 'Cargo.lock').write_text('version = 4\n')
        git = ['git', '-C', str(source)]
        for args in (['init', '-q'], ['add', '.'], ['-c', 'user.name=Identity Test',
                     '-c', 'user.email=identity@example.invalid', 'commit', '-qm', 'fixture']):
            subprocess.run(git + args, check=True)
        selected = os.environ.get('RMUX_RETENTION_TEST_RUSTC') or shutil.which('rustc')
        sysroot = subprocess.check_output([selected, '--print', 'sysroot'], text=True).strip()
        toolchain = (Path(sysroot) / 'bin' / 'rustc').resolve(strict=True)
        links = Path(tempfile.mkdtemp(prefix='links-', dir=audit)).resolve()
        (links / 'proxy').write_text(
            '#!/bin/sh\ncase "${0##*/}" in rustc) exec ' + shlex.quote(str(toolchain))
            + ' "$@";; esac\necho "proxy: unknown applet" >&2\nexit 64\n')
        (links / 'proxy').chmod(0o755)
        proxy = links / 'rustc'
        os.link(links / 'proxy', proxy)
        # No path comparison separates a hard-linked proxy from a compiler.
        self.assertEqual(proxy.resolve(strict=True), proxy)
        env = {key: value for key, value in os.environ.items()
               if not (key.startswith('RMUX_IDENTITY_')
                       or (key.startswith('CARGO_TARGET_') and key.endswith('_RUNNER'))
                       or key in ('CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER'))}
        env.update(RUSTC=str(proxy), RUSTDOC=str(toolchain.with_name('rustdoc')))
        # The native wrapper build is the Windows host's proof; this fixture qualifies the selection.
        native = guard.support('candidate_identity_windows')
        previous = Path.cwd()
        try:
            os.chdir(source)
            with patch.dict(os.environ, env, clear=True), patch.object(native, 'install'), \
                    patch.object(native, 'validate'):
                qualified = guard.prepare()
                self.assertEqual(qualified['RMUX_IDENTITY_RUSTC'], str(toolchain))
                # validate_context() asks the same selection, so the bound context holds.
                with patch.dict(os.environ, qualified, clear=True):
                    guard.validate_context()
        finally:
            os.chdir(previous)
        # A compiler that is its own sysroot binary keeps its spelling.
        self.assertEqual(native.selected_compiler(str(toolchain)), str(toolchain))
        # An explicit RUSTC naming the proxy reaches the wrapper verbatim.
        self.assertEqual(native.invoked_compiler(str(proxy), str(toolchain)), str(toolchain))
        self.assertEqual(native.invoked_compiler(str(toolchain), str(toolchain)), str(toolchain))
        self.assertEqual(native.invoked_compiler('rustc', str(toolchain)), 'rustc')


if __name__ == '__main__':
    unittest.main()
