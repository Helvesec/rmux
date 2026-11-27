"""Reverse registry arrival, first frames, detach, switch and reattach."""
from support import Client

EXPECTED = {"creation_neighbors": {
    'P-first': 'LB1|P,0,1;|END',
    'Q-first': 'LB1|P,0,0;C,1,0;Q,0,1;|END',
    'mixed': 'P,C,Q,|Q,C,P,',
    'detached': 'C,Q,|Q,C,',
    'control-switched': 'X,Q,|Q,X,',
    'terminal-switched': 'X,P,|P,X,',
    'reattached': 'X,P,Q,|Q,P,X,',
}}


def creation_neighbors(c):
    for name in ('P', 'C', 'Q', 'X'):
        c.new(name)
    c.q('set-option', '-g', 'set-titles-string',
        'LB1|#{L:#{client_session},#{client_control_mode},#{loop_last_flag};}|END')
    c.q('set-option', '-g', 'set-titles', 'on')
    c.r['observations'] = {}
    def first(client, label):
        title = c.until(lambda: client.drains[0].first, label, 3)
        (c.out / (label + '.bin')).write_bytes(bytes(client.drains[0].raw[:title['end']]))
        c.r['observations'][label] = title
        c.require(title['payload'] == EXPECTED['creation_neighbors'][label], label, actual=title)
    def order(label):
        result = c.q('display-message', '-p', '-t', 'X',
                     '#{L/i:#{client_session},}|#{L/ir:#{client_session},}')
        c.r['observations'][label] = result
        c.require(result == EXPECTED['creation_neighbors'][label], label, actual=result)
        default = c.q('display-message', '-p', '-t', 'X', '#{L:#{client_session},}')
        c.require(default == result.split('|')[0], label + ' default matches /i')
    p = Client(c, 'P'); first(p, 'P-first'); c.wait_clients([p])
    control = Client(c, 'C', control=True); c.wait_clients([p, control])
    q = Client(c, 'Q'); first(q, 'Q-first'); rows = c.wait_clients([p, control, q])
    targets = {row[0]: row[1] for row in rows}
    order('mixed')
    c.q('detach-client', '-t', targets[str(p.p.pid)])
    c.wait_clients([control, q]); order('detached')
    c.q('switch-client', '-c', targets[str(control.p.pid)], '-t', 'X')
    control.record['session'] = 'X'
    c.wait_clients([control, q]); order('control-switched')
    c.q('switch-client', '-c', targets[str(q.p.pid)], '-t', 'P')
    q.record['session'] = 'P'
    c.wait_clients([control, q]); order('terminal-switched')
    again = Client(c, 'Q'); c.wait_clients([control, q, again]); order('reattached')
    c.handshake(); c.r['qualified'] = True
