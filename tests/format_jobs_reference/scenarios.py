"""Consumer observations and literal expectations measured on pinned tmux 3.7b."""
import shlex
import time
from support import Client, identity, marker_matches_identity


def control(c, client, command):
    start = len(client.drains[0].raw)
    token = 'FJBOUNDARY' + str(time.monotonic_ns())
    began = time.monotonic()
    client.p.stdin.write((command + '\ndisplay-message -p ' + token + '\n').encode())
    client.p.stdin.flush()
    c.until(lambda: token.encode() in client.drains[0].raw[start:], 'control command boundary', 3)
    lines = bytes(client.drains[0].raw[start:]).decode(errors='replace').splitlines()
    elapsed = time.monotonic() - began
    c.r.setdefault('control_commands', []).append(dict(command=command, lines=lines, elapsed=elapsed))
    c.require(not any(line.startswith('%error') for line in lines), 'control command succeeded', command=command)
    return [line for line in lines if not line.startswith(('%', token))]


def display(c, client, template, target=None):
    command = 'display-message -p ' + (('-t ' + shlex.quote(target) + ' ') if target else '') + shlex.quote(template)
    return control(c, client, command)


def settle(c, client, template, expected, target=None, timeout=3):
    latest = []
    def match():
        latest[:] = display(c, client, template, target)
        return latest == [expected]
    c.until(match, 'settled display ' + expected, timeout)
    c.require(latest == [expected], 'settled consumer value', expected=expected, actual=latest)


def setup(c):
    c.new('s')
    c.q('set', '-g', 'status-interval', '0')
    client = Client(c, 's', True)
    c.wait_clients([client])
    return client


def setup_self_identifying(c):
    producer = c.new_self_identifying('s')
    c.q('set', '-g', 'status-interval', '0')
    client = Client(c, 's', True)
    c.wait_clients([client])
    return client, producer


def display_cache(c):
    client = setup(c)
    marker = c.out / 'runs'
    release = (c.out / 'release').resolve()
    command = "printf x >> " + shlex.quote(str(marker.resolve())) + "; while [ ! -f " + shlex.quote(str(release)) + " ]; do sleep .01; done; printf 'first\\nlast\\n'"
    fmt = 'A#(' + command + ')Z'
    began = time.monotonic()
    c.require(display(c, client, fmt) == ['AZ'], 'cold display is empty')
    c.require(time.monotonic() - began < .2, 'display returns before job finishes')
    c.until(marker.exists, 'job started behind release barrier')
    c.require(display(c, client, fmt) == ['AZ'], 'in-flight read is nonblocking')
    c.require(marker.read_text() == 'x', 'in-flight reads do not duplicate the worker')
    release.write_text('release')
    settle(c, client, fmt, 'AlastZ')
    c.require(display(c, client, fmt) == ['AlastZ'], 'warm display retains output')
    # A separate short-lived client must not read the persistent owner's cache.
    c.require(c.q('display-message', '-p', fmt) == 'AZ', 'CLI request owns a cold cache')
    stream = "B#(printf 'intermediate\\n'; sleep .4; printf tail)E"
    c.require(display(c, client, stream) == ['BE'], 'stream cold')
    settle(c, client, stream, 'BintermediateE', timeout=.3)
    settle(c, client, stream, 'BtailE')
    settle(c, client, "#(printf '%%s' percent)", 'percent')
    settle(c, client, "#(printf 'first\\n\\n')", '')


def status(c):
    client = setup(c)
    c.q('set', '-g', 'status-left-length', '70')
    c.q('set', '-g', 'status-right', '')
    c.q('set', '-g', 'window-status-format', '')
    c.q('set', '-g', 'window-status-current-format', '')
    c.q('set', '-g', 'status-left', 'AA#(sleep .25; printf STATUSDONE)ZZ')
    attached = Client(c, 's', False)
    c.wait_clients([client, attached])
    c.until(lambda: b'STATUSDONE' in attached.drains[0].raw, 'status completion without timer or input', 3)
    c.require(True, 'status interval zero observes completion')
    # Expanded option jobs must be evaluated in the same engine.
    start = len(attached.drains[0].raw)
    c.q('set', '-g', 'status-left', 'AA#(sleep .2; printf OPTIONDONE)ZZ')
    c.q('set', '-g', 'status-format[0]', '#{T:status-left}')
    c.until(lambda: b'OPTIONDONE' in attached.drains[0].raw[start:], 'T option job completion', 3)
    c.require(True, 'status-format T option completion')
    c.q('set', '-g', 'status-interval', '1')
    c.q('set', '-g', 'set-titles-string', '#(sleep .2; printf TITLEDONE)')
    c.q('set', '-g', 'set-titles', 'on')
    c.until(lambda: b'TITLEDONE' in attached.drains[0].raw, 'title job neighbor', 3)
    c.require(True, 'title completion through shared runtime')


def selection(c):
    client = setup(c)
    forbidden = c.out / 'forbidden'
    cmd = 'printf forbidden > ' + shlex.quote(str(forbidden.resolve()))
    for fmt, want in [('##(' + cmd + ')', '#(' + cmd + ')'),
                      ('#{?0,#(' + cmd + '),SAFE}', 'SAFE')]:
        c.require(display(c, client, fmt) == [want], 'selection and escaping', template=fmt)
    c.q('set', '-g', '@job', '#(printf FROMOPTION)')
    settle(c, client, '#{E:@job}', 'FROMOPTION')
    c.q('set', '-g', 'status-left', '#{?0,#(' + cmd + '),SAFE} ##(' + cmd + ')')
    attached = Client(c, 's', False)
    c.until(lambda: b'SAFE' in attached.drains[0].raw, 'selected status branch')
    time.sleep(.25)
    c.require(not forbidden.exists(), 'unselected and escaped jobs never execute in consumers')
    source = c.out / 'source.conf'
    source.write_text('display-message -p ' + shlex.quote('#(printf SOURCEDONE)') + '\n')
    control(c, client, 'source-file ' + shlex.quote(str(source.resolve())))
    time.sleep(.12)
    c.require(control(c, client, 'source-file ' + shlex.quote(str(source.resolve()))) == ['SOURCEDONE'],
              'source-file preserves initiating cache owner')


def invalidation(c):
    client, s_producer = setup_self_identifying(c)
    c.q('set', '-g', '@word', 'OLD')
    fmt = "#(sleep .2; printf '#{@word}')"
    settle(c, client, fmt, 'OLD')
    c.q('set', '-g', '@word', 'NEW')
    c.require(display(c, client, fmt) == ['OLD'], 'command edit keeps previous line while refreshing')
    settle(c, client, fmt, 'NEW')
    # Mutate again while a job is running; obsolete completion must not replace it.
    c.q('set', '-g', '@word', 'OBSOLETE')
    display(c, client, fmt)
    c.q('set', '-g', '@word', 'CURRENT')
    display(c, client, fmt)
    settle(c, client, fmt, 'CURRENT')
    time.sleep(.25)
    c.require(display(c, client, fmt) == ['CURRENT'], 'obsolete completion cannot overwrite replacement')
    other = Client(c, 's', True)
    c.require(display(c, other, fmt) == [''], 'second persistent client has isolated cold cache')
    settle(c, other, fmt, 'CURRENT')
    t_producer = c.new_self_identifying('t')
    s_pid = s_producer['identity']['pid']
    t_pid = t_producer['identity']['pid']
    c.require(s_pid != t_pid and identity(t_pid) == t_producer['identity'],
              'wrong-PID control is a distinct live producer',
              target='s', wrong_target='t', wrong_identity=t_producer['identity'])
    c.require(not marker_matches_identity(s_producer['marker'], t_producer['identity']),
              'target fixture rejects a coherent wrong live PID attribution',
              target='s', marker=s_producer['marker'], wrong_identity=t_producer['identity'])
    fmt2 = "#(echo '#{session_name}/#{pane_pid}')"
    for target in ['s', 't', 's']:
        expected = target + '/' + str(s_pid if target == 's' else t_pid)
        settle(c, client, fmt2, expected, target)


def subscriptions(c):
    client = setup(c)
    control(c, client, "refresh-client -B 'job::#(printf BVALUE)'")
    c.until(lambda: b': BVALUE' in client.drains[0].raw, 'B asynchronous publication', 4)
    c.require(True, 'B observes job completion')
    start = len(client.drains[0].raw)
    control(c, client, "refresh-client -B 'job::#(printf BREPLACED)'")
    c.until(lambda: b': BREPLACED' in client.drains[0].raw[start:], 'B replacement', 4)
    c.require(True, 'B replacement observes new value')
    control(c, client, 'refresh-client -B job')
    start = len(client.drains[0].raw)
    time.sleep(1.15)
    c.require(b'%subscription-changed job ' not in client.drains[0].raw[start:], 'B removal retires delivery')
    pane = c.q('display-message', '-p', '-t', 's', '#{pane_id}')
    window = c.q('display-message', '-p', '-t', 's', '#{window_id}')
    control(c, client, 'refresh-client -B ' + shlex.quote('pane:' + pane + ':#(printf PANEJOB)'))
    control(c, client, 'refresh-client -B ' + shlex.quote('window:' + window + ':#(printf WINDOWJOB)'))
    c.until(lambda: b': PANEJOB' in client.drains[0].raw and b': WINDOWJOB' in client.drains[0].raw,
            'B explicit pane and window jobs', 4)
    c.require(True, 'B explicit target consumers')


def loops(c):
    client = setup(c)
    c.new('t')
    other = Client(c, 't', True)
    c.wait_clients([client, other])
    # Client PIDs are owned Popen identities, independent of format expansion.
    expected = str(client.p.pid) + ';' + str(other.p.pid) + ';'
    fmt = "#{L/i:#(printf '#{client_pid}');}"
    settle(c, client, fmt, expected)
    settle(c, client, fmt + '#{client_pid}', expected + str(client.p.pid))
    c.new_window('s:1')
    c.split_window('s:0')
    # Expected loop values come from the explicit fixture, not from a plain
    # expansion by the engine under test. The tmux S value is measured below.
    for scope, key, wanted in [('S', 'session_name', 's;t;'),
                               ('W', 'window_index', '0;1;'),
                               ('P', 'pane_index', '0;1;')]:
        template = '#{' + scope + ":#(printf '#{" + key + "}');}"
        if scope == 'S' and c.engine == 'oracle':
            c.r.setdefault('divergences', []).append('tmux S jobs reuse one raw-command slot across sessions: t;t;. RMUX isolates canonical session contexts: s;t;.')
            wanted = 't;t;'
        settle(c, client, template, wanted)
    control(c, client, "refresh-client -B " + shlex.quote('loop::' + fmt))
    c.until(lambda: (': ' + expected).encode() in client.drains[0].raw, 'B L jobs', 4)
    c.require(True, 'B L neighbor uses each client context')


def linked_occurrences(c):
    client = setup(c)
    c.new_window('s:1')
    c.q('link-window', '-d', '-s', 's:0', '-t', 's:2')
    c.r['fixture'] = {'window_occurrences': ['0/@0', '1/@1', '2/@0']}

    linked = "#{W:#(sleep .1; printf '#{window_index}');}"
    wanted = '0;1;2;'
    if c.engine == 'oracle':
        wanted = '2;1;2;'
        c.r.setdefault('divergences', []).append(
            'tmux 3.7b shares one W job slot between linked occurrences and yields 2;1;2;. '
            'RMUX keeps session-local occurrences distinct and yields 0;1;2;.'
        )
    observed_value = []
    for _ in range(20):
        observed_value = display(c, client, linked)
        if observed_value == [wanted]:
            break
        time.sleep(.15)
    c.require(observed_value == [wanted], 'linked occurrence value',
              expected=wanted, actual=observed_value)

    for name, selector in [('windows', '@*'), ('panes', '%*')]:
        start = len(client.drains[0].raw)
        control(c, client, 'refresh-client -B ' + shlex.quote(
            name + ':' + selector + ':#(sleep .1; printf IDX#{window_index})'))

        def observed():
            lines = bytes(client.drains[0].raw[start:]).decode(errors='replace').splitlines()
            for index in [0, 1, 2]:
                value = 2 if c.engine == 'oracle' else index
                if not any(line.startswith('%subscription-changed ' + name + ' ') and
                           (' ' + str(index) + ' ') in line and
                           line.endswith(': IDX' + str(value)) for line in lines):
                    return False
            return True

        c.until(observed, 'linked-window wildcard results', 4)
        c.require(True, 'wildcard linked-window observation', name=name)


SCENARIOS = dict(display_cache=display_cache, status=status, selection=selection,
                 invalidation=invalidation, subscriptions=subscriptions, loops=loops,
                 linked_occurrences=linked_occurrences)


def explicit_refresh(c):
    client = setup(c)
    c.q('set', '-g', 'status-interval', '30')
    c.q('set', '-g', 'status-left-length', '70')
    c.q('set', '-g', 'status-right', '')
    value = (c.out / 'refresh-value').resolve()
    value.write_text('OLDMARKER')
    c.q('set', '-g', 'status-left', '#(sleep .2; cat ' + shlex.quote(str(value)) + ')')
    first = Client(c, 's', False)
    second = Client(c, 's', False)
    c.wait_clients([client, first, second])
    c.until(lambda: all(b'OLDMARKER' in cl.drains[0].raw for cl in (first, second)), 'both status caches ready')
    value.write_text('NEWMARKER')
    one, two = len(first.drains[0].raw), len(second.drains[0].raw)
    c.q('refresh-client', '-t', first.tty, '-S')
    c.until(lambda: b'NEWMARKER' in first.drains[0].raw[one:], 'explicit -S invalidates before TTL', 3)
    c.require(b'NEWMARKER' not in second.drains[0].raw[two:], 'explicit refresh does not invalidate the peer cache')
    value.write_text('THIRDMARKER')
    one = len(first.drains[0].raw)
    c.q('refresh-client', '-t', first.tty)
    c.until(lambda: b'THIRDMARKER' in first.drains[0].raw[one:], 'explicit full refresh invalidates before TTL', 3)
    c.require(True, 'both explicit refresh entry paths use owner-scoped invalidation')

SCENARIOS['explicit_refresh'] = explicit_refresh
