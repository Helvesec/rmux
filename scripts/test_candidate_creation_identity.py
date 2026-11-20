"""Frozen creation/exit race models; simulations do not prove Windows behavior."""
import ctypes
import importlib.util
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch


SCRIPTS = Path(__file__).resolve().parent


def load(name):
    spec = importlib.util.spec_from_file_location('creation_model_' + name, SCRIPTS / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class CreationIdentity(unittest.TestCase):
    def test_direct_conpty_immediate_exit_never_queries_ended_image(self):
        runtime = load('candidate_conpty_runtime')
        native = load('native_input_windows_process')
        events = []
        child = dict(pid=81, birth_100ns=123, birth_ns=12300, end_ns=None, image='model.exe')

        class Process:
            _handle = 900
            pid = 81
            creation_identity = child
            def __init__(self, *args, **kwargs):
                events.append('create_capture_resume_exit')
            def __enter__(self):
                return self
            def wait(self):
                events.append('original_wait')
                return 0
            def __exit__(self, *args):
                self.wait()

        def late(*args, **kwargs):
            events.append('forbidden_late_image_query')
            raise AssertionError('image queried after physical exit')

        # Original source calls identity after Popen has already exited. The
        # corrected source consumes the identity retained at CreateProcess.
        native.popen = Process
        native.current = lambda: dict(pid=1, birth_100ns=10)
        native.identity = late
        native.wait_receipt = lambda *args: dict(exit=0, child=child)
        observer = SimpleNamespace(observe=lambda *args: dict(pair_loaded=False, errors=[]))
        guard = SimpleNamespace(IdentityError=RuntimeError,
                                support=lambda name: native if name == 'native_input_windows_process' else observer)
        with tempfile.TemporaryDirectory() as directory:
            record = dict(path=str(Path(directory) / 'model.exe'), target=directory,
                          commit='a' * 40, sha256='b' * 64)
            with patch.object(runtime, 'stage', return_value=dict(configuration='system')), \
                    patch.object(runtime.subprocess, 'Popen', Process):
                self.assertEqual(runtime.launch(guard, record, []), 0)
        self.assertNotIn('forbidden_late_image_query', events)
        self.assertIn('original_wait', events)


# Windows-shaped stdlib launch boundary, executed without a Windows process.
_winapi = None


class ModelPopen:
    def __init__(self, args, **kwargs):
        self.returncode = None
        self._execute_child(args, kwargs.get('creationflags', 0))

    def _execute_child(self, args, flags):
        hp, ht, self.pid, _ = _winapi.CreateProcess(None, args, None, None, False,
                                                   flags, None, None, None)
        self._handle = hp
        _winapi.CloseHandle(ht)

    def wait(self, timeout=None):
        self.returncode = 0
        return 0

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.wait()


class CreationBoundary(unittest.TestCase):
    def model(self, *, flags=0, resume=1, capture_error=None, identity_change=None):
        native = load('native_input_windows_process')
        events = []
        state = dict(suspended=False, ended=False)
        child = dict(pid=81, birth_100ns=123, birth_ns=12300, end_ns=None,
                     image=str(Path(__file__).resolve()))

        def create(*args):
            events.append(('create', args[5]))
            state['suspended'] = bool(args[5] & 4)
            if not state['suspended']:
                state['ended'] = True
            return 900, 901, 81, 82

        def identity(handle):
            self.assertEqual(handle, 900)
            events.append(('identity', state['suspended'], state['ended']))
            self.assertTrue(state['suspended'], 'identity captured after execution')
            if capture_error:
                raise capture_error
            return dict(child, **(identity_change or {}))

        def resume_thread(handle):
            self.assertEqual(handle, 901)
            events.append(('resume', handle))
            state['suspended'] = False
            state['ended'] = True
            return resume

        def terminate(handle, code):
            events.append(('terminate', handle))
            state['ended'] = True
            return True

        def wait(handle, timeout):
            events.append(('cleanup_wait', handle))
            return 0

        api = SimpleNamespace(ResumeThread=resume_thread, TerminateProcess=terminate,
                              WaitForSingleObject=wait)
        winapi = SimpleNamespace(CreateProcess=create,
                                 CloseHandle=lambda h: events.append(('close', h)))
        global _winapi
        _winapi = winapi
        patches = [patch.object(native, 'identity', side_effect=identity),
                   patch.object(native, 'kernel', return_value=api),
                   patch.object(native.subprocess, 'Popen', ModelPopen)]
        return native, child, state, events, patches, flags

    def launch_model(self, model, **kwargs):
        from contextlib import ExitStack
        native, child, state, events, patches, flags = model
        with ExitStack() as stack:
            for item in patches:
                stack.enter_context(item)
            process = native.popen(['model.exe'], creationflags=flags, **kwargs)
        return process

    def test_creation_captures_before_resume_and_keeps_exact_identity(self):
        model = self.model(flags=0x200)
        process = self.launch_model(model)
        self.assertEqual(process.creation_identity, model[1])
        self.assertTrue(model[2]['ended'])
        self.assertEqual(model[3][:3], [('create', 0x204), ('identity', True, False), ('resume', 901)])
        self.assertEqual(model[3][-1], ('close', 901))

    def test_explicit_suspension_is_preserved(self):
        model = self.model(flags=4)
        process = self.launch_model(model)
        self.assertEqual(process.creation_identity, model[1])
        self.assertTrue(model[2]['suspended'])
        self.assertFalse(any(event[0] == 'resume' for event in model[3]))

    def test_image_error_31_and_other_errors_are_fatal_and_waited(self):
        for code in (31, 5):
            error = OSError(code, 'simulated image error')
            model = self.model(capture_error=error)
            with self.subTest(code=code), self.assertRaises(OSError) as caught:
                self.launch_model(model)
            self.assertIs(caught.exception, error)
            self.assertIn(('cleanup_wait', 900), model[3])
            self.assertEqual(model[3][-2:], [('close', 901), ('close', 900)])
            self.assertFalse(any(event[0] == 'resume' for event in model[3]))

    def test_empty_or_wrong_pid_birth_or_ended_identity_is_refused(self):
        for change in ({'pid': 82}, {'birth_100ns': 0}, {'image': ''}, {'end_ns': 1}):
            model = self.model(identity_change=change)
            with self.subTest(change=change), self.assertRaises(RuntimeError):
                self.launch_model(model)
            self.assertIn(('cleanup_wait', 900), model[3])
            self.assertFalse(any(event[0] == 'resume' for event in model[3]))

    def test_qualified_image_mismatch_is_refused_before_resume(self):
        model = self.model()
        with self.assertRaises(RuntimeError):
            self.launch_model(model, creation_image=Path(__file__).with_name('foreign.exe'))
        self.assertIn(('cleanup_wait', 900), model[3])
        self.assertFalse(any(event[0] == 'resume' for event in model[3]))

    def test_resume_failure_terminates_waits_and_closes_original_handles(self):
        for result in (0, 2, 0xffffffff):
            model = self.model(resume=result)
            with self.subTest(result=result), self.assertRaises(RuntimeError):
                self.launch_model(model)
            self.assertIn(('terminate', 900), model[3])
            self.assertIn(('cleanup_wait', 900), model[3])
            self.assertEqual(model[3][-2:], [('close', 901), ('close', 900)])

    def test_stdlib_api_is_never_mutated_globally(self):
        model = self.model()
        original_create = _winapi.CreateProcess
        self.launch_model(model)
        self.assertIs(_winapi.CreateProcess, original_create)

    def test_nested_support_modules_reuse_the_creation_boundary(self):
        model = self.model()
        native, _, _, events, patches, _ = model
        from contextlib import ExitStack
        with ExitStack() as stack:
            for item in patches:
                stack.enter_context(item)
            first = native.creation_popen(ModelPopen)
            other = load('native_input_windows_process')
            self.assertIs(other.creation_popen(first), first)
        self.assertEqual(events, [])


if __name__ == '__main__':
    unittest.main()
