"""macOS boundary for the existing bank; reuse its qualified Darwin collectors."""
import errno
import os
import re
import shlex
import time

import default_shell_darwin as darwin


def identity(pid):
    row = darwin.acquired.bsd(pid)
    if row is None:
        return None
    return {'pid': row.pid, 'ppid': row.ppid,
            'start_ticks': str(row.start_sec * 1000000 + row.start_usec),
            'birth_method': 'darwin-start-time-microseconds',
            'birth_time': [row.start_sec, row.start_usec],
            'state': {1: 'I', 2: 'R', 3: 'S', 4: 'T', 5: 'Z'}.get(row.status, 'X')}


def same(a, b):
    return bool(a and b and a['pid'] == b['pid'] and a['start_ticks'] == b['start_ticks'])


def describe(pid):
    birth = identity(pid)
    if birth is None:
        raise ProcessLookupError(pid)
    deadline = time.monotonic() + 1
    while True:
        try:
            record = snapshot(pid)
            if record is not None:
                if not same(birth, record['identity']):
                    raise ProcessLookupError('macOS birth changed during exec readiness')
                return record
        except OSError as error:
            if error.errno not in (errno.EIO, errno.EINVAL, errno.ESRCH, errno.ENOENT, errno.EBADF):
                raise
            current = identity(pid)
            if not same(birth, current) or current['state'] == 'Z':
                raise ProcessLookupError(pid) from error
            if time.monotonic() >= deadline:
                raise
        if time.monotonic() >= deadline:
            raise RuntimeError('bounded macOS exec snapshot readiness')
        time.sleep(.005)


def snapshot(pid):
    before = identity(pid)
    if before is None:
        raise ProcessLookupError(pid)
    image = darwin.acquired.identity(pid)
    args = darwin.procargs(pid)['argv']
    cwd = darwin.cwd(pid)['path']
    fd0 = darwin.acquired.fd_identity(pid, 0)
    if not same(before, identity(pid)):
        raise RuntimeError('incoherent macOS process identity')
    after_image = darwin.acquired.identity(pid)
    if any(image[key] != after_image[key] for key in ('executable', 'sha256')):
        return None
    return {'identity': before, 'exe': image['executable'], 'exe_sha256': image['sha256'],
            'argv': args, 'cwd': cwd, 'fd0': fd0['path'], 'fd0_native': fd0,
            'image_method': 'libproc-path-and-disk-sha256'}


children = darwin.children
fds = darwin.fd_snapshot


class ExitWatch:
    """Expose the bank's bounded poll interface over retained kqueue NOTE_EXIT."""
    def __init__(self, pid):
        self.birth = identity(pid)
        if self.birth is None:
            raise ProcessLookupError(pid)
        self.native = darwin.native.FixtureProcess(pid)
        if not same(self.birth, identity(pid)):
            self.native.close()
            raise ProcessLookupError('birth changed during kqueue registration')

    def poll(self, timeout=0):
        end = time.monotonic() + timeout / 1000
        while True:
            if self.native.poll_exit():
                return [(self.native._queue.fileno(), 'NOTE_EXIT')]
            if time.monotonic() >= end:
                return []
            time.sleep(min(.01, max(0, end-time.monotonic())))

    def close(self):
        self.native.close()


def open_watch(pid):
    watch = ExitWatch(pid)
    return watch.native._queue.fileno(), watch


def send_signal(fd, watch, birth, signum):
    if fd != watch.native._queue.fileno() or watch.poll(0) or not same(birth, identity(birth['pid'])):
        raise RuntimeError('stale macOS signal target')
    # Darwin has no pidfd_send_signal: NOTE_EXIT and start time guard owned PIDs.
    os.kill(birth['pid'], signum)


def validate_birth(birth):
    value = birth.get('birth_time')
    if (birth.get('birth_method') != 'darwin-start-time-microseconds'
            or not isinstance(value, list) or len(value) != 2
            or any(type(item) is not int for item in value)
            or value[0] <= 0 or not 0 <= value[1] < 1000000
            or birth['start_ticks'] != str(value[0] * 1000000 + value[1])):
        raise RuntimeError('macOS native birth binding')


def valid_tty(path):
    return isinstance(path, str) and re.fullmatch(r'/dev/ttys[0-9a-f]+', path) is not None


def consumer_paths(plan, contract):
    """Substitute native interpreter coordinates; preserve payloads/assertions."""
    executable = contract['python']['path']
    for row in plan:
        row['argv'] = [arg.replace('/usr/bin/python3 -c ', shlex.quote(executable) + ' -c ')
                       if isinstance(arg, str) else arg for arg in row['argv']]
        if row.get('consumer_exe') == '/usr/bin/python3':
            row['consumer_exe'] = executable
            row['consumer_argv'] = [executable, *row['consumer_argv'][1:]]
        if row['role'].startswith('default-shell:'):
            row['argv'] = [arg.replace('[ -n "${BASH_VERSION-}" ]', '[ "${0##*/}" = bash ]')
                           for arg in row['argv']]
        if row['role'].startswith('status-interval:') and row['role'].endswith(('-seed', '-template', '-change')):
            # A fresh format-job key binds FIRST to this native consumer, rather
            # than a previous transition's still cached asynchronous result.
            phase = row['role'].split(':', 1)[1].rsplit('-', 1)[0]
            row['argv'][-1] = row['argv'][-1].replace('ticker', 'ticker-' + phase)


EXIT_METHOD = 'kqueue-NOTE_EXIT'
ENDPOINT_ERRORS = (errno.ENOENT, errno.ECONNREFUSED)
