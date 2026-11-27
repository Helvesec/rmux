"""Fail-closed checks for the pilot-frozen Darwin A–G restoration protocol.

Inputs are complete JSON snapshots, never masked final attributes. Classification
is an intermediate observation, not a restoration verdict.
"""
import re
import termios

CONTRACT_SHA = '2e4573dd0e7d4e3101dc7f56d8942939d0a50383aea713c5bd93d2ec69204d63'
PROFILE = [11010, 3, 19200, 1483, 9600, 9600,
           ['04', 'ff', 'ff', '7f', '17', '15', '12', 'ff', '03', '1c',
            '1a', '19', '11', '13', '16', '0f', '01', '00', '14', 'ff']]


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def admit(before, attached):
    require(before == PROFILE, 'initial profile outside frozen admission')
    require(not before[3] & termios.PENDIN, 'PENDIN initially present')
    require(len(attached) == 7 and len(attached[6]) == 20, 'attached snapshot incomplete')
    require(before[3] & (termios.ICANON | termios.ECHO) == (termios.ICANON | termios.ECHO),
            'initial canonical/echo flags missing')
    require(attached[3] & (termios.ICANON | termios.ECHO) == 0,
            'canonical/echo transition not observed')


def classify(before, attached, raw):
    admit(before, attached)
    require(len(raw) == 7, 'raw snapshot incomplete')
    require(raw[:3] == before[:3] and raw[4:] == before[4:],
            'raw flags/cc/speeds differ outside lflag')
    require(raw[3] in (before[3], before[3] | termios.PENDIN),
            'raw lflag differs beyond exclusive PENDIN addition')
    return {'configuration_equal': True, 'pending_reprocess': raw[3] != before[3],
            'historical_strict_equal': raw == before,
            'flag_xor': [a ^ b for a, b in zip(before[:4], raw[:4])],
            'restoration_pass': False}


def outer_mode(product):
    enable, disable = product.rfind(b'\x1b[?2004h'), product.rfind(b'\x1b[?2004l')
    require(disable > enable >= 0, 'outer2004 final disable absent or out of order')
    return {'last_enable_offset': enable, 'last_disable_offset': disable, 'restored': True}


def same_identity(actual, expected):
    require(actual is not None and expected is not None, 'terminal host disappeared')
    for key in ('pid', 'birth', 'executable', 'sha256'):
        require(actual[key] == expected[key], 'terminal host identity changed: ' + key)


def same_slave(actual, expected):
    for key in ('dev', 'ino', 'rdev', 'mode'):
        require(actual[key] == expected[key], 'slave identity changed: ' + key)


def marker_bytes(marker):
    require(isinstance(marker, bytes) and re.fullmatch(rb'restore:[0-9a-f]{32}\n', marker),
            'marker is not the frozen 41-byte ASCII format')
    require(len(marker) == 41, 'marker length differs')
    return marker


def effects(before, marker, ack, echo, final):
    marker_bytes(marker)
    require(ack is not None, 'service acknowledgement missing')
    require(ack['error'] is None, 'service read error')
    require(ack['read_calls'] == 1 and ack['read_limit'] == 64, 'service read count/limit differs')
    require(isinstance(ack['read_ns'], int) and ack['read_ns'] <= ack['ack_ns'],
            'service read timestamp missing or out of order')
    require(ack['read_hex'] == marker.hex(), 'service bytes differ (including EOF/truncation/residue)')
    require(echo == marker[:-1] + b'\r\n' and len(echo) == 42, 'service echo differs')
    require(final == before, 'final termios differs; no mask permitted')


def boundary(trace):
    require(trace['client_exit_event'] and trace['client_reaped'] and trace['client_code'] == 0,
            'client did not exit and reap normally')
    require(trace['slave_observers_before_snapshot'] == [], 'slave serviced before raw snapshot')
    require(trace['product_saved_ns'] <= trace['raw_snapshot_ns'] < trace['classification_ns']
            < trace['command_ns'] <= trace['first_observer_ns'] <= trace['ready_ns']
            <= trace['write_before_ns'] <= trace['read_ns'] <= trace['ack_ns']
            <= trace['final_snapshot_ns'] <= trace['service_deadline_ns'],
            'incomplete or unordered terminal-service trace')
    require(trace['write_before_ns'] <= trace['write_after_ns'] <= trace['final_snapshot_ns'],
            'marker write timestamps out of order')
    require(trace['write_calls'] == 1 and trace['write_bytes'] == 41, 'marker write differs')
    require(trace['service_deadline_ns'] <= trace['cleanup_deadline_ns'], 'service extends cleanup')
    require(trace['service_deadline_ns'] - trace['command_ns'] <= 5_000_000_000,
            'service exceeds five-second budget')


def cleanup_ok(cleanup, fds_stable):
    require(not cleanup['errors'] and not cleanup['rescues'], 'cleanup error or rescue')
    require(cleanup['all_known_exits_observed'], 'owned process survived')
    require(cleanup['host_exit_code'] == 0, 'host not reaped normally')
    require(cleanup['client_end']['reaped'] and cleanup['client_end']['code'] == 0,
            'client not reaped normally')
    require(cleanup['socket_absent'], 'owned socket survived')
    require(cleanup['termios_restored'] and cleanup['outer_mode_2004']['restored'],
            'terminal restoration incomplete')
    require(fds_stable, 'parent FD set differs')
