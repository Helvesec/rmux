#!/usr/bin/env python3
"""Pure full-entry provenance checks and bounded validation of acquired tar pairs."""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

import package_tar_repro_spec as acquired

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / 'scripts/package-unix.sh'


def function(text, name):
    found = re.search(r'^' + name + r'\(\) \{\n.*?^\}\n', text, re.M | re.S)
    if not found:
        raise ValueError('missing function ' + name)
    return found.group()


def validate_metadata(raw, freeze):
    acquired.require(raw == Path(freeze['expected_metadata_bytes']['path']).read_bytes(), 'metadata bytes differ')
    data = json.loads(raw)
    acquired.require(data == freeze['expected_metadata'], 'metadata differs')
    acquired.require(data['git_commit'] == freeze['head'] and data['git_dirty'] is False
                     and data['generated_at_utc'] == freeze['commit_date'], 'packager identity differs')
    acquired.require(data['release_artifact'] is False and data['skip_build'] is True, 'local-only metadata differs')
    acquired.require(data['payload_provenance']['source'] == freeze['payload_source']['commit']
                     and data['payload_provenance']['tree'] == freeze['payload_source']['tree'], 'payload identity differs')
    return data


def validate_archive(path, freeze):
    # Retain the acquired010 reader and complete canonical-header checks unchanged.
    value = acquired.inventory(path)
    acquired.check_canonical(value, freeze)
    acquired.require(len([r for r in value['entries'] if r['kind'] == 'file']) == 16, 'full inventory differs')
    return value


class UnixFullProvenance(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name); self.stage = self.root / 'stage'; self.stage.mkdir()
        self.roles = {'cli': 'bin/rmux', 'helper': 'libexec/rmux/rmux', 'daemon': 'bin/rmux-daemon'}
        self.declaration = dict(source='b' * 40, tree='1' * 40, configuration='release', files=[])
        for role, name in self.roles.items():
            p = self.stage / name; p.parent.mkdir(parents=True, exist_ok=True)
            data = ('never execute fixture ' + role).encode(); p.write_bytes(data)
            self.declaration['files'].append(dict(role=role, sha256=acquired.digest(data), bytes=len(data)))

    def save(self, name, value):
        if ARGS.proofs:
            p = Path(ARGS.proofs); p.mkdir(parents=True, exist_ok=True)
            (p / (name + '.json')).write_text(json.dumps(value, indent=2) + '\n')

    def invoke(self, declaration, baseline=False):
        p = self.root / 'declaration.json'; p.write_text(json.dumps(declaration))
        if baseline:
            old = subprocess.check_output(['git', '-C', str(ROOT), 'show', ARGS.base + ':scripts/package-unix.sh'], text=True)
            boundary = old.split('payload_provenance_field=""\n', 1)[1].split('\nglibc_metadata_fields=', 1)[0]
            code = function(old, 'layout_tool') + function(old, 'die')
            code += '\nrepo_root="$1"\nstage_dir="$2"\nconfiguration=release\npayload_provenance_file="$3"\n'
            code += boundary + '\nprintf "%s\\n" "$payload_provenance_field"\n'
        else:
            code = function(SCRIPT.read_text(), 'unix_payload_provenance')
            code += '\nrepo_root="$1"\nstage_dir="$2"\nconfiguration=release\nunix_payload_provenance "$3"\n'
        return subprocess.run(['/usr/bin/bash', '-c', 'set -euo pipefail\n' + code, 'test', str(ROOT), str(self.stage), str(p)],
                              capture_output=True, text=True, timeout=10, env=dict(PATH='/usr/bin:/bin', PYTHONDONTWRITEBYTECODE='1'))

    def rejected(self, name, edit, reason):
        d = copy.deepcopy(self.declaration); edit(d); r = self.invoke(d)
        self.save(name, dict(declaration=d, rc=r.returncode, stdout=r.stdout, stderr=r.stderr, expected=reason))
        self.assertNotEqual(r.returncode, 0); self.assertEqual(r.stdout, ''); self.assertIn(reason, r.stderr)

    def test_01_valid_with_and_without_optional_sizes(self):
        for sizes in [True, False]:
            if not sizes:
                for item in self.declaration['files']: item.pop('bytes')
            r = self.invoke(self.declaration); self.assertEqual(r.returncode, 0, r.stderr)
            actual = json.loads('{' + r.stdout.rstrip().removesuffix(',') + '}')['payload_provenance']
            self.assertEqual({f['role']: f['path'] for f in actual['files']}, self.roles)
            self.assertEqual(actual['configuration'], 'release')

    def test_02_configuration_and_baseline_counterproof(self):
        bad = dict(self.declaration, configuration='debug'); r = self.invoke(bad, baseline=True)
        self.save('baseline-configuration-accepted', dict(rc=r.returncode, stdout=r.stdout, stderr=r.stderr))
        self.assertEqual(r.returncode, 0, r.stderr)
        self.rejected('configuration-refused', lambda d: d.update(configuration='debug'), 'configuration differs')

    def test_03_size_and_baseline_counterproof(self):
        bad = copy.deepcopy(self.declaration); bad['files'][0]['bytes'] = 1; r = self.invoke(bad, baseline=True)
        self.save('baseline-size-accepted', dict(rc=r.returncode, stdout=r.stdout, stderr=r.stderr))
        self.assertEqual(r.returncode, 0, r.stderr)
        for value in [1, True, '22', -1]:
            with self.subTest(value=value):
                self.rejected('size-' + str(value), lambda d: d['files'][0].update(bytes=value), 'size differs')

    def test_04_wrong_hash_and_truncation(self):
        self.rejected('hash', lambda d: d['files'][0].update(sha256='0' * 64), 'declared')
        (self.stage / self.roles['cli']).write_bytes(b'')
        self.rejected('truncated', lambda d: None, 'declared')

    def test_05_role_set(self):
        self.rejected('missing-role', lambda d: d['files'].pop(), 'undeclared binary role')
        self.rejected('duplicate-role', lambda d: d['files'].append(d['files'][0]), 'role declared twice')
        self.rejected('unknown-role', lambda d: d['files'][0].update(role='alien'), 'unknown binary role')

    def test_06_whole_entry_arguments(self):
        for args, reason in [(['--skip-build'], 'local-only'), (['--configuration', 'alien'], 'unsupported configuration'),
                             (['--payload-provenance'], 'requires a value'), (['--alien'], 'unknown argument')]:
            with self.subTest(args=args):
                r = subprocess.run(['/usr/bin/bash', str(SCRIPT), *args], capture_output=True, text=True, timeout=10)
                self.assertNotEqual(r.returncode, 0); self.assertIn(reason, r.stderr)

    def test_07_whole_entry_refuses_before_native_tools(self):
        target = self.root / 'target/x86_64-unknown-linux-gnu/release'; target.mkdir(parents=True)
        for role, name in [('cli', 'rmux'), ('helper', 'rmux-full'), ('daemon', 'rmux-daemon')]:
            p = target / name; p.write_bytes((self.stage / self.roles[role]).read_bytes()); p.chmod(0o755)
        completions = self.root / 'completions'; completions.mkdir()
        for name in ['rmux.bash', '_rmux', 'rmux.fish', '_rmux.ps1', 'rmux.elv']:
            (completions / name).write_text('fixture completion\n')
        sentinels = self.root / 'bin'; sentinels.mkdir(); marker = self.root / 'forbidden-call'
        for name in ['tar', 'gzip', 'readelf', 'cargo', 'strip', 'codesign']:
            p = sentinels / name; p.write_text('#!/bin/sh\nprintf called > "$FORBIDDEN_MARKER"\nexit 95\n'); p.chmod(0o755)
        env = dict(os.environ, PATH=str(sentinels) + ':/usr/bin:/bin', FORBIDDEN_MARKER=str(marker),
                   CARGO_TARGET_DIR=str(self.root / 'target'), RMUX_COMPLETIONS_DIR=str(completions),
                   RMUX_PACKAGE_STRIP_TINY='0', TMPDIR=str(self.root), PYTHONDONTWRITEBYTECODE='1')
        for kind in ['configuration', 'bytes', 'sha256']:
            d = copy.deepcopy(self.declaration)
            if kind == 'configuration': d['configuration'] = 'debug'
            else: d['files'][0][kind] = 1 if kind == 'bytes' else '0' * 64
            p = self.root / 'declaration.json'; p.write_text(json.dumps(d))
            argv = ['/usr/bin/bash', str(SCRIPT), '--skip-build', '--allow-stale-binary', '--target',
                    'x86_64-unknown-linux-gnu', '--payload-provenance', str(p), '--output-dir', str(self.root / 'dist')]
            r = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=15)
            self.save('whole-entry-' + kind, dict(argv=argv, rc=r.returncode, stdout=r.stdout, stderr=r.stderr,
                                                forbidden_tool_called=marker.exists()))
            self.assertNotEqual(r.returncode, 0); self.assertIn('payload provenance', r.stderr)
            self.assertFalse(marker.exists()); self.assertFalse(list((self.root / 'dist').glob('*.tar.gz')))


    def test_08_binary_copy_modes_independent_of_umask(self):
        old = subprocess.check_output(['git', '-C', str(ROOT), 'show', ARGS.base + ':scripts/package-unix.sh'], text=True)
        for baseline, text in [(True, old), (False, SCRIPT.read_text())]:
            commands = re.findall(r'^(?:cp|install -m 0755) "\$(?:binary|helper_binary|daemon_binary)" "\$stage_dir/[^\"]+"$', text, re.M)
            self.assertEqual(len(commands), 3)
            for mask in ['0022', '0077']:
                stage = self.root / ('modes-' + str(baseline) + mask)
                (stage / 'bin').mkdir(parents=True); (stage / 'libexec/rmux').mkdir(parents=True)
                source = self.root / 'fixture'; source.write_text('never execute fixture'); source.chmod(0o755)
                code = 'set -euo pipefail\numask ' + mask + '\nbinary="$1"\nhelper_binary="$1"\ndaemon_binary="$1"\nstage_dir="$2"\n' + '\n'.join(commands)
                r = subprocess.run(['/usr/bin/bash', '-c', code, 'test', str(source), str(stage)], capture_output=True, text=True)
                modes = {str(p.relative_to(stage)): p.stat().st_mode & 0o777 for p in stage.rglob('*') if p.is_file()}
                self.save('copy-modes-' + str(baseline) + mask, dict(rc=r.returncode, observed_modes=modes, baseline=baseline, umask=mask))
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(set(modes.values()), {0o700 if baseline and mask == '0077' else 0o755})


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', default='d8ca956b0af07f178a9531778354d0cd42e5f8bc')
    parser.add_argument('--proofs')
    ARGS, remaining = parser.parse_known_args()
    unittest.main(argv=[sys.argv[0], *remaining], verbosity=2)
