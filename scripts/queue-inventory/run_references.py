"""Resolve references from fresh Cargo artifacts, then execute exact libtest names."""
import collections
import json
import os
import re
import signal
import subprocess
import sys

from source_scan import require


def run(command, root, timeout=None):
    print('+ ' + ' '.join(map(str, command)), flush=True)
    kwargs = {'start_new_session': True} if os.name != 'nt' else {
        'creationflags': subprocess.CREATE_NEW_PROCESS_GROUP}
    with subprocess.Popen(command, cwd=root, text=True, stdout=subprocess.PIPE,
                          stderr=subprocess.STDOUT, **kwargs,
                          env={**os.environ, 'RUST_MIN_STACK': '8388608'}) as child:
        try:
            output, _ = child.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            if os.name == 'nt':
                subprocess.run(['taskkill', '/PID', str(child.pid), '/T', '/F'],
                               capture_output=True, check=False)
            else:
                os.killpg(child.pid, signal.SIGKILL)
            output, _ = child.communicate()
            raise ValueError(f'timed out after {timeout}s: {command}\n{output}')
        require(child.returncode == 0, output)
        return output


def host_platform():
    # The one platform fact behind selection, runner startup and Windows
    # qualification, so a test that simulates a host simulates all three.
    # run() keeps os.name: it binds real child processes, never simulated.
    return {'darwin': 'macos', 'linux': 'linux', 'win32': 'windows'}[sys.platform]


def native_runner(platform):
    python = os.environ.get('RMUX_IDENTITY_PYTHON')
    script = os.environ.get('RMUX_IDENTITY_SCRIPT')
    if not any(key.startswith('RMUX_IDENTITY_') for key in os.environ):
        return []
    require(python and script, 'incomplete native identity runner context')
    # Match the active harness, including its isolated Windows input mode.
    flags = ['-I', '-S', '-B'] if platform == 'windows' and os.environ.get('RMUX_NATIVE_INPUT_MANIFEST') else []
    return [python, *flags, script, 'run']


def execute(root, profiles, cargo_target=None):
    platform = host_platform()
    runner = native_runner(platform)
    groups = collections.defaultdict(dict)
    for profile in profiles.values():
        for test in profile['tests']:
            group = groups[(test['package'], test['target'])]
            symbol, platforms = test['symbol'], set(test['platforms'])
            require(symbol not in group or group[symbol] == platforms,
                    f'inconsistent native selection for {symbol}')
            group[symbol] = platforms
    plan, excluded, errors = [], [], []
    # Qualify the whole compiled population before executing it. A registry
    # exclusion is valid only if the native binary really omits that test.
    for (package, target), symbols in sorted(groups.items()):
        flags = ['--lib'] if target == 'lib' else ['--' + target.split(':')[0], target.split(':')[1]]
        command = ['cargo', 'test', '-p', package, '--offline', '--locked', *flags,
                   '--no-run', '--message-format=json']
        if cargo_target:
            command += ['--target', cargo_target]
        output = run(command, root)
        artifacts = set()
        for line in output.splitlines():
            if not line.startswith('{'):
                continue
            message = json.loads(line)
            if message.get('reason') == 'compiler-artifact' and message.get('executable') and message['profile']['test']:
                artifacts.add(message['executable'])
        require(len(artifacts) == 1, f'{package}/{target}: expected one test artifact, got {artifacts}')
        binary = artifacts.pop()
        if runner and platform == 'windows':
            # A Windows runner proves the image's whole transitive graph before
            # launch and caches that proof only once it completes (92-120 s cold
            # on the Windows host). Qualify outside the libtest deadline, as the build above,
            # so a timeout kill cannot leave every retry cold. Unix `check`
            # differs from `run`'s pre-launch proof, so Unix is unchanged.
            run([*runner[:-1], 'check', binary], root)
        listing = run([*runner, binary, '--list', '--format', 'terse'], root, 60)
        names = [line[:-6] for line in listing.splitlines() if line.endswith(': test')]
        for symbol, platforms in sorted(symbols.items()):
            matches = [name for name in names if name.split('::')[-1] == symbol]
            if platform not in platforms:
                if matches:
                    errors.append(f'{package}/{target}: incorrectly excluded native test: {symbol}')
                excluded.append((package, target, symbol))
            elif len(matches) != 1:
                errors.append(f'{package}/{target}: expected one native test for {symbol}, got {matches}')
            else:
                plan.append((package, target, binary, matches[0]))
    require(not errors, '\n'.join(errors))
    if any(package == 'rmux-sdk' and target == 'test:terminal_automation'
           for package, target, _, _ in plan):
        # This existing fixture starts the actual daemon; keep its build outside
        # the per-test execution deadline, including when this gate runs alone.
        build = ['cargo', 'build', '-p', 'rmux', '--bin', 'rmux', '--offline', '--locked']
        if cargo_target:
            build += ['--target', cargo_target]
        run(build, root)
    for package, target, binary, name in plan:
        output = run([*runner, binary, name, '--exact', '--nocapture', '--test-threads=1'], root, 60)
        require(re.search(r'test result: ok\. 1 passed; 0 failed; 0 ignored;', output),
                f'{name}: no executed passing test\n{output}')
        print(f'PASS {package} {name}', flush=True)
    require(plan, 'no native queue tests executed')
    return {'product_tests_executed': True, 'native_platform': platform,
            'passed': len(plan), 'other_platform_references': len(excluded)}
