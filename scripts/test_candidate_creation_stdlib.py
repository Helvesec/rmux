"""Exercise this interpreter's real Windows subprocess method as a Mac model."""
import ast
from contextlib import ExitStack
from pathlib import Path
import subprocess
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import test_candidate_creation_identity as models


class WindowsStdlibCreation(unittest.TestCase):
    def model_class(self, api, events):
        source = Path(subprocess.__file__)
        tree = ast.parse(source.read_text())
        method = next(node for node in ast.walk(tree)
                      if isinstance(node, ast.FunctionDef) and node.name == '_execute_child'
                      and ast.get_docstring(node) == 'Execute program (MS Windows version)')
        namespace = dict(vars(subprocess), _winapi=api, Handle=int,
                         STARTUPINFO=lambda: SimpleNamespace(dwFlags=0, lpAttributeList=None))
        exec(compile(ast.Module(body=[method], type_ignores=[]), str(source), 'exec'), namespace)
        execute = namespace['_execute_child']

        class StdlibProcess(models.ModelPopen):
            _execute_child = execute
            def __init__(self, args, **kwargs):
                parameters = {item.arg: None for item in method.args.args[1:]}
                parameters.update(args=args, pass_fds=(), close_fds=True, shell=False,
                                  creationflags=kwargs.get('creationflags', 0),
                                  p2cread=-1, p2cwrite=-1, c2pread=-1, c2pwrite=-1,
                                  errread=-1, errwrite=-1)
                self.returncode = None
                self._execute_child(**parameters)
            def _close_pipe_fds(self, *args):
                events.append(('stdlib_close_pipes', args))

        return StdlibProcess

    def test_original_windows_method_keeps_handle_ownership_and_captures_before_resume(self):
        native, child, _, events, patches, _ = models.CreationBoundary().model()
        original = models._winapi.CreateProcess
        calls = []
        def create(*args):
            calls.append(args)
            return original(*args)
        models._winapi.CreateProcess = create
        base = self.model_class(models._winapi, events)
        with ExitStack() as stack:
            for item in patches:
                stack.enter_context(item)
            stack.enter_context(patch.object(native.subprocess, 'Popen', base))
            process = native.popen(['model.exe', 'literal argument'], creationflags=0x200)
        self.assertEqual(process.creation_identity, child)
        self.assertEqual((process._handle, process.pid), (900, 81))
        self.assertEqual(calls[0][1], 'model.exe "literal argument"')
        self.assertEqual(calls[0][5], 0x204)
        self.assertEqual(events[-1], ('close', 901))
        self.assertEqual(sum(item == ('close', 901) for item in events), 1)
        self.assertNotIn(('close', 900), events)
        self.assertTrue(any(item[0] == 'stdlib_close_pipes' for item in events))

    def test_original_windows_method_closes_pipes_after_capture_failure(self):
        native, _, _, events, patches, _ = models.CreationBoundary().model(
            capture_error=RuntimeError('capture failed'))
        base = self.model_class(models._winapi, events)
        with ExitStack() as stack:
            for item in patches:
                stack.enter_context(item)
            stack.enter_context(patch.object(native.subprocess, 'Popen', base))
            with self.assertRaisesRegex(RuntimeError, 'capture failed'):
                native.popen(['model.exe'])
        self.assertIn(('cleanup_wait', 900), events)
        self.assertEqual(sum(item == ('close', 900) for item in events), 1)
        self.assertEqual(sum(item == ('close', 901) for item in events), 1)
        self.assertEqual(events[-1][0], 'stdlib_close_pipes')


if __name__ == '__main__':
    unittest.main()
