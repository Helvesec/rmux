"""Neighbor checks for the child's retained consumer session and version."""
from contextlib import ExitStack
import copy
import json
import os
from pathlib import Path
import subprocess
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import candidate_identity as guard
import native_input_windows as native
import native_input_windows_consumer as retention
import native_input_windows_process as process
import test_native_input_collector_consumer as fixtures


class ChildSessionNeighbors(unittest.TestCase):
    setUp = fixtures.ConsumerRetention.setUp
    assert_receipt = fixtures.ConsumerRetention.assert_receipt
    run_action = fixtures.ConsumerRetention.run_action

    def prepare(self):
        ticket = self.run_action()
        self.compiler = self.source / 'compiler-fixture'
        self.compiler.write_text('never launched')
        ticket['compiler'] = str(self.compiler)
        self.ticket_path = self.target / 'identity-generators' / (ticket['session'] + '.json')
        self.ticket_path.write_text(json.dumps(ticket))
        env = dict(os.environ, RMUX_IDENTITY_TARGET=str(self.target),
                   RMUX_IDENTITY_HEAD=ticket['commit'], RMUX_IDENTITY_SOURCE=ticket['source'],
                   RMUX_IDENTITY_DOC_SESSION=ticket['session'],
                   CARGO_TARGET_DIR=str(self.directory / 'target'))
        return ticket, env

    def test_popen_refuses_missing_or_changed_child_session_for_both_tools(self):
        ticket, env = self.prepare()
        for tool in ('cargo', 'compiler'):
            for key in ('RMUX_NATIVE_INPUT_MANIFEST', 'RMUX_NATIVE_INPUT_MANIFEST_SHA256'):
                for value in (None, '', 'different'):
                    with self.subTest(tool=tool, key=key, value=value), ExitStack() as stack:
                        child_env = dict(env)
                        if value is None:
                            child_env.pop(key)
                        else:
                            child_env[key] = value
                        original = subprocess.Popen
                        stack.enter_context(patch.object(subprocess, 'Popen', original))
                        stack.enter_context(patch.object(process, 'os', SimpleNamespace(**(vars(os) | {'name': 'nt'}))))
                        created = stack.enter_context(patch.object(original, '_execute_child', side_effect=AssertionError('unexpected OS launch')))
                        stack.enter_context(patch.object(guard, 'generation', return_value=ticket))
                        process.install()
                        with self.assertRaises((guard.IdentityError, RuntimeError)):
                            subprocess.Popen([ticket[tool], 'check', '--locked', '--offline'], env=child_env, cwd=self.directory)
                        self.assertEqual(created.call_count, 0)

    @unittest.skipUnless(os.name == 'nt', 'requires native Windows handles')
    def test_valid_child_launches_and_has_a_native_wait(self):
        ticket, env = self.prepare()
        original = subprocess.Popen
        creation = process.creation_popen(original)
        execute = creation._execute_child
        record = native.verify_manifest('qualified', 'manifest-sha')
        record['files'] = {str(Path(ticket['cargo'])): {}}
        with patch.object(subprocess, 'Popen', original), \
                patch.object(creation, '_execute_child', autospec=True, side_effect=execute) as created, \
                patch.object(guard, 'generation', return_value=ticket), \
                patch.object(native, 'verify_manifest', return_value=record):
            process.install()
            with subprocess.Popen([ticket['cargo'], '-c',
                                   'import sys; print(sys.flags.isolated, sys.flags.no_site)'],
                                  env=env, cwd=self.directory, stdout=subprocess.PIPE) as child:
                output, _ = child.communicate()
                self.assertEqual(child.returncode, 0)
            self.assertEqual(output.strip(), b'1 1')
            self.assertEqual(created.call_count, 1)
        waits = list(self.audit.glob('boundary-*/wait.json'))
        self.assertEqual(len(waits), 1)
        receipt = json.loads(waits[0].read_text())
        self.assertEqual((receipt['wait_return'], receipt['exit'], receipt['child']['pid']), (0, 0, child.pid))
        self.assertIsNotNone(receipt['child']['end_ns'])

    def test_effective_child_environment_is_verified(self):
        ticket, env = self.prepare()
        for key in ('RMUX_IDENTITY_HEAD', 'RMUX_IDENTITY_SOURCE'):
            with self.subTest(key=key), patch.object(guard, 'generation', return_value=ticket), \
                    self.assertRaises(guard.IdentityError):
                process.generated_inputs([ticket['cargo'], 'check'], dict(env, **{key: 'foreign'}), self.directory)
        # A foreign parent manifest cannot be substituted for a valid child's.
        with patch.dict(os.environ, {'RMUX_NATIVE_INPUT_MANIFEST': 'other-parent',
                                     'RMUX_NATIVE_INPUT_MANIFEST_SHA256': 'other-parent-digest'}), \
                patch.object(guard, 'generation', return_value=ticket):
            self.assertEqual(set(process.generated_inputs([ticket['cargo'], 'check'], env, self.directory)),
                             {self.manifest, self.code, self.lock})

    def test_initial_metadata_and_frozen_compiler_use_their_retained_version(self):
        ticket, env = self.prepare()
        for phase in ('frozen', 'resolving'):
            with self.subTest(phase=phase):
                version = copy.deepcopy(ticket)
                version['phase'] = phase
                if phase == 'resolving':
                    self.lock.write_bytes(self.seed)
                    version['files']['Cargo.lock'] = guard.file_hash(self.lock)
                    version['native_inputs'] = {}
                    retention.retain(guard, version, [self.manifest, self.code, self.lock], 'initial')
                self.ticket_path.write_text(json.dumps(version))
                args = [version['cargo'], 'metadata', '--offline', '--format-version', '1'] if phase == 'resolving' else [version['compiler'], str(self.code)]
                with patch.object(guard, 'generation', return_value=version):
                    self.assertEqual(set(process.generated_inputs(args, env, self.directory)), {self.manifest, self.code, self.lock})
                    for key in ('RMUX_NATIVE_INPUT_MANIFEST', 'RMUX_NATIVE_INPUT_MANIFEST_SHA256'):
                        child_env = dict(env)
                        child_env.pop(key)
                        with patch.object(process, 'os', SimpleNamespace(**(vars(os) | {'name': 'nt'}))), \
                                self.assertRaises(guard.IdentityError):
                            process.generated_inputs(args, child_env, self.directory)

    def test_valid_child_still_refuses_retained_copy_loss_or_mutation(self):
        ticket, env = self.prepare()
        for phase in ('initial', 'resolved'):
            retained = Path(ticket['native_inputs'][phase]['files'][str(self.manifest)]['object'])
            original = retained.read_bytes()
            retained.chmod(0o600)
            for mutation in ('missing', 'changed'):
                with self.subTest(phase=phase, mutation=mutation):
                    if mutation == 'missing':
                        retained.unlink()
                    else:
                        retained.write_bytes(b'changed')
                    with patch.object(guard, 'generation', return_value=ticket), \
                            self.assertRaises((guard.IdentityError, RuntimeError, FileNotFoundError)):
                        process.generated_inputs([ticket['cargo'], 'check'], env, self.directory)
                    retained.write_bytes(original)

    def test_git_and_nonconsumer_do_not_require_child_manifest(self):
        ticket, env = self.prepare()
        env.pop('RMUX_NATIVE_INPUT_MANIFEST')
        with patch.object(process, 'os', SimpleNamespace(**(vars(os) | {'name': 'nt'}))), \
                patch.object(guard, 'generation', side_effect=AssertionError('recursive validation')):
            git = self.source / 'git-fixture'
            git.write_text('not executed')
            self.assertEqual(process.generated_inputs([str(git)], env, self.directory), ())
            ticket['kind'] = 'rustdoc'
            self.ticket_path.write_text(json.dumps(ticket))
            self.assertEqual(process.generated_inputs([ticket['cargo']], env, self.directory), ())
            ticket['kind'] = 'compile-consumer'
            ticket.pop('native_inputs')
            self.ticket_path.write_text(json.dumps(ticket))
            self.assertEqual(process.generated_inputs([ticket['cargo']], env, self.directory), ())

    def test_unix_without_manifest_preserves_opt_out(self):
        ticket, env = self.prepare()
        env.pop('RMUX_NATIVE_INPUT_MANIFEST')
        with patch.object(process, 'os', SimpleNamespace(**(vars(os) | {'name': 'posix'}))), \
                patch.object(native, 'os', SimpleNamespace(**(vars(os) | {'name': 'posix'}))), \
                patch.object(retention, 'launch_inputs', side_effect=AssertionError('Unix retention entered')):
            self.assertEqual(process.generated_inputs([ticket['cargo']], env, self.directory), ())
            self.assertIsNone(native.before_launch([ticket['cargo']], env))

    def test_direct_before_launch_cannot_drop_active_consumer_manifest(self):
        ticket, env = self.prepare()
        env.pop('RMUX_NATIVE_INPUT_MANIFEST')
        with patch.object(native, 'os', SimpleNamespace(**(vars(os) | {'name': 'nt'}))), \
                patch.object(guard, 'generation', return_value=ticket), \
                self.assertRaises(guard.IdentityError):
            native.before_launch([ticket['cargo']], env, cwd=self.directory)


if __name__ == '__main__':
    unittest.main()
