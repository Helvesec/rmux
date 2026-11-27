"""Executable comparison contract; fixture archives never claim native builds."""
import copy
import hashlib
import io
import json
import os
import re
import shutil
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / 'scripts/release/independent-builds.py'
sys.path.insert(0, str(ROOT / 'scripts'))
from candidate_identity import PRIVATE_ROOT  # noqa: E402
SHA = 'a' * 40
ROLES = {'cli': 'bin/rmux', 'helper': 'libexec/rmux/rmux', 'daemon': 'bin/rmux-daemon'}


class IndependentBuilds(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.addCleanup(self.temporary.cleanup)

    def receipt(self, name, changed=None, transform=None, platform="linux-x86_64", canonical=False, source_sha=SHA):
        directory = self.root / name
        directory.mkdir()
        sys.path.insert(0, str(ROOT / 'scripts/packaging'))
        import layout
        from layout_profiles import with_notices
        desc = with_notices(layout.load())
        variant = 'windows-archive' if platform.startswith('windows-') else 'unix-archive'
        suffix = '.zip' if variant == 'windows-archive' else '.tar.gz'
        contract = json.loads((ROOT / '.github/release/canonical-build-contract.json').read_text())
        target = next(item['target_triple'] for item in contract['platforms'] if item['key'] == platform)
        archive = directory / ('rmux-0.10.0-' + platform + suffix)
        roles = layout.payload_binary_roles(desc, variant)
        roles.update({e['id']: p['path'] for e, p in layout.entries(desc, variant)
                      if e['source']['kind'] == 'conpty' and e['source']['name'].endswith(('.exe', '.dll'))})
        files = {path: (role + '-binary').encode() for role, path in roles.items()}
        if canonical:
            for role in ('cli', 'helper', 'daemon'):
                files[roles[role]] += b'RMUX_RELEASE_IDENTITY_V1:' + source_sha.encode() + b':END'
        if changed:
            files[roles[changed]] += b'!'
        metadata = {'git_commit': source_sha, 'git_dirty': False, 'configuration': 'release',
                    'release_artifact': True, 'target': target,
                    'rmux_version': '0.10.0'}
        for role, prefix in [('cli', 'binary'), ('helper', 'helper_binary'), ('daemon', 'daemon_binary')]:
            data = files[roles[role]]
            metadata[prefix + '_sha256'] = hashlib.sha256(data).hexdigest()
            metadata[prefix + '_bytes'] = len(data)
        metadata['package_layout'] = layout.variant(desc, variant)['package_layout']
        metadata['skip_build'] = False
        for role, prefix in [('cli', 'binary'), ('helper', 'helper_binary'), ('daemon', 'daemon_binary')]:
            metadata[prefix + '_path'] = roles[role]
        for entry, placement in layout.entries(desc, variant):
            if not placement.get('optional'):
                files.setdefault(placement['path'], (entry['id'] + '-fixture').encode())
        files['share/rmux/package-layout.tsv'] = subprocess.check_output([
            sys.executable, str(ROOT / 'scripts/packaging/layout_dispatch.py'), '--with-notices',
            'manifest', '--variant', variant])
        files['share/rmux/artifact-metadata.json'] = json.dumps(metadata).encode()
        if transform:
            transform(files)
        files['SHA256SUMS.txt'] = ''.join(
            hashlib.sha256(data).hexdigest() + '  ' + path + '\n'
            for path, data in sorted(files.items()) if path != 'SHA256SUMS.txt').encode()
        if suffix == '.zip':
            with zipfile.ZipFile(archive, 'w') as zip_archive:
                for path, data in files.items():
                    zip_archive.writestr(archive.stem + '/' + path, data)
        else:
            with tarfile.open(archive, 'w:gz') as tar:
                for path, data in files.items():
                    info = tarfile.TarInfo(archive.name[:-7] + '/' + path)
                    info.size = len(data)
                    info.mode = 0o755 if path in set(roles.values()) | {'install.sh'} else 0o644
                    tar.addfile(info, io.BytesIO(data))
        value = {'schema_version': 1, 'source_sha': source_sha, 'platform_key': platform,
                 'toolchain': {'rustc': 'rustc 1.96.1\nhost: ' + target + '\nrelease: 1.96.1\ncommit-hash: ' + 'b' * 40, 'cargo': 'cargo 1.96.1 fixture', 'native_tools': {}},
                 'options': {'configuration': 'release', 'target': target,
                             'normalizations': [], 'cargo_locked': True, 'cargo_incremental': False,
                             'source_date_epoch': '12345',
                             'features': {'cli': ['default', 'tiny-cli'], 'helper': ['default'], 'daemon': ['default']},
                             'cargo_toml_sha256': 'c' * 64, 'cargo_lock_sha256': 'd' * 64,
                             'layout_sha256': 'e' * 64, 'notice_layout_sha256': 'f' * 64,
                             'packaging': {'linux_glibc_floor': '', 'max_supported_glibc': '',
                                           'codesign_adhoc': '0', 'strip_tiny': '1'}, 'cargo_configs': {}},
                 'build': {'id': name, 'target_dir': str(directory / 'target'),
                           'fresh_target': True, 'object_cache_restored': False},
                 'archive': {'path': archive.name, 'size': archive.stat().st_size,
                             'sha256': hashlib.sha256(archive.read_bytes()).hexdigest()},
                 'binaries': [{'role': role, 'path': path, 'size': len(files[path]),
                               'sha256': hashlib.sha256(files[path]).hexdigest()}
                              for role, path in sorted(roles.items()) if path in files]}
        if canonical:
            build_id = hashlib.sha256(name.encode()).hexdigest()[:32]
            target_dir = self.root / PRIVATE_ROOT / 'candidate-target' / source_sha / platform.split('-')[0] / build_id
            value['schema_version'] = 2
            value['build'].update(id=build_id, target_dir=str(target_dir), producers={
                role: dict(path=str(target_dir / target / 'release' / binary),
                           sha256=hashlib.sha256(files[roles[role]]).hexdigest(), size=len(files[roles[role]]),
                           commit=source_sha, source=str(self.root), os=platform.split('-')[0], target=str(target_dir))
                for role, binary in [('cli', 'rmux'), ('helper', 'rmux-full'), ('daemon', 'rmux-daemon')]})
        if canonical:
            for role, record in value['build']['producers'].items():
                path = Path(record['path'])
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(files[roles[role]])
        receipt = directory / 'independent-build.json'
        receipt.write_text(json.dumps(value))
        return receipt

    def rewrite(self, path, change):
        value = json.loads(path.read_text())
        change(value)
        path.write_text(json.dumps(value))

    def compare(self, left, right, extra=()):
        return subprocess.run([sys.executable, str(SCRIPT), 'compare', '--left', str(left),
                               '--right', str(right), '--source-sha', SHA,
                               '--output', str(self.root / 'comparison.json'), *extra],
                              capture_output=True, text=True)

    def test_exact_binary_comparison_measures_useful_byte_difference(self):
        left = self.receipt('left')
        right = self.receipt('right')
        result = self.compare(left, right)
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads((self.root / 'comparison.json').read_text())
        self.assertTrue(report['identical'])
        changed = self.receipt('changed', 'helper')
        result = self.compare(left, changed)
        self.assertNotEqual(result.returncode, 0)
        report = json.loads((self.root / 'comparison.json').read_text())
        self.assertFalse(report['identical'])
        self.assertEqual([item['role'] for item in report['differences']], ['helper'])
        self.assertEqual(report['differences'][0]['first_difference'], len(b'helper-binary'))

    def test_windows_executables_and_both_conpty_runtime_copies_are_critical(self):
        left = self.receipt('left', platform='windows-x86_64')
        right = self.receipt('right', platform='windows-x86_64')
        self.assertEqual(self.compare(left, right).returncode, 0)
        self.assertEqual(len(json.loads(left.read_text())['binaries']), 7)
        right = self.receipt('changed', changed='conpty-dll-helper', platform='windows-x86_64')
        self.assertEqual(self.compare(left, right).returncode, 1)
        report = json.loads((self.root / 'comparison.json').read_text())
        self.assertEqual([d['role'] for d in report['differences']], ['conpty-dll-helper'])

    def test_provenance_and_complete_binary_set_fail_closed(self):
        left = self.receipt('left')
        mutations = [
            ('source', lambda v: v.update(source_sha='b' * 40)),
            ('toolchain', lambda v: v['toolchain'].update(cargo='cargo other')),
            ('missing-toolchain', lambda v: v.pop('toolchain')),
            ('missing-options', lambda v: v.pop('options')),
            ('unknown-field', lambda v: v.update(unapproved=True)),
            ('normalization', lambda v: v['options'].update(normalizations=['strip-build-id'])),
            ('missing-normalizations', lambda v: v['options'].pop('normalizations')),
            ('profile', lambda v: v['options'].update(configuration='debug')),
            ('features', lambda v: v['options'].update(features={'cli': []})),
            ('lock', lambda v: v['options'].update(cargo_locked=False)),
            ('incremental', lambda v: v['options'].update(cargo_incremental=True)),
            ('epoch', lambda v: v['options'].update(source_date_epoch='different')),
            ('unknown-option', lambda v: v['options'].update(strip_useful_bytes=True)),
            ('source-input', lambda v: v['options'].update(cargo_lock_sha256='0' * 64)),
            ('cached', lambda v: v['build'].update(object_cache_restored=True)),
            ('not-fresh', lambda v: v['build'].update(fresh_target=False)),
            ('same-build', lambda v: v['build'].update(id='left')),
            ('same-target', lambda v: v['build'].update(target_dir=str(left.parent / 'target'))),
            ('missing-binary', lambda v: v['binaries'].pop()),
            ('duplicate-binary', lambda v: v['binaries'].append(copy.deepcopy(v['binaries'][0]))),
            ('binary-hash', lambda v: v['binaries'][0].update(sha256='0' * 64)),
            ('archive-hash', lambda v: v['archive'].update(sha256='0' * 64)),
            ('archive-path', lambda v: v['archive'].update(path='../escape.tar.gz')),
        ]
        for name, mutate in mutations:
            with self.subTest(name=name):
                right = self.receipt(name)
                self.rewrite(right, mutate)
                self.assertEqual(self.compare(left, right).returncode, 2)
                self.assertFalse((self.root / 'comparison.json').exists())

    def test_archive_boundaries_and_metadata_cannot_be_forged_by_rehashing(self):
        left = self.receipt('left')
        def metadata_change(field, value):
            def change(files):
                metadata = json.loads(files['share/rmux/artifact-metadata.json'])
                metadata[field] = value
                files['share/rmux/artifact-metadata.json'] = json.dumps(metadata).encode()
            return change
        cases = [
            ('extra', lambda files: files.update({'bin/hidden': b'executable'})),
            ('missing', lambda files: files.pop('libexec/rmux/rmux')),
            ('traversal', lambda files: files.update({'../escape': b'escape'})),
            ('manifest', lambda files: files.update({'share/rmux/package-layout.tsv': b'forged'})),
            ('dirty', metadata_change('git_dirty', True)),
            ('skip-build', metadata_change('skip_build', True)),
            ('other-source', metadata_change('git_commit', 'b' * 40)),
            ('wrong-size', metadata_change('helper_binary_bytes', 1)),
            ('wrong-hash', metadata_change('daemon_binary_sha256', '0' * 64)),
        ]
        for name, change in cases:
            with self.subTest(name=name):
                right = self.receipt(name, transform=change)
                self.assertEqual(self.compare(left, right).returncode, 2)

    def test_same_receipt_missing_archive_and_duplicate_json_are_refused(self):
        left = self.receipt('left')
        self.assertEqual(self.compare(left, left).returncode, 2)
        right = self.receipt('right')
        right.write_text(right.read_text().replace('"schema_version": 1', '"schema_version": 1, "schema_version": 1'))
        self.assertEqual(self.compare(left, right).returncode, 2)
        right = self.receipt('missing')
        (right.parent / json.loads(right.read_text())['archive']['path']).unlink()
        self.assertEqual(self.compare(left, right).returncode, 2)

    def test_canonical_fixture_keeps_lf_under_windows_text_translation(self):
        from unittest.mock import patch
        write_text = Path.write_text

        def windows_write_text(path, data, *args, **kwargs):
            if path.name == 'rustc-vV.txt':
                kwargs['newline'] = '\r\n'
            return write_text(path, data, *args, **kwargs)

        # Exercise the actual fixture with Windows text translation on any host.
        with patch.object(Path, 'write_text', windows_write_text):
            self.test_canonical_producer_record_is_joined_to_the_exact_archive()
        rustc = self.root / 'canonical/rustc-vV.txt'
        left = json.loads((self.root / 'left/independent-build.json').read_text())
        self.assertEqual(rustc.read_bytes(), (left['toolchain']['rustc'] + '\n').encode())

    def test_canonical_producer_record_is_joined_to_the_exact_archive(self):
        import shutil
        # Fixture assets claim no hosted/native credit.
        left, right = self.receipt('left', canonical=True), self.receipt('right', canonical=True)
        bundle = self.root / 'canonical'
        assets = bundle / 'assets'
        assets.mkdir(parents=True)
        value = json.loads(left.read_text())
        shutil.copy2(left.parent / value['archive']['path'], assets)
        for name in ['rmux_0.10.0_amd64.deb', 'rmux-0.10.0-1.x86_64.rpm',
                     'rmux-0.10.0-crate-package-set.tar', 'rmux-0.10.0-snap-amd64.snap',
                     'rmux-web-crypto-wasm-0.10.0.tar', 'rmux-web-crypto-wasm-0.10.0.provenance.json']:
            (assets / name).write_bytes(b'fixture-not-a-native-build')
        rustc = bundle / 'rustc-vV.txt'
        rustc.write_bytes((value['toolchain']['rustc'] + '\n').encode())
        program = ROOT / 'scripts/release/canonical-build-record.py'
        canonical = bundle / 'canonical-build-record.json'
        for arguments in [
            ['checksums', '--assets-dir', str(assets)],
            ['create', '--source-sha', SHA, '--fast-run-id', '42', '--candidate-run-id', '77',
             '--candidate-run-attempt', '1', '--release-intent-id', 'fixture:independent',
             '--planned-release-ref', 'v0.10.0', '--release-kind', 'shadow',
             '--platform-key', 'linux-x86_64', '--assets-dir', str(assets),
             '--runner-image', 'ubuntu-22.04', '--runner-os', 'Linux', '--runner-arch', 'X64',
             '--runner-environment', 'github-hosted', '--rustc-verbose', str(rustc),
             '--output', str(canonical)]]:
            result = subprocess.run([sys.executable, str(program), *arguments], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
        unpinned = self.compare(left, right, ['--canonical-record', str(canonical)])
        self.assertEqual(unpinned.returncode, 2, 'canonical byte join must require the producer record digest')
        digest = hashlib.sha256(canonical.read_bytes()).hexdigest()
        binding = ['--canonical-record', str(canonical), '--expected-canonical-record-sha256', digest]
        result = self.compare(left, right, binding)
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads((self.root / 'comparison.json').read_text())
        self.assertEqual(report['canonical_record_sha256'], hashlib.sha256(canonical.read_bytes()).hexdigest())
        self.assertFalse(report['hosted_attestation_verified'])
        lf = (value['toolchain']['rustc'] + '\n').encode()
        original_record = canonical.read_bytes()
        self.assertEqual(json.loads(original_record)['toolchain']['rustc_verbose_sha256'],
                         hashlib.sha256(lf).hexdigest())
        archive = assets / value['archive']['path']
        original_archive = archive.read_bytes()
        crlf = lf.replace(b'\n', b'\r\n')
        rustc.write_bytes(crlf)
        result = subprocess.run([sys.executable, str(program), *arguments], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        crlf_record = canonical.read_bytes()
        self.assertEqual(json.loads(crlf_record)['toolchain']['rustc_verbose_sha256'],
                         hashlib.sha256(crlf).hexdigest())
        self.assertNotEqual(hashlib.sha256(lf).hexdigest(), hashlib.sha256(crlf).hexdigest())
        stale = self.compare(left, right, binding)
        self.assertEqual(stale.returncode, 2, stale.stderr)
        self.assertIn('canonical producer record digest differs', stale.stderr)
        crlf_binding = ['--canonical-record', str(canonical), '--expected-canonical-record-sha256',
                        hashlib.sha256(crlf_record).hexdigest()]
        mismatch = self.compare(left, right, crlf_binding)
        self.assertEqual(mismatch.returncode, 2, mismatch.stderr)
        self.assertIn('canonical toolchain identity differs', mismatch.stderr)
        self.assertFalse((self.root / 'comparison.json').exists())
        self.assertEqual(canonical.read_bytes(), crlf_record)
        self.assertEqual(archive.read_bytes(), original_archive)
        rustc.write_bytes(lf)
        canonical.write_bytes(original_record)
        result = self.compare(left, right, binding)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.rewrite(canonical, lambda v: v.update(source_git_sha='b' * 40))
        self.assertEqual(self.compare(left, right, binding).returncode, 2)

    def test_evidence_links_and_output_overwrites_are_refused(self):
        import os
        left, right = self.receipt('left'), self.receipt('right')
        link = self.root / 'receipt-link'
        os.symlink(left, link)
        self.assertEqual(self.compare(link, right).returncode, 2)
        self.assertEqual(self.compare(left, right, ['--output', str(left)]).returncode, 2)
        self.assertEqual(json.loads(left.read_text())['source_sha'], SHA)
        output = self.root / 'comparison.json'
        os.symlink(left, output)
        self.assertEqual(self.compare(left, right).returncode, 2)
        self.assertEqual(json.loads(left.read_text())['source_sha'], SHA)

    def test_refusal_invalidates_success_when_receipt_disappears_or_breaks(self):
        left, right = self.receipt('left'), self.receipt('right')
        original = right.read_bytes()
        left_bytes = left.read_bytes()
        archive = right.parent / json.loads(original)['archive']['path']
        archive_bytes = archive.read_bytes()
        for directory in (self.root, left.parent, right.parent):
            output = directory / 'comparison.json'
            for defect in ('absent', 'invalid-json'):
                with self.subTest(defect=defect, directory=directory):
                    right.write_bytes(original)
                    self.assertEqual(self.compare(left, right, ['--output', str(output)]).returncode, 0)
                    if defect == 'absent':
                        right.unlink()
                    else:
                        right.write_bytes(b'{broken json')
                    self.assertEqual(self.compare(left, right, ['--output', str(output)]).returncode, 2)
                    self.assertFalse(output.exists(), 'stale successful report')
                    self.assertEqual(left.read_bytes(), left_bytes)
                    self.assertEqual(archive.read_bytes(), archive_bytes)
                    self.assertEqual(right.exists(), defect != 'absent')
                    if right.exists():
                        self.assertEqual(right.read_bytes(), b'{broken json')

    def test_broken_receipt_keeps_comparison_with_a_neighboring_alias(self):
        left, right = self.receipt('left'), self.receipt('right')
        original = right.read_bytes()
        output = right.parent / 'comparison.json'
        alias = right.parent / 'alias.json'
        for defect in ('absent', 'invalid-json'):
            for link in (os.link, os.symlink):
                with self.subTest(defect=defect, link=link.__name__):
                    right.write_bytes(original)
                    extra = ['--output', str(output)]
                    self.assertEqual(self.compare(left, right, extra).returncode, 0)
                    report = output.read_bytes()
                    link(output, alias)
                    if defect == 'absent':
                        right.unlink()
                    else:
                        right.write_bytes(b'{broken json')
                    result = self.compare(left, right, extra)
                    self.assertEqual(result.returncode, 2)
                    self.assertIn('would overwrite evidence', result.stderr)
                    self.assertEqual(output.read_bytes(), report)
                    self.assertEqual(alias.read_bytes(), report)
                    self.assertTrue(alias.samefile(output))
                    alias.unlink()

    @unittest.skipIf(sys.platform == 'win32', 'Unix canonical producer')
    def test_canonical_producer_reaches_packaging_without_identity_injection(self):
        import importlib.util
        import os
        from types import SimpleNamespace
        from unittest.mock import patch
        sys.path.insert(0, str(SCRIPT.parent))
        spec = importlib.util.spec_from_file_location('independent_builds', SCRIPT)
        producer = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(producer)
        head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
        rustc = subprocess.check_output(['rustc', '-vV'], text=True)
        triple = next(line.removeprefix('host: ') for line in rustc.splitlines() if line.startswith('host: '))
        platform = next(key for key, value in producer.PLATFORMS.items() if value[3] == triple)
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(('RMUX_IDENTITY_', 'CARGO_PROFILE_')) and key not in
               ('CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'RMUX_COMPLETIONS_DIR',
                'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS')}
        actual_run = subprocess.run
        def package(argv, **kwargs):
            if Path(argv[0]) != ROOT / 'scripts/package-unix.sh':
                return actual_run(argv, **kwargs)
            self.assertFalse(kwargs['env'].get('RUSTC_WRAPPER'))
            self.assertFalse(kwargs['env'].get('RUSTC_WORKSPACE_WRAPPER'))
            self.assertFalse(any(key.startswith('RMUX_IDENTITY_') for key in kwargs['env']))
            target = Path(kwargs['env']['CARGO_TARGET_DIR'])
            self.assertIn(head, target.parts)
            self.assertFalse(any(target.iterdir()), 'object target must be cold')
            output = Path(argv[-1])
            receipt = self.receipt('fixture-' + output.name, canonical=True, platform=platform, source_sha=head)
            archive = receipt.parent / json.loads(receipt.read_text())['archive']['path']
            _, _, binaries = producer.read_archive(archive, platform)
            directory = target / triple / 'release'
            directory.mkdir(parents=True)
            for role, name in [('cli', 'rmux'), ('helper', 'rmux-full'), ('daemon', 'rmux-daemon')]:
                (directory / name).write_bytes(binaries[role])
            shutil.copyfile(archive, output / archive.name)
            return subprocess.CompletedProcess(argv, 0)
        with patch.dict(os.environ, env, clear=True), patch.object(producer.subprocess, 'run', side_effect=package):
            receipts = []
            for name in ('canonical-left', 'canonical-right'):
                output = self.root / name
                self.assertEqual(producer.build(SimpleNamespace(source_sha=head, platform_key=platform,
                                                               output_dir=output)), 0)
                receipts.append(output / 'independent-build.json')
            values = [json.loads(path.read_text()) for path in receipts]
            self.assertNotEqual(values[0]['build']['target_dir'], values[1]['build']['target_dir'])
            for value in values:
                self.assertEqual(value['schema_version'], 2)
                for role, record in value['build']['producers'].items():
                    self.assertEqual(record['commit'], head)
                    self.assertEqual(hashlib.sha256(Path(record['path']).read_bytes()).hexdigest(), record['sha256'])
            self.assertEqual(producer.compare(SimpleNamespace(left=receipts[0], right=receipts[1],
                output=self.root / 'comparison.json', source_sha=head, canonical_record=None,
                expected_canonical_record_sha256=None)), 0)

    @unittest.skipIf(sys.platform == 'win32', 'Unix canonical producer')
    def test_two_cold_canonical_targets_keep_identical_embedded_commit_bytes(self):
        import importlib.util
        import uuid
        from unittest.mock import patch
        audit = ROOT / 'target' / 'tmp' / 'canonical-commit-tests'
        audit.mkdir(parents=True, exist_ok=True)
        source = Path(tempfile.mkdtemp(dir=audit)).resolve()
        (source / '.gitignore').write_text(PRIVATE_ROOT + '/\n')
        (source / 'Cargo.toml').write_text(
            '[package]\nname="rmux"\nversion="0.1.0"\nedition="2021"\n'
            '[build-dependencies]\nembed-manifest={path="embed"}\n'
            '[[bin]]\nname="rmux"\npath="main.rs"\n'
            '[[bin]]\nname="rmux-daemon"\npath="main.rs"\n[workspace]\n')
        # Windows manifest support is outside this native Unix fixture.
        (source / 'embed/src').mkdir(parents=True)
        (source / 'embed/Cargo.toml').write_text('[package]\nname="embed-manifest"\nversion="1.4.0"\nedition="2021"\n')
        (source / 'embed/src/lib.rs').write_text('pub fn embed_manifest_file(_: &str) -> std::io::Result<()> { Ok(()) }\n')
        (source / 'main.rs').write_text('fn main() {}\n')
        shutil.copyfile(ROOT / 'build.rs', source / 'build.rs')
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(('RMUX_IDENTITY_', 'CARGO_PROFILE_', 'CARGO_TARGET_'))
               and key not in ('CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER',
                               'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_MANIFEST_DIR')}
        locked = subprocess.run(['cargo', 'generate-lockfile', '--offline'], cwd=source, env=env,
                                capture_output=True, text=True)
        self.assertEqual(locked.returncode, 0, locked.stderr)
        without_git = subprocess.run(['cargo', 'build', '--locked', '--offline', '--release', '--target',
                                      next(line.removeprefix('host: ') for line in subprocess.check_output(
                                          ['rustc', '-vV'], env=env, text=True).splitlines() if line.startswith('host: '))],
                                     cwd=source, env=dict(env, CARGO_TARGET_DIR=str(source / PRIVATE_ROOT / 'no-git')),
                                     capture_output=True, text=True)
        self.assertEqual(without_git.returncode, 0, without_git.stderr)
        no_git_binary = next((source / PRIVATE_ROOT / 'no-git').glob('*/release/rmux'))
        self.assertFalse(b'RMUX_RELEASE_IDENTITY_V1:' in no_git_binary.read_bytes(),
                         'an ancestor repository cannot identify an unpacked source package')
        git = ['git', '-C', str(source)]
        for args in [['init', '-q'], ['add', '.'], ['-c', 'user.name=Identity Test',
                     '-c', 'user.email=identity@example.invalid', 'commit', '-qm', 'fixture']]:
            subprocess.run(git + args, check=True, capture_output=True)
        head = subprocess.check_output(git + ['rev-parse', 'HEAD'], text=True).strip()
        rustc = subprocess.check_output(['rustc', '-vV'], env=env, text=True)
        triple = next(line.removeprefix('host: ') for line in rustc.splitlines() if line.startswith('host: '))
        spec = importlib.util.spec_from_file_location('release_identity', ROOT / 'scripts/candidate_identity.py')
        identity = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(identity)
        payloads = []
        targets = []
        for _ in range(2):
            target = identity.target_directory(source, head, uuid.uuid4().hex)
            self.assertFalse(target.exists())
            targets.append(target)
            result = subprocess.run(['cargo', 'build', '--locked', '--offline', '--release', '--target', triple],
                                    cwd=source, env=dict(env, CARGO_TARGET_DIR=str(target), CARGO_INCREMENTAL='0'),
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            pair = {}
            for name in ('rmux', 'rmux-daemon'):
                data = (target / triple / 'release' / name).read_bytes()
                self.assertEqual(set(re.findall(rb'RMUX_RELEASE_IDENTITY_V1:([0-9a-f]{40}):END', data)),
                                 {head.encode()}, 'canonical build must embed the actual clean HEAD')
                self.assertNotRegex(data, rb'RMUX_BUILD_GRAPH_V[0-9]+:')
                self.assertNotIn(str(target).encode(), data)
                pair[name] = data
            payloads.append(pair)
        self.assertNotEqual(targets[0], targets[1])
        self.assertEqual(payloads[0], payloads[1], 'no normalization of native binary bytes')
        # Reusing this target at a new HEAD must update the marker as well.
        subprocess.run(git + ['-c', 'user.name=Identity Test', '-c', 'user.email=identity@example.invalid',
                       'commit', '--allow-empty', '-qm', 'new head'], check=True, capture_output=True)
        newer = subprocess.check_output(git + ['rev-parse', 'HEAD'], text=True).strip()
        result = subprocess.run(['cargo', 'build', '--locked', '--offline', '--release', '--target', triple],
                                cwd=source, env=dict(env, CARGO_TARGET_DIR=str(targets[-1]), CARGO_INCREMENTAL='0'),
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        data = (targets[-1] / triple / 'release/rmux').read_bytes()
        self.assertEqual(set(re.findall(rb'RMUX_RELEASE_IDENTITY_V1:([0-9a-f]{40}):END', data)), {newer.encode()})

    def test_canonical_v2_provenance_and_embedded_commit_fail_closed(self):
        left = self.receipt('left', canonical=True)
        right = self.receipt('right', canonical=True)
        self.assertEqual(self.compare(left, right).returncode, 0)
        original = right.read_bytes()
        mutations = [lambda v: v['build']['producers']['cli'].update(commit='b' * 40),
                     lambda v: v['build']['producers']['helper'].update(sha256='bad'),
                     lambda v: v['build']['producers']['daemon'].update(path='/foreign/binary'),
                     lambda v: v['build']['producers']['cli'].update(source='/foreign/source'),
                     lambda v: v['build']['producers'].pop('helper')]
        for change in mutations:
            right.write_bytes(original)
            self.rewrite(right, change)
            self.assertEqual(self.compare(left, right).returncode, 2)
            self.assertFalse((self.root / 'comparison.json').exists())
        def changed_marker(files):
            path = ROLES['cli']
            files[path] = files[path].replace(SHA.encode(), b'b' * 40)
            metadata = json.loads(files['share/rmux/artifact-metadata.json'])
            metadata['binary_sha256'] = hashlib.sha256(files[path]).hexdigest()
            files['share/rmux/artifact-metadata.json'] = json.dumps(metadata).encode()
        wrong = self.receipt('wrong-marker', canonical=True, transform=changed_marker)
        self.assertEqual(self.compare(left, wrong).returncode, 2)
        def graph(files):
            path = ROLES['cli']
            files[path] += b'RMUX_BUILD_GRAPH_V1:variable-target:END'
            metadata = json.loads(files['share/rmux/artifact-metadata.json'])
            metadata.update(binary_sha256=hashlib.sha256(files[path]).hexdigest(), binary_bytes=len(files[path]))
            files['share/rmux/artifact-metadata.json'] = json.dumps(metadata).encode()
        injected = self.receipt('graph', canonical=True, transform=graph)
        self.assertEqual(self.compare(left, injected).returncode, 2)
        self.assertEqual(self.compare(left, self.receipt('legacy')).returncode, 2)

    def test_early_refusals_clear_safe_outputs_and_preserve_all_evidence_aliases(self):
        left, right = self.receipt('left'), self.receipt('right')
        output = self.root / 'comparison.json'
        self.assertEqual(self.compare(left, right).returncode, 0)
        original = right.read_bytes()
        left_bytes = left.read_bytes()
        archive = right.parent / json.loads(original)['archive']['path']
        archive_bytes = archive.read_bytes()
        right.write_bytes(b'{broken')
        # Unknown archive names must stay protected after receipt parse failure.
        self.assertEqual(self.compare(left, right, ['--output', str(archive)]).returncode, 2)
        self.assertEqual(archive.read_bytes(), archive_bytes)
        output.unlink()
        os.link(archive, output)
        self.assertEqual(self.compare(left, right).returncode, 2)
        self.assertEqual(output.read_bytes(), archive_bytes)
        output.unlink()
        right.write_bytes(original)
        for extra in (['--expected-canonical-record-sha256', 'a' * 64],
                      ['--canonical-record', str(self.root / 'missing-canonical')],
                      ['--source-sha', 'bad']):
            self.assertEqual(self.compare(left, right).returncode, 0)
            self.assertEqual(self.compare(left, right, extra).returncode, 2)
            self.assertFalse(output.exists())
        self.assertEqual(left.read_bytes(), left_bytes)
        self.assertEqual(right.read_bytes(), original)
        self.assertEqual(archive.read_bytes(), archive_bytes)

    def test_comparison_cannot_overwrite_canonical_producer_bytes(self):
        left, right = self.receipt('left', canonical=True), self.receipt('right', canonical=True)
        raw = Path(json.loads(right.read_text())['build']['producers']['cli']['path'])
        data = raw.read_bytes()
        for defect in ('valid', 'invalid-json', 'absent'):
            with self.subTest(defect=defect):
                raw.write_bytes(data)
                if defect == 'invalid-json':
                    right.write_bytes(b'{broken')
                elif defect == 'absent':
                    right.unlink()
                result = self.compare(left, right, ['--output', str(raw)])
                self.assertEqual(result.returncode, 2)
                self.assertEqual(raw.read_bytes(), data)

    @unittest.skipIf(sys.platform == 'win32', 'Unix canonical producer')
    def test_canonical_producer_rejects_a_gitless_package_inside_another_repository(self):
        import importlib.util
        from types import SimpleNamespace
        from unittest.mock import patch
        sys.path.insert(0, str(SCRIPT.parent))
        spec = importlib.util.spec_from_file_location('independent_builds', SCRIPT)
        producer = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(producer)
        source = ROOT / 'target' / 'tmp' / 'foreign-canonical-source'
        source.mkdir(parents=True, exist_ok=True)
        head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
        triple = next(line.removeprefix('host: ') for line in subprocess.check_output(
            ['rustc', '-vV'], text=True).splitlines() if line.startswith('host: '))
        platform = next(key for key, value in producer.PLATFORMS.items() if value[3] == triple)
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(('RMUX_IDENTITY_', 'CARGO_PROFILE_')) and key not in
               ('CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'RMUX_COMPLETIONS_DIR',
                'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS')}
        output = self.root / 'foreign-output'
        with patch.dict(os.environ, env, clear=True), patch.object(producer, 'ROOT', source):
            with self.assertRaisesRegex(ValueError, 'canonical source must be the repository root'):
                producer.build(SimpleNamespace(source_sha=head, platform_key=platform, output_dir=output))
        self.assertFalse(output.exists())

    def test_canonical_provenance_ignores_read_access_time_but_rejects_mutation(self):
        from types import SimpleNamespace
        from unittest.mock import patch
        sys.path.insert(0, str(SCRIPT.parent))
        import canonical_payload as payload
        target = self.root / 'target'
        target.mkdir()
        binary = target / 'rmux'
        data = b'RMUX_RELEASE_IDENTITY_V1:' + SHA.encode() + b':END'
        binary.write_bytes(data)
        info = binary.stat()
        fields = ('st_dev', 'st_ino', 'st_mode', 'st_size', 'st_mtime_ns', 'st_ctime_ns', 'st_atime_ns')
        before = SimpleNamespace(**{key: getattr(info, key) for key in fields})
        accessed = SimpleNamespace(**dict(vars(before), st_atime_ns=before.st_atime_ns + 1))
        with patch.object(payload, 'regular', return_value=binary), patch.object(Path, 'stat', side_effect=[before, accessed]):
            record = payload.provenance(binary, self.root, SHA, target, 'linux')
        self.assertEqual(record['sha256'], hashlib.sha256(data).hexdigest())
        self.assertEqual(record['commit'], SHA)
        changed = SimpleNamespace(**dict(vars(before), st_mtime_ns=before.st_mtime_ns + 1))
        with patch.object(payload, 'regular', return_value=binary), patch.object(Path, 'stat', side_effect=[before, changed]):
            with self.assertRaisesRegex(ValueError, 'changed during verification'):
                payload.provenance(binary, self.root, SHA, target, 'linux')

    def test_build_without_qualified_context_is_refused_before_output(self):
        import os
        env = {k: v for k, v in os.environ.items() if not k.startswith('RMUX_IDENTITY_')}
        result = subprocess.run([sys.executable, str(SCRIPT), 'build', '--source-sha', SHA,
                                 '--platform-key', 'linux-x86_64', '--output-dir', str(self.root / 'new')],
                                env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertFalse((self.root / 'new').exists())


@unittest.skipUnless(sys.platform == "linux", "native Linux identity linker")
class ReleaseIdentityLink(unittest.TestCase):
    def test_release_link_retains_actual_head_for_both_binary_names(self):
        import uuid
        sys.path.insert(0, str(ROOT / "scripts/release"))
        from canonical_payload import embedded_commit

        head = subprocess.check_output(["git", "-C", str(ROOT), "rev-parse", "HEAD"],
                                       text=True).strip()
        target = ROOT / "target" / "tmp" / "identity-link" / head / "linux" / uuid.uuid4().hex
        target.mkdir(parents=True, exist_ok=False)
        rustc = shutil.which("rustc")
        linker = os.environ.get("RMUX_TEST_RELEASE_LINKER", shutil.which("cc"))
        self.assertTrue(rustc and linker, "Rust compiler and native linker required")
        env = {k: v for k, v in os.environ.items()
               if k not in ("CARGO_CFG_WINDOWS", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS")}
        commands = []

        def run(args, extra=None):
            index = len(commands)
            result = subprocess.run(args, cwd=ROOT, env=dict(env, **(extra or {})),
                                    capture_output=True, text=True)
            log = target / f"command-{index}.log"
            log.write_text(result.stdout + result.stderr)
            commands.append(dict(argv=list(map(str, args)), returncode=result.returncode,
                                 log=str(log)))
            (target / "commands.json").write_text(json.dumps(commands, indent=2) + "\n")
            self.assertEqual(result.returncode, 0, f"{args}\n{result.stderr}\n{log}")
            return result.stdout

        # Manifest embedding is Windows-only; this support crate is never invoked.
        support = target / "embed.rs"
        support.write_text("pub fn embed_manifest_file(_: &str) -> std::io::Result<()> { Ok(()) }\n")
        library = target / "libembed_manifest.rlib"
        run([rustc, "--crate-name", "embed_manifest", "--crate-type", "rlib",
             str(support), "-o", str(library)])
        build = target / "build-script"
        run([rustc, "--edition=2021", str(ROOT / "build.rs"), "--extern",
             f"embed_manifest={library}", "-o", str(build)])
        triple = next(line.removeprefix("host: ") for line in run([rustc, "-vV"]).splitlines()
                      if line.startswith("host: "))
        emitted = run([str(build)], dict(CARGO_MANIFEST_DIR=str(ROOT), OUT_DIR=str(target),
                                        TARGET=triple, RUSTC=rustc))
        main = target / "main.rs"
        main.write_text("fn main() {}\n")
        images = []
        for name in ("rmux", "rmux-daemon"):
            prefix = f"cargo:rustc-link-arg-bin={name}="
            arguments = [line[len(prefix):] for line in emitted.splitlines() if line.startswith(prefix)]
            self.assertEqual(len(arguments), 2, emitted)
            image = target / name
            command = [rustc, "--edition=2021", "--crate-name", name.replace("-", "_"),
                       str(main), "--target", triple, "-C", "opt-level=z", "-C", "lto=fat",
                       "-C", "panic=abort", "-C", "strip=symbols", "-C", f"linker={linker}"]
            for argument in arguments:
                command += ["-C", f"link-arg={argument}"]
            run(command + ["-o", str(image)])
            data = image.read_bytes()
            self.assertEqual(embedded_commit(data, head), head)
            self.assertEqual(re.findall(rb"RMUX_RELEASE_IDENTITY_V1:([0-9a-f]{40}):END", data),
                             [head.encode()])
            self.assertNotIn(str(target).encode(), data)
            images.append(dict(path=str(image), sha256=hashlib.sha256(data).hexdigest(),
                               head=head, size=len(data)))
            run([str(image)])
        (target / "images.json").write_text(json.dumps(images, indent=2) + "\n")
        print(f"identity-link evidence: {target}", flush=True)


if __name__ == '__main__':
    unittest.main()
