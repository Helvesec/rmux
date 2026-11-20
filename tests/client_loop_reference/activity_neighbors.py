"""Acknowledged inputs in opposite lexical order and across a second boundary."""
import math
import os
import time
from activity import wait_wall, parse_activity
from support import Client, Skip, stamp

DEADLINE = 35
EXPECTED = {'activity_neighbors': {
    'opposite': 'lo,0;hi,1;|hi,0;lo,1;',
    'boundary': 'hi,0;lo,1;|lo,0;hi,1;',
    'boundary-opposite': 'lo,0;hi,1;|hi,0;lo,1;',
}}


def activity_neighbors(c):
    c.new('A'); c.new('B')
    clients = [Client(c, session) for session in ('A', 'B')]
    rows = c.wait_clients(clients)
    lo, hi = sorted(clients, key=lambda client: client.tty)
    names = {lo.tty: 'lo', hi.tty: 'hi'}
    c.require(lo.tty != hi.tty, 'independent distinct PTY names')
    for client in clients:
        row = next(row for row in rows if row[0] == str(client.p.pid))
        c.require(row[1] == row[2] == client.tty, 'public identity matches OS slave path')
    c.q('bind-key', '-n', 'C-g', 'set-option', '-g', '@lb_ack', 'lo')
    c.q('bind-key', '-n', 'C-k', 'set-option', '-g', '@lb_ack', 'hi')
    c.r['observations'], c.r['timing_attempts'] = {}, []
    roles = {'lo': (lo, b'\x07'), 'hi': (hi, b'\x0b')}
    for label, events in [
        ('opposite', [('hi', .1, .3), ('lo', .5, .8)]),
        ('boundary', [('lo', .8, .95), ('hi', 1.1, 1.4)]),
        ('boundary-opposite', [('hi', .8, .95), ('lo', 1.1, 1.4)]),
    ]:
        for number in range(1, 4):
            c.q('set-option', '-g', '@lb_ack', 'pending')
            epoch = math.ceil(time.time()) + 1
            attempt = dict(label=label, number=number, epoch=epoch, events=[])
            c.r['timing_attempts'].append(attempt)
            qualified = True
            for role, offset, limit in events:
                client, data = roles[role]
                wait_wall(epoch + offset)
                event = dict(role=role, write_before=stamp(), deadline=epoch + limit)
                event['write_bytes'] = os.write(client.master, data)
                event['write_after'] = stamp()
                while time.time() < epoch + limit:
                    if c.q('show-options', '-gqv', '@lb_ack', timeout=.1) == role:
                        event['ack'] = stamp(); break
                    time.sleep(.005)
                event['qualified'] = bool(event.get('ack') and epoch + offset <=
                    event['write_before']['wall'] <= event['write_after']['wall'] <=
                    event['ack']['wall'] < epoch + limit)
                qualified = qualified and event['qualified']
                attempt['events'].append(event)
            raw = c.q('display-message', '-p', '-t', 'A',
                      'D=#{L/n:#{client_name},#{client_activity};}'
                      '|T=#{L/t:#{client_name},#{loop_last_flag};}'
                      '|R=#{L/tr:#{client_name},#{loop_last_flag};}')
            parsed = parse_activity(raw)
            seconds = {role: epoch + int(offset) for role, offset, _ in events}
            qualified = qualified and len(parsed['D']) == 2 and all(
                int(value) == seconds[names[name]] for name, value in parsed['D'])
            attempt.update(raw=raw, qualified=qualified)
            if not qualified:
                continue
            actual = '|'.join(''.join(f'{names[name]},{value};' for name, value in parsed[key])
                              for key in ('T', 'R'))
            c.r['observations'][label] = dict(actual=actual, seconds=seconds, raw=raw)
            c.require(actual == EXPECTED['activity_neighbors'][label], label, actual=actual)
            break
        else:
            raise Skip(label + ': timing preconditions exhausted')
    c.r['qualified'] = True
    c.handshake()
