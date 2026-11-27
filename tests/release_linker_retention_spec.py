"""Exercise build.rs retention through native Cargo and the glibc-floor linker."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
from candidate_identity import PRIVATE_ROOT  # noqa: E402
MARKER = re.compile(rb'RMUX_RELEASE_IDENTITY_V1:([0-9a-f]{40}):END')


class ReleaseLinkerRetention(unittest.TestCase):
    def setUp(self):
        audit = ROOT / 'target' / 'tmp' / 'release-linker-retention'
        audit.mkdir(parents=True, exist_ok=True)
        self.source = Path(tempfile.mkdtemp(dir=audit)).resolve()
        self.logs = self.source / PRIVATE_ROOT / 'receipts'
        self.logs.mkdir(parents=True)
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith(('RMUX_IDENTITY_', 'CARGO_PROFILE_', 'CARGO_TARGET_', 'ZIG_'))
                    and k not in ('CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER',
                                  'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_MANIFEST_DIR',
                                  'RMUX_LINUX_GLIBC_FLOOR', 'CARGO_ZIGBUILD_CACHE_DIR')}
        self.env['CARGO_INCREMENTAL'] = '0'
        self.env['CARGO_BUILD_JOBS'] = '6'
        self.cargo = shutil.which('cargo', path=self.env['PATH'])
        self.rustc = shutil.which('rustc', path=self.env['PATH'])
        self.assertIsNotNone(self.cargo)
        self.assertIsNotNone(self.rustc)
        self.env['RUSTC'] = self.rustc
        self.waits = []
        (self.source / '.gitignore').write_text(PRIVATE_ROOT + '/\n')
        (self.source / 'Cargo.toml').write_text(
            '[package]\nname="rmux"\nversion="0.1.0"\nedition="2021"\n'
            '[build-dependencies]\nembed-manifest={path="embed"}\n'
            '[[bin]]\nname="rmux"\npath="main.rs"\n'
            '[[bin]]\nname="rmux-daemon"\npath="main.rs"\n[workspace]\n')
        (self.source / 'embed/src').mkdir(parents=True)
        (self.source / 'embed/Cargo.toml').write_text(
            '[package]\nname="embed-manifest"\nversion="1.4.0"\nedition="2021"\n')
        (self.source / 'embed/src/lib.rs').write_text(
            'pub fn embed_manifest_file(_: &str) -> std::io::Result<()> { Ok(()) }\n')
        (self.source / 'main.rs').write_text('fn main() {}\n')
        shutil.copyfile(ROOT / 'build.rs', self.source / 'build.rs')
        # build.rs watches this path on every target; a missing one keeps rmux stale.
        manifest = self.source / 'resources/windows/rmux.exe.manifest'
        manifest.parent.mkdir(parents=True)
        shutil.copyfile(ROOT / 'resources/windows/rmux.exe.manifest', manifest)
        self.run_command([self.cargo, 'generate-lockfile', '--offline'], 'lock')
        for args in [('init', '-q'), ('add', '.'),
                     ('-c', 'user.name=Identity Test', '-c', 'user.email=identity@example.invalid',
                      'commit', '-qm', 'retention fixture')]:
            self.run_command(['git', *args], 'git-' + args[-1].replace(' ', '-'))
        self.head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=self.source,
                                            text=True).strip()
        rustc = subprocess.check_output([self.rustc, '-vV'], env=self.env, text=True)
        self.triple = next(line[6:] for line in rustc.splitlines() if line.startswith('host: '))

    def run_command(self, argv, label, env=None):
        start = time.time_ns()
        with (self.logs / (label + '.log')).open('wb') as log:
            child = subprocess.Popen(argv, cwd=self.source, env=env or self.env,
                                     stdout=log, stderr=subprocess.STDOUT)
            try:
                birth = Path('/proc', str(child.pid), 'stat').read_text()
            except OSError:
                birth = None
            code = child.wait()
        self.waits.append(dict(argv=argv, pid=child.pid, birth=birth, start_ns=start,
                               end_ns=time.time_ns(), exit=code, log=label + '.log'))
        (self.logs / 'original-waits.json').write_text(json.dumps(self.waits, indent=2))
        self.assertEqual(code, 0, (self.logs / (label + '.log')).read_text())

    def build_pair(self, zig=False):
        sys.path.insert(0, str(ROOT / 'scripts'))
        import candidate_identity
        target = candidate_identity.target_directory(self.source, self.head, uuid.uuid4().hex)
        self.assertFalse(target.exists())
        env = dict(self.env, CARGO_TARGET_DIR=str(target))
        if zig:
            env.update(RMUX_LINUX_GLIBC_FLOOR='2.31',
                       CARGO_ZIGBUILD_CACHE_DIR=str(target / 'zigbuild-cache'),
                       ZIG_LOCAL_CACHE_DIR=str(target / 'zig-local-cache'),
                       ZIG_GLOBAL_CACHE_DIR=str(target / 'zig-global-cache'))
            argv = ['/usr/bin/bash', str(ROOT / 'scripts/release/cargo-build-compatible.sh'),
                    '--target', self.triple, '--', 'build']
        else:
            argv = [self.cargo, 'build', '--target', self.triple]
        argv += ['--locked', '--offline', '--release', '--message-format=json-render-diagnostics']
        self.run_command(argv, target.name, env)
        pair = {}
        records = []
        for name in ('rmux', 'rmux-daemon'):
            path = target / self.triple / 'release' / (name + ('.exe' if os.name == 'nt' else ''))
            before = path.read_bytes()
            self.assertEqual(MARKER.findall(before), [self.head.encode()], str(path))
            self.assertNotRegex(before, rb'RMUX_BUILD_GRAPH_V[0-9]+:')
            self.assertNotIn(str(target).encode(), before)
            strip = shutil.which('llvm-strip' if os.name == 'nt' else 'strip', path=env['PATH'])
            self.assertIsNotNone(strip, 'native strip tool is required')
            self.run_command([strip, str(path)], target.name + '-' + name + '-strip', env)
            after = path.read_bytes()
            self.assertEqual(MARKER.findall(after), [self.head.encode()], 'strip must retain HEAD')
            pair[name] = after
            records.append(dict(path=str(path), head=self.head, fixture_source=str(self.source),
                                before_sha256=hashlib.sha256(before).hexdigest(),
                                stripped_sha256=hashlib.sha256(after).hexdigest()))
        (target / 'retention.json').write_text(json.dumps(records, indent=2))
        return pair

    def test_native_link_retains_exact_head_after_strip_and_is_reproducible(self):
        self.assertEqual(self.build_pair(), self.build_pair(), 'cold native bytes must reproduce')

    def build_debug(self, label):
        """Return (fresh, embedded commit) for a plain debug build of rmux."""
        target = self.source / PRIVATE_ROOT / 'rerun-target'
        log = self.logs / (label + '.log')
        self.run_command([self.cargo, 'build', '--locked', '--offline', '--bin', 'rmux',
                          '--message-format=json-render-diagnostics'], label,
                         dict(self.env, CARGO_TARGET_DIR=str(target),
                              CARGO_LOG='cargo::core::compiler::fingerprint=info'))
        artifacts = [json.loads(line) for line in log.read_text().splitlines()
                     if line.startswith('{"reason":"compiler-artifact"')]
        fresh = [item['fresh'] for item in artifacts if item['target']['name'] == 'rmux']
        self.assertEqual(len(fresh), 1, log.read_text())
        binary = target / 'debug' / ('rmux.exe' if os.name == 'nt' else 'rmux')
        return fresh[0], MARKER.findall(binary.read_bytes())

    def git(self, *args):
        self.run_command(['git', '-c', 'user.name=Identity Test',
                          '-c', 'user.email=identity@example.invalid', *args],
                         'git-%d' % len(self.waits))

    def rev(self, *args):
        return subprocess.check_output(['git', 'rev-parse', *args, 'HEAD'], cwd=self.source,
                                       text=True).strip()

    def assert_tracks_head_then_stays_fresh(self, label, head):
        fresh, marker = self.build_debug(label)
        self.assertFalse(fresh, label + ': a HEAD move must rebuild rmux')
        self.assertEqual(marker, [head.encode()], label)
        fresh, marker = self.build_debug(label + '-again')
        self.assertTrue(fresh, label + ': an unchanged second build must not recompile rmux')
        self.assertEqual(marker, [head.encode()], label)

    def test_unchanged_rebuild_stays_fresh_while_identity_follows_head(self):
        git_dir = self.source / '.git'
        self.assertFalse((git_dir / 'packed-refs').exists(), 'fresh init has no packed-refs')
        _, marker = self.build_debug('cold')
        self.assertEqual(marker, [self.head.encode()])
        fresh, marker = self.build_debug('cold-again')
        self.assertTrue(fresh, 'missing packed-refs must not keep rmux stale')
        (self.source / 'notes.txt').write_text('index churn\n')
        self.git('add', 'notes.txt')
        self.assertTrue(self.build_debug('index-only')[0], 'an index change is not a HEAD move')
        branch = self.rev('--abbrev-ref')
        self.git('pack-refs', '--all')
        self.assertFalse((git_dir / 'refs/heads' / branch).exists(), 'ref is packed only')
        _, marker = self.build_debug('packed')
        self.assertEqual(marker, [self.head.encode()], 'pack-refs keeps the same HEAD')
        self.assertTrue(self.build_debug('packed-again')[0],
                        'a packed-only branch ref must not keep rmux stale')
        self.git('commit', '-qm', 'packed ref becomes loose')
        second = self.rev()
        self.assert_tracks_head_then_stays_fresh('loose-commit', second)
        self.git('commit', '-q', '--allow-empty', '-m', 'loose ref moves')
        third = self.rev()
        self.assert_tracks_head_then_stays_fresh('loose-move', third)
        self.git('checkout', '-q', '-b', 'nested/older', self.head)
        self.assert_tracks_head_then_stays_fresh('checkout-new-branch', self.head)
        self.git('pack-refs', '--all')
        self.git('checkout', '-q', branch)
        self.assert_tracks_head_then_stays_fresh('checkout-packed-branch', third)
        self.git('checkout', '-q', 'nested/older')
        self.assertFalse((git_dir / 'refs/heads/nested').exists(), 'nested ref is packed only')
        self.assert_tracks_head_then_stays_fresh('checkout-nested-packed', self.head)
        self.git('commit', '-q', '--allow-empty', '-m', 'nested packed ref becomes loose')
        nested = self.rev()
        self.assert_tracks_head_then_stays_fresh('nested-commit', nested)
        self.git('checkout', '-q', '--detach', second)
        self.assert_tracks_head_then_stays_fresh('detached', second)

    def test_reftable_identity_follows_head_and_stays_fresh(self):
        shutil.rmtree(self.source / '.git')
        probe = subprocess.run(['git', 'init', '-q', '--ref-format=reftable'], cwd=self.source,
                               capture_output=True, text=True)
        if probe.returncode != 0:
            self.skipTest('this Git has no reftable backend')
        self.git('add', '.')
        self.git('commit', '-qm', 'reftable fixture')
        first = self.rev()
        self.assert_tracks_head_then_stays_fresh('reftable-cold', first)
        self.git('commit', '-q', '--allow-empty', '-m', 'reftable commit')
        second = self.rev()
        self.assert_tracks_head_then_stays_fresh('reftable-commit', second)
        self.git('checkout', '-q', '-b', 'older', first)
        self.assert_tracks_head_then_stays_fresh('reftable-checkout', first)

    @unittest.skipUnless(sys.platform.startswith('linux'), 'Linux GNU glibc-floor route')
    def test_zig_glibc231_link_retains_exact_head_after_strip_and_is_reproducible(self):
        self.assertIsNotNone(shutil.which('cargo-zigbuild', path=self.env['PATH']))
        self.assertEqual(self.build_pair(zig=True), self.build_pair(zig=True),
                         'cold Zig glibc2.31 bytes must reproduce')


if __name__ == '__main__':
    unittest.main()
