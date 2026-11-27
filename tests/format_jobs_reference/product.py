"""RMUX profile-isolation and cancellation contracts (stronger than tmux's cache)."""
import shlex
import time
from support import Client
from scenarios import control, display, settle, setup


def profiles(c):
    client = setup(c)
    c.new('t')
    for session, value in [('s', 'ALPHA'), ('t', 'BETA')]:
        c.q('set-environment', '-t', session, 'FJ_VALUE', value)
    fmt = '#(echo "$FJ_VALUE")'
    for session, expected in [('s', 'ALPHA'), ('t', 'BETA'), ('s', 'ALPHA')]:
        settle(c, client, fmt, expected, session)
    c.q('set-environment', '-t', 's', 'FJ_VALUE', 'GAMMA')
    c.require(display(c, client, fmt, 's') == [''], 'environment change cannot show old-profile output')
    settle(c, client, fmt, 'GAMMA', 's')
    settle(c, client, fmt, 'BETA', 't')
    slow = '#(sleep .3; echo "$FJ_VALUE")'
    display(c, client, slow, 's')
    c.q('set-environment', '-t', 's', 'FJ_VALUE', 'DELTA')
    display(c, client, slow, 's')
    settle(c, client, slow, 'DELTA', 's')
    time.sleep(.15)
    c.require(display(c, client, slow, 's') == ['DELTA'], 'old environment worker cannot publish late')
    for index in [1, 2]:
        cwd = (c.out / ('cwd-' + str(index))).resolve()
        cwd.mkdir()
        c.new_window('s:' + str(index), cwd=cwd)
        target = 's:' + str(index) + '.0'
        settle(c, client, '#(pwd)', str(cwd), target)
    other = Client(c, 't', True)
    control(c, client, 'refresh-client -B ' + shlex.quote('profile::' + fmt))
    control(c, other, 'refresh-client -B ' + shlex.quote('profile::' + fmt))
    c.until(lambda: b': DELTA' in client.drains[0].raw and b': BETA' in other.drains[0].raw,
            'separate B profile ownership', 4)
    c.require(True, 'B derives the live session environment per owner')


def retirement(c):
    client = setup(c)
    rejected_started = (c.out / 'rejected-started').resolve()
    rejected_finished = (c.out / 'rejected-finished').resolve()
    rejected_command = ('echo start > ' + shlex.quote(str(rejected_started)) +
                        '; sleep .35; echo kept > ' + shlex.quote(str(rejected_finished)) +
                        '; echo KEPT')
    control(c, client, 'refresh-client -B ' + shlex.quote(
        'kept::#(' + rejected_command + ')'))
    c.until(rejected_started.exists, 'retained subscription job started', 3)
    oversized = 'refresh-client ' + ' '.join(
        '-B ' + shlex.quote('rejected-' + str(index) + '::value') for index in range(257))
    start = len(client.drains[0].raw)
    token = 'FJREJECT' + str(time.monotonic_ns())
    client.p.stdin.write((oversized + '\ndisplay-message -p ' + token + '\n').encode())
    client.p.stdin.flush()
    c.until(lambda: token.encode() in client.drains[0].raw[start:], 'rejected batch boundary', 3)
    rejected_lines = bytes(client.drains[0].raw[start:]).decode(errors='replace').splitlines()
    c.require(any(line.startswith('%error') for line in rejected_lines),
              'oversized B batch is rejected atomically')
    c.until(rejected_finished.exists, 'rejected batch preserves the old owner', 2)
    c.until(lambda: b': KEPT' in client.drains[0].raw[start:],
            'retained owner still publishes after rejection', 3)

    replaced_started = (c.out / 'replaced-started').resolve()
    replaced_finished = (c.out / 'forbidden-replaced-finish').resolve()
    replaced_command = ('echo start > ' + shlex.quote(str(replaced_started)) +
                        '; sleep .6; echo late > ' + shlex.quote(str(replaced_finished)))
    control(c, client, 'refresh-client -B ' + shlex.quote(
        'replace::#(' + replaced_command + ')'))
    c.until(replaced_started.exists, 'replaced subscription job started', 3)
    control(c, client, "refresh-client -B 'replace::#(echo CURRENT)'")
    c.until(lambda: b': CURRENT' in client.drains[0].raw, 'replacement generation publishes', 4)
    time.sleep(.7)
    c.require(not replaced_finished.exists(), 'committed B replacement cancels the old owner')

    started = (c.out / 'started').resolve()
    finished = (c.out / 'forbidden-finish').resolve()
    command = 'echo start > ' + shlex.quote(str(started)) + '; sleep .6; echo late > ' + shlex.quote(str(finished))
    control(c, client, 'refresh-client -B ' + shlex.quote('retire::#(' + command + ')'))
    c.until(started.exists, 'subscription job actually started', 3)
    control(c, client, 'refresh-client -B retire')
    time.sleep(.75)
    c.require(not finished.exists(), 'committed B removal cancels the obsolete process')
    # New definition generation under the same name must remain usable.
    control(c, client, "refresh-client -B 'retire::#(echo ALIVE)'")
    c.until(lambda: b': ALIVE' in client.drains[0].raw, 'B re-add new generation', 4)
    c.require(True, 'B re-add cannot inherit retired execution state')

PRODUCT_SCENARIOS = dict(profiles=profiles, retirement=retirement)


def occurrence_stability(c):
    client = setup(c)
    c.q('set', '-g', 'status-interval', '60')
    c.new_window('s:1')
    c.q('link-window', '-d', '-s', 's:0', '-t', 's:2')
    marker = (c.out / 'w-launches').resolve()
    job = ('echo #{window_index} >> ' + shlex.quote(str(marker)) +
           '; sleep .1; printf #{window_index}')
    c.q('set', '-g', 'status-format[0]', '#{W:#(' + job + ');}')
    attached = Client(c, 's', False, cols=100)
    c.wait_clients([client, attached])
    c.until(lambda: b'0;1;2;' in attached.drains[0].raw, 'linked W status result', 4)
    c.until(lambda: marker.exists() and len(marker.read_text().splitlines()) == 3,
            'one worker per linked W occurrence', 2)
    time.sleep(.2)
    render_bytes = len(attached.drains[0].raw)
    time.sleep(.8)
    c.require(sorted(marker.read_text().splitlines()) == ['0', '1', '2'],
              'linked W workers stay idle before TTL')
    c.require(len(attached.drains[0].raw) == render_bytes,
              'obsolete finishes do not drive a passive render loop')

    c.q('set', '-gu', 'status-format')
    c.q('set', '-g', 'status-left', '')
    c.q('set', '-g', 'status-right', '')
    window_marker = (c.out / 'window-status-launches').resolve()
    window_job = ('echo #{window_index} >> ' + shlex.quote(str(window_marker)) +
                  '; sleep .1; printf W#{window_index}')
    window_format = '#(' + window_job + ')'
    c.q('set', '-g', 'window-status-format', window_format)
    c.q('set', '-g', 'window-status-current-format', window_format)
    c.until(lambda: window_marker.exists() and
                   len(window_marker.read_text().splitlines()) == 3,
            'window-status evaluates each linked occurrence once', 4)
    time.sleep(.8)
    c.require(sorted(window_marker.read_text().splitlines()) == ['0', '1', '2'],
              'window-status remains idle before TTL')

    c.q('move-window', '-s', 's:2', '-t', 's:3')
    c.until(lambda: sorted(window_marker.read_text().splitlines()) == ['0', '1', '2', '3'],
            'reindexed link gets one fresh occurrence worker', 4)
    time.sleep(.8)
    c.require(sorted(window_marker.read_text().splitlines()) == ['0', '1', '2', '3'],
              'reindexing does not restart unchanged neighboring occurrences')


def ttl(c):
    client = setup(c)
    marker = (c.out / 'ttl-launches').resolve()
    fmt = '#(printf x >> ' + shlex.quote(str(marker)) + '; printf STABLE)'
    settle(c, client, fmt, 'STABLE')
    c.require(marker.read_text() == 'x', 'cold value launches once')
    c.require(display(c, client, fmt) == ['STABLE'], 'warm value is retained')
    c.require(marker.read_text() == 'x', 'warm read does not launch')
    time.sleep(1.1)
    c.require(display(c, client, fmt) == ['STABLE'],
              'expired value remains visible while refreshing')
    c.until(lambda: marker.read_text() == 'xx', 'TTL expiry launches one refresh', 2)
    time.sleep(.15)
    c.require(marker.read_text() == 'xx', 'TTL refresh is singular')


PRODUCT_SCENARIOS.update(occurrence_stability=occurrence_stability, ttl=ttl)


def capacity(c):
    client = setup(c)
    expected = ''.join(f'{index:02}' for index in range(40))
    template = ''.join(f'#(sleep .1; printf {index:02})' for index in range(40))
    c.q('set', '-g', 'status-format[0]', template)
    attached = Client(c, 's', False, cols=100)
    c.until(lambda: expected.encode() in attached.drains[0].raw,
            'all jobs progress beyond the 32-worker admission limit with interval zero', 5)
    c.require(True, 'completion wakeups retry capacity-denied status jobs')
    # A raw option lookup must not acquire E/T expansion or execute its jobs.
    forbidden = (c.out / 'raw-option-forbidden').resolve()
    raw = 'RAW#S#(echo forbidden > ' + shlex.quote(str(forbidden)) + ')'
    c.q('set', '-g', 'status-left', raw)
    start = len(attached.drains[0].raw)
    c.q('set', '-g', 'status-format[0]', '#{status-left}')
    c.until(lambda: b'RAW#S#(' in attached.drains[0].raw[start:], 'raw status option remains literal')
    time.sleep(.2)
    c.require(not forbidden.exists(), 'raw option lookup has no job execution authority')

PRODUCT_SCENARIOS['capacity'] = capacity


def bounded_output(c):
    client = setup(c)
    complete = '#(echo KEPTLINE; sleep 5)'
    settle(c, client, complete, 'KEPTLINE')
    time.sleep(1.25)
    c.require(display(c, client, complete) == ['KEPTLINE'], 'timeout preserves the last complete line')
    partial = '#(printf partial; sleep 5)'
    c.require(display(c, client, partial) == [''], 'incomplete line is not published while running')
    time.sleep(1.25)
    c.require(display(c, client, partial) == [''], 'timeout never presents a fragment as completed output')
    limited = "#(printf 'GOOD\\n'; head -c 65536 /dev/zero | tr '\\000' X; sleep 5)"
    c.require(display(c, client, limited) == [''], 'capture-limit job starts cold')
    observed = []

    def reached_limit():
        observed[:] = display(c, client, limited)
        return len(observed) == 1 and len(observed[0]) == 65531

    c.until(reached_limit, 'bounded capture-limit fragment', 3)
    c.require(observed[0] == 'X' * 65531,
              'capture ceiling returns only the bounded retained fragment')

PRODUCT_SCENARIOS['bounded_output'] = bounded_output
