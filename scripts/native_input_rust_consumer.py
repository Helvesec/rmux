"""The existing Windows checker at Rust's public-consumer launch boundary."""
import json
import os
import sys


def before():
    from native_input_windows import before_launch
    from native_input_windows_process import generated_inputs
    request = json.load(sys.stdin)
    args, cwd = request['args'], request['cwd']
    if (not isinstance(args, list) or not args
            or any(not isinstance(arg, str) for arg in args)
            or not isinstance(cwd, str)):
        raise RuntimeError('Rust consumer requires literal argv and cwd')
    env = dict(os.environ)
    receipt = before_launch(args, env, generated_inputs(args, env, cwd), cwd=cwd)
    if receipt is None:
        raise RuntimeError('Rust consumer lost its native session')
    print(json.dumps(receipt, sort_keys=True))
    return 0
