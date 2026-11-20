"""Execute the actual CLI hardlink test under D41 while watching its image."""
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / 'scripts/candidate_identity.py'


class HardlinkEvidence(unittest.TestCase):
    def test_cli_hardlink_writer_preserves_its_verified_execution_image(self):
        spec = importlib.util.spec_from_file_location('guard', SCRIPT)
        g = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(g)
        cache = g.support('candidate_identity_cache')
        audit = ROOT / '.rmux-audit/hardlink-evidence'
        audit.mkdir(parents=True, exist_ok=True)
        source = Path(tempfile.mkdtemp(dir=audit)).resolve()
        # The CLI unit tests live in src/cli/tests.rs, declared by src/cli.rs.
        text = (ROOT / 'src/cli/tests.rs').read_text()
        name = 'usable_shell_path_rejects_hardlink_to_the_current_executable'
        start = text.index('\n#[test]\nfn ' + name + '(') + len('\n#[test]\n')
        end = text.index('\n#[test]', start)
        body = text[start:end]
        shutil.copyfile(ROOT / 'src/cli/shell_startup.rs', source / 'shell_startup.rs')
        prelude = '''
#![allow(dead_code)]
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
struct StartupOptions;
struct ExitFailure;
impl ExitFailure { fn new(_: i32, _: String) -> Self { Self } }
fn connect_with_startserver(_: &Path, _: StartupOptions) -> Result<(), ExitFailure> { Ok(()) }
mod shell_startup;
use shell_startup::{usable_shell_path, same_file_identity_for_paths};
static UNIQUE_TEST_ID: AtomicUsize = AtomicUsize::new(0);
fn unique_test_dir(name: &str) -> PathBuf {
    std::env::current_exe().unwrap().parent().unwrap().join(format!("{}-{}", name, std::process::id()))
}
'''
        (source / 'main.rs').write_text(prelude + '\nmod tests { use super::*;\n#[test]\n' + body + '\n}\n')
        (source / 'Cargo.toml').write_text('[package]\nname="int278-writer"\nversion="0.1.0"\n')
        (source / 'Cargo.lock').write_text('version = 4\n')
        (source / '.gitignore').write_text('.rmux-audit/\n')
        git = ['git', '-C', str(source)]
        for args in (['init', '-q'], ['add', '.'], ['-c', 'user.name=Identity Test', '-c',
                     'user.email=identity@example.invalid', 'commit', '-qm', 'native writer fixture']):
            subprocess.run(git + args, check=True)
        rustc = Path(subprocess.check_output(['rustup', 'which', 'rustc'], text=True).strip()).resolve()
        env = dict(os.environ, RUSTC=str(rustc), RUSTDOC=str(rustc.with_name('rustdoc')))
        for key in tuple(env):
            if key.startswith('RMUX_IDENTITY_') or (key.startswith('CARGO_TARGET_') and key.endswith('_RUNNER')) or key in ('CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER'):
                env.pop(key)
        previous = Path.cwd()
        try:
            os.chdir(source)
            with patch.dict(os.environ, env, clear=True):
                env = g.prepare()
        finally:
            os.chdir(previous)
        target = Path(env['RMUX_IDENTITY_TARGET'])
        binary = target / 'hardlink-writer'
        env['CARGO_MANIFEST_DIR'] = str(source)
        with patch.dict(os.environ, env, clear=True):
            self.assertEqual(g.rustc_wrapper([str(rustc), '--crate-name', 'hardlink_writer',
                '--test', str(source / 'main.rs'), '-o', str(binary)]), 0)
            proof = g.verify_built(binary, source, env['RMUX_IDENTITY_HEAD'], target)
            import json
            image = Path(json.loads(Path(proof['build']).read_text())['path'])

            def calculate():
                before = g.file_hash(image)
                stamp = cache.snapshot(g, image)
                child = subprocess.run([sys.executable, str(SCRIPT), 'run', str(image),
                    'tests::' + name, '--exact', '--nocapture'], env=env, cwd=source,
                    capture_output=True, text=True)
                self.assertEqual(child.returncode, 0, child.stdout + child.stderr)
                self.assertIn('1 passed', child.stdout)
                import hashlib
                observed = dict(image=proof, retained_path=str(image), before=stamp,
                    after=cache.snapshot(g, image), before_sha256=before,
                    after_sha256=hashlib.sha256(image.read_bytes()).hexdigest(),
                    HEAD=subprocess.check_output(git + ['rev-parse', 'HEAD'], text=True).strip(),
                    native_stdout=child.stdout, native_exit=child.returncode)
                (audit / (source.name + '-observation.json')).write_text(json.dumps(observed, indent=2))
                print('native observation: ' + str(audit / (source.name + '-observation.json')))
                self.assertEqual(observed['after_sha256'], before)
                self.assertEqual(g.file_hash(image), before)
                return proof

            self.assertEqual(cache.cached(g, 'writer-witness', image, source,
                env['RMUX_IDENTITY_HEAD'], target, calculate), proof)


if __name__ == '__main__':
    unittest.main()
