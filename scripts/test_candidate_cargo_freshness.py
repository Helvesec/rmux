"""A real guarded Cargo build must stay fresh across distinct test filters."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch


@unittest.skipIf(os.name == 'nt', 'Unix native fixture; Windows is verified at integration')
class CargoFreshness(unittest.TestCase):
    def test_cargo_depinfo_and_second_filter_reuse_the_guarded_image(self):
        script = Path(__file__).with_name('candidate_identity.py').resolve()
        spec = importlib.util.spec_from_file_location('freshness_identity', script)
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        audit = script.parent.parent / '.rmux-audit/cargo-freshness'
        audit.mkdir(parents=True, exist_ok=True)
        source = Path(tempfile.mkdtemp(prefix='native-', dir=audit)).resolve()
        (source / '.gitignore').write_text('.rmux-audit/\n')
        (source / 'Cargo.toml').write_text(
            '[package]\nname="identity-freshness"\nversion="0.1.0"\nedition="2021"\n[workspace]\n')
        (source / 'Cargo.lock').write_text(
            'version = 4\n[[package]]\nname = "identity-freshness"\nversion = "0.1.0"\n')
        (source / 'src').mkdir()
        (source / 'src/lib.rs').write_text(
            '#[test] fn first_filter() { assert_eq!(2 + 2, 4); }\n'
            '#[test] fn second_filter() { assert_eq!(3 + 3, 6); }\n')
        git = str(Path(shutil.which('git')).resolve())
        rustc = Path(os.environ.get('RMUX_RETENTION_TEST_RUSTC') or
                     subprocess.check_output([shutil.which('rustup'), 'which', 'rustc'], text=True).strip()).resolve()
        cargo = rustc.with_name('cargo')
        env = dict(HOME=os.environ['HOME'], PATH=str(rustc.parent) + os.pathsep + os.defpath,
                   RUSTC=str(rustc), RUSTDOC=str(rustc.with_name('rustdoc')), CARGO_BUILD_JOBS='1')
        for key in ('TMPDIR', 'TEMP', 'TMP', 'SYSTEMROOT'):
            if key in os.environ:
                env[key] = os.environ[key]
        records = []
        evidence = dict(os=guard.host_os(), harness_head=guard.git(script.parent.parent, 'rev-parse', 'HEAD'),
                        harness={str(path): guard.file_hash(path) for path in
                                 (script, script.with_name('candidate_identity_units.py'))},
                        source=str(source), executions=records)

        def execute(argv, active, timeout=120):
            started = time.monotonic()
            result = subprocess.run(argv, cwd=source, env=active, capture_output=True,
                                    text=True, timeout=timeout)
            records.append(dict(argv=argv, env=active, wait_seconds=time.monotonic() - started,
                                timeout_seconds=timeout, exit=result.returncode,
                                stdout=result.stdout, stderr=result.stderr))
            self.assertEqual(result.returncode, 0, result.stderr)
            return result

        try:
            for args in (['init', '-q'], ['add', '.'],
                         ['-c', 'user.name=Identity Test', '-c', 'user.email=identity@example.invalid',
                          'commit', '-qm', 'fixture']):
                execute([git, '-C', str(source), *args], env)
            previous = Path.cwd()
            started = time.monotonic()
            try:
                os.chdir(source)
                with patch.dict(os.environ, env, clear=True):
                    qualified = guard.prepare()
            finally:
                os.chdir(previous)
            evidence['preparation_seconds'] = time.monotonic() - started
            evidence['env'] = qualified
            target = Path(qualified['RMUX_IDENTITY_TARGET'])
            evidence['head'] = qualified['RMUX_IDENTITY_HEAD']
            evidence['target'] = str(target)
            prefix = [sys.executable, str(script), 'with', '--', str(cargo), 'test',
                      '--locked', '--offline', '--lib', '--verbose']
            first = execute([*prefix, 'first_filter', '--', '--exact'], qualified)
            self.assertIn('1 passed', first.stdout)
            images = list((target / 'debug/deps').glob('identity_freshness-*'))
            binary = next(path for path in images if path.is_file() and path.suffix == '')
            before = guard.stamp(binary.stat())
            logs = set((target / 'identity-compiler-logs').glob('*.log'))
            conventional = list((target / 'debug/deps').glob('identity_freshness-*.d'))
            second = execute([*prefix, 'second_filter', '--', '--exact'], qualified)
            self.assertIn('1 passed', second.stdout)
            with patch.dict(os.environ, qualified, clear=True):
                image = guard.verify_built(binary, source, evidence['head'], target)
            evidence.update(image=image, cargo_depinfo=[str(path) for path in conventional],
                            compiler_logs_before=len(logs), compiler_logs_after=len(list(
                                (target / 'identity-compiler-logs').glob('*.log'))),
                            stamp_before=before, stamp_after=guard.stamp(binary.stat()))
            evidence['launches'] = [json.loads(path.read_text()) for path in
                                    (target / 'identity-launches').glob('*.json')]
            self.assertTrue(conventional, 'Cargo conventional .d must survive the identity observation')
            self.assertEqual(set((target / 'identity-compiler-logs').glob('*.log')), logs,
                             'second filter must not invoke rustc')
            self.assertEqual(guard.stamp(binary.stat()), before, 'second filter must not relink')
            self.assertIn('Fresh identity-freshness', second.stderr)
            execute([sys.executable, str(script), 'check', str(binary)], qualified)
            execute([sys.executable, str(script), 'run', str(binary), 'second_filter', '--exact'], qualified)
        finally:
            (source / '.rmux-audit').mkdir(exist_ok=True)
            (source / '.rmux-audit/receipt.json').write_text(json.dumps(evidence, indent=2) + '\n')


if __name__ == '__main__':
    unittest.main()
