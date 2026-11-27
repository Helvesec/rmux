"""Portable simulations for creation-held identity and sibling launch consumers."""
from contextlib import ExitStack
import ctypes
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import Mock, patch

import test_candidate_creation_identity as models
from test_candidate_creation_identity import ModelPopen, load


class HandleIdentity(unittest.TestCase):
    def setUp(self):
        self.native = load('native_input_windows_process')
        self.events = []
        self.child = dict(pid=81, birth_100ns=116444736000000123,
                          birth_ns=12300, end_ns=None, image=str(Path(__file__).resolve()))
        self.parent = dict(pid=1, birth_100ns=116444736000000010,
                           birth_ns=1000, end_ns=None, image=self.child['image'])
        self.end = 0

    def api(self):
        def times(handle, birth, end, kernel, user):
            ticks = self.child['birth_100ns']
            for pointer, value in ((birth, ticks), (end, self.end)):
                pointer._obj.dwLowDateTime = value & 0xffffffff
                pointer._obj.dwHighDateTime = value >> 32
            return True

        def image(handle, flags, buffer, length):
            self.events.append(('image', handle))
            buffer.value = self.child['image']
            return True

        def wait(handle, timeout):
            self.events.append(('wait', handle))
            self.end = self.child['birth_100ns'] + 100
            return 0

        def exit_code(handle, code):
            code._obj.value = 0
            return True

        return SimpleNamespace(GetProcessTimes=times, GetProcessId=lambda _: 81,
                               QueryFullProcessImageNameW=image, WaitForSingleObject=wait,
                               GetExitCodeProcess=exit_code)

    def test_live_handle_is_captured_once_and_ended_snapshot_retains_image(self):
        with patch.object(self.native, 'kernel', return_value=self.api()):
            first = self.native.identity(900)
            self.assertEqual(first, self.child)
            self.end = self.child['birth_100ns'] + 100
            after = self.native.identity(900, image=first['image'])
        self.assertEqual(after['image'], first['image'])
        self.assertEqual(after['end_ns'], 22300)
        self.assertEqual(self.events, [('image', 900)])

    def test_ended_handle_never_performs_an_image_query(self):
        self.end = self.child['birth_100ns'] + 100
        with patch.object(self.native, 'kernel', return_value=self.api()), \
                self.assertRaisesRegex(ProcessLookupError, 'after process exit'):
            self.native.identity(900)
        self.assertEqual(self.events, [])

    def test_ended_descendant_keeps_observer_disappearance_handling(self):
        observer = load('candidate_conpty_windows')
        self.end = self.child['birth_100ns'] + 100
        closed = []
        kernel = SimpleNamespace(GetCurrentProcess=lambda: 42,
            WaitForSingleObject=lambda handle, timeout: 0x102,
            OpenProcess=lambda *args: 901, CloseHandle=closed.append)
        psapi = SimpleNamespace(GetProcessMemoryInfo=lambda *args: True)
        runtime = SimpleNamespace(manifest=lambda g: (dict(files=[]), 'model'))
        guard = SimpleNamespace(support=lambda name: self.native if name == 'native_input_windows_process' else runtime)
        child = dict(pid=1, birth_ns=1, image=self.child['image'])
        with patch.object(self.native, 'kernel', return_value=self.api()), \
                patch.object(observer, 'api', return_value=(kernel, psapi)), \
                patch.object(observer, 'processes', return_value=[(81, 1)]), \
                patch.object(observer, 'modules', return_value=[]):
            result = observer.observe(guard, SimpleNamespace(_handle=900, poll=lambda: 0),
                                      child, Path('.'), dict(configuration='system'))
        self.assertEqual(result['errors'], [])
        self.assertEqual(closed, [901])
        self.assertEqual(self.events, [])

    def test_missing_pid_or_birth_refuses_before_image_query(self):
        for missing in ('pid', 'birth'):
            api = self.api()
            if missing == 'pid':
                api.GetProcessId = lambda _: 0
            else:
                self.child['birth_100ns'] = 0
            with self.subTest(missing=missing), patch.object(self.native, 'kernel', return_value=api), \
                    self.assertRaises(RuntimeError):
                self.native.identity(900)
        self.assertEqual(self.events, [])

    def test_wait_uses_original_handle_and_never_rediscovers_image(self):
        before = dict(case='model', head='a' * 40, executable_sha256='b' * 64, at_ns=1)
        with patch.object(self.native, 'kernel', return_value=self.api()), \
                patch.object(self.native, 'current', return_value=self.parent):
            receipt = self.native.wait_receipt(SimpleNamespace(_handle=900), before,
                                                self.parent, self.child, 2)
        self.assertEqual(receipt['child']['pid'], self.child['pid'])
        self.assertEqual(receipt['child']['birth_100ns'], self.child['birth_100ns'])
        self.assertEqual(receipt['child']['image'], self.child['image'])
        self.assertEqual(receipt['wait_return'], 0)
        self.assertEqual(self.events, [('wait', 900)])

    def test_pid_or_birth_mismatch_before_and_after_wait_is_fatal(self):
        for when in ('before', 'after'):
            for key in ('pid', 'birth_100ns'):
                bad = dict(self.child, **{key: self.child[key] + 1})
                snapshots = [bad] if when == 'before' else [self.child, dict(bad, end_ns=22300)]
                with self.subTest(when=when, key=key), \
                        patch.object(self.native, 'kernel', return_value=self.api()), \
                        patch.object(self.native, 'identity', side_effect=snapshots) as captured, \
                        patch.object(self.native, 'current', return_value=self.parent), \
                        self.assertRaisesRegex(RuntimeError, 'identity changed'):
                    self.native.wait_receipt(SimpleNamespace(_handle=900), {},
                                             self.parent, self.child, 2)
                for call in captured.call_args_list:
                    self.assertEqual(call.args, (900,))
                    self.assertEqual(call.kwargs, dict(image=self.child['image']))

    def test_parent_pid_or_birth_mismatch_is_fatal(self):
        for key in ('pid', 'birth_100ns'):
            parent = dict(self.parent, **{key: self.parent[key] + 1})
            with self.subTest(key=key), patch.object(self.native, 'kernel', return_value=self.api()), \
                    patch.object(self.native, 'current', return_value=parent), \
                    self.assertRaisesRegex(RuntimeError, 'matching parent birth'):
                self.native.wait_receipt(SimpleNamespace(_handle=900), {},
                                         self.parent, self.child, 2)

    def test_wait_failure_or_missing_exit_code_is_fatal(self):
        for failure in ('wait', 'exit'):
            api = self.api()
            if failure == 'wait':
                api.WaitForSingleObject = lambda *args: 0xffffffff
            else:
                api.GetExitCodeProcess = lambda *args: False
            with self.subTest(failure=failure), patch.object(self.native, 'kernel', return_value=api), \
                    self.assertRaisesRegex(RuntimeError, 'WAIT_OBJECT_0'):
                self.native.wait_receipt(SimpleNamespace(_handle=900), {},
                                         self.parent, self.child, 2)


class CreationFailures(unittest.TestCase):
    def test_empty_capture_is_fatal_and_cleanup_waited(self):
        for result in (None, {}):
            native, _, _, events, patches, _ = models.CreationBoundary().model()
            with self.subTest(result=result), ExitStack() as stack:
                for item in patches:
                    stack.enter_context(item)
                stack.enter_context(patch.object(native, 'identity', return_value=result))
                with self.assertRaises(RuntimeError):
                    native.popen(['model'])
            self.assertIn(('cleanup_wait', 900), events)
            self.assertEqual(events[-2:], [('close', 901), ('close', 900)])

    def test_create_failure_never_attempts_to_wait_or_close_unowned_handles(self):
        native, _, _, events, patches, _ = models.CreationBoundary().model()
        error = OSError(5, 'simulated creation failure')
        with ExitStack() as stack:
            for item in patches:
                stack.enter_context(item)
            stack.enter_context(patch.object(models._winapi, 'CreateProcess', side_effect=error))
            with self.assertRaises(OSError) as caught:
                native.popen(['model'])
        self.assertIs(caught.exception, error)
        self.assertEqual(events, [])

    def test_capture_interruption_is_cleaned_up_before_propagation(self):
        error = KeyboardInterrupt('simulated interruption')
        model = models.CreationBoundary().model(capture_error=error)
        with self.assertRaises(KeyboardInterrupt) as caught:
            models.CreationBoundary().launch_model(model)
        self.assertIs(caught.exception, error)
        self.assertIn(('cleanup_wait', 900), model[3])

    def test_already_ended_child_is_waited_when_terminate_returns_false(self):
        native, _, _, events, patches, _ = models.CreationBoundary().model(resume=0)
        # Override the kernel fixture explicitly; no native calls occur.
        api = SimpleNamespace(ResumeThread=lambda h: 0, TerminateProcess=lambda *a: False,
                              WaitForSingleObject=lambda h, timeout: events.append(('ended_wait', h, timeout)) or 0)
        with ExitStack() as stack:
            for item in patches:
                stack.enter_context(item)
            stack.enter_context(patch.object(native, 'kernel', return_value=api))
            with self.assertRaisesRegex(RuntimeError, 'resume failed'):
                native.popen(['model'])
        self.assertIn(('ended_wait', 900, 0), events)
        self.assertEqual(events[-2:], [('close', 901), ('close', 900)])

    def test_cleanup_wait_failure_remains_fatal_and_handles_are_closed(self):
        native, _, _, events, patches, _ = models.CreationBoundary().model(resume=0)
        api = SimpleNamespace(ResumeThread=lambda h: 0, TerminateProcess=lambda *a: True,
                              WaitForSingleObject=lambda *a: 0xffffffff)
        with ExitStack() as stack:
            for item in patches:
                stack.enter_context(item)
            stack.enter_context(patch.object(native, 'kernel', return_value=api))
            with self.assertRaisesRegex(RuntimeError, 'cleanup wait failed'):
                native.popen(['model'])
        self.assertEqual(events[-2:], [('close', 901), ('close', 900)])

    def test_literal_argv_environment_cwd_and_streams_survive_the_boundary(self):
        native, _, _, _, patches, _ = models.CreationBoundary().model()
        original = ModelPopen.__init__
        calls = []
        def constructor(self, args, **kwargs):
            calls.append((args, kwargs))
            original(self, args, **kwargs)
        argv, environment, stream = ['model', 'literal argument'], {'MODEL': 'value'}, object()
        with ExitStack() as stack:
            for item in patches:
                stack.enter_context(item)
            stack.enter_context(patch.object(ModelPopen, '__init__', constructor))
            native.popen(argv, env=environment, cwd='literal-directory', stdout=stream)
        self.assertEqual(calls, [(argv, dict(env=environment, cwd='literal-directory', stdout=stream))])


class LaunchConsumers(unittest.TestCase):
    def test_cached_guard_support_uses_original_creation_handle_once(self):
        guard = load('candidate_identity')
        native = guard.support('native_input_windows_process')
        runtime = guard.support('candidate_conpty_runtime')
        self.assertIs(guard.support('native_input_windows_process'), native)
        model = models.CreationBoundary().model()
        _, child, _, events, _, _ = model
        import test_candidate_creation_identity as model_module
        observer = SimpleNamespace(observe=Mock(return_value=dict(pair_loaded=False, errors=[])))
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(native.subprocess, 'Popen', ModelPopen), \
                patch.object(native, 'kernel', return_value=SimpleNamespace(ResumeThread=lambda h: 1)), \
                patch.object(native, 'identity', return_value=child) as capture, \
                patch.object(native, 'current', return_value=dict(pid=1, birth_100ns=1)), \
                patch.object(native, 'wait_receipt', return_value=dict(exit=0)) as waited, \
                patch.object(runtime, 'stage', return_value=dict(configuration='system')):
            # Use the exact adjacent modules returned by the real cached loader.
            model_module._winapi.CreateProcess = lambda *args: (900, 901, 81, 82)
            guard.support.cache_clear()
            guard.support = lambda name: native if name == 'native_input_windows_process' else observer
            record = dict(path=child['image'], target=directory, commit='a' * 40, sha256='b' * 64)
            self.assertEqual(runtime.launch(guard, record, []), 0)
            capture.assert_called_once_with(900)
            self.assertIs(waited.call_args.args[3], child)

    def test_observed_popen_retains_creation_identity_and_writes_one_wait(self):
        import native_input_windows as inputs
        native, child, _, _, patches, _ = models.CreationBoundary().model()
        with tempfile.TemporaryDirectory() as directory, ExitStack() as stack:
            for item in patches:
                stack.enter_context(item)
            stack.enter_context(patch.dict(native.os.environ, RMUX_NATIVE_INPUT_MANIFEST='model'))
            stack.enter_context(patch.object(native, 'os', SimpleNamespace(name='nt', environ=native.os.environ)))
            before = dict(executable=child['image'], files={'model': dict(object=str(Path(directory) / 'objects/item'))})
            stack.enter_context(patch.object(inputs, 'before_launch', return_value=before))
            stack.enter_context(patch.object(native, 'generated_inputs', return_value=()))
            stack.enter_context(patch.object(native, 'current', return_value=dict(pid=1, birth_100ns=1)))
            waited = stack.enter_context(patch.object(native, 'wait_receipt', return_value=dict(exit=0)))
            written = stack.enter_context(patch.object(inputs, 'write_once'))
            native.install()
            installed = native.subprocess.Popen
            native.install()
            self.assertIs(native.subprocess.Popen, installed)
            with native.popen([child['image']]) as process:
                self.assertIs(process._native_child, process.creation_identity)
                process.wait()
                process.wait()
            waited.assert_called_once()
            written.assert_called_once()
            self.assertEqual(written.call_args.args[0], Path(directory) / 'wait.json')

    def test_unobserved_sibling_keeps_original_launch_path(self):
        import native_input_windows as inputs
        native, _, state, events, patches, _ = models.CreationBoundary().model()
        with ExitStack() as stack:
            for item in patches:
                stack.enter_context(item)
            stack.enter_context(patch.dict(native.os.environ, RMUX_NATIVE_INPUT_MANIFEST='model'))
            stack.enter_context(patch.object(native, 'os', SimpleNamespace(name='nt', environ=native.os.environ)))
            stack.enter_context(patch.object(inputs, 'before_launch', return_value=None))
            stack.enter_context(patch.object(native, 'generated_inputs', return_value=()))
            native.install()
            with native.subprocess.Popen(['git-model']) as process:
                self.assertIsNone(process.creation_identity)
        self.assertTrue(state['ended'])
        self.assertFalse(any(event[0] in ('identity', 'resume') for event in events))

    def test_observer_exception_still_completes_original_context_wait(self):
        runtime = load('candidate_conpty_runtime')
        child = dict(pid=81, birth_100ns=123, image='model.exe')
        process = Mock(_handle=900, creation_identity=child)
        process.wait.return_value = None
        process.__enter__ = Mock(return_value=process)
        process.__exit__ = Mock(side_effect=lambda *args: process.wait())
        native = SimpleNamespace(popen=Mock(return_value=process), current=lambda: {})
        observer = SimpleNamespace(observe=Mock(side_effect=ValueError('observer failed')))
        guard = SimpleNamespace(support=lambda name: native if name == 'native_input_windows_process' else observer)
        with patch.object(runtime, 'stage', return_value=dict(configuration='system')), \
                self.assertRaisesRegex(ValueError, 'observer failed'):
            runtime.launch(guard, dict(path='model.exe', target='.', commit='a' * 40, sha256='b' * 64), [])
        process.wait.assert_called_once()


if __name__ == '__main__':
    unittest.main()
