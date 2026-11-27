"""Lossless admission of the existing Kernel-Process ETW SDK/proto traces."""
import collections
import datetime
from pathlib import PureWindowsPath
import re
import statistics
import xml.etree.ElementTree as ET

NS = {'e': 'http://schemas.microsoft.com/win/2004/08/events/event'}


def ns_time(value):
    """Keep ETW's 100 ns precision instead of rounding reused PID lifetimes."""
    match = re.fullmatch(r'(.+?)(?:\.(\d+))?(Z|[+-]\d\d:\d\d)', value)
    if not match:
        raise RuntimeError('native timestamp lacks a UTC offset')
    base, fraction, zone = match.groups()
    instant = datetime.datetime.fromisoformat(base + ('+00:00' if zone == 'Z' else zone))
    epoch = datetime.datetime(1970, 1, 1, tzinfo=datetime.timezone.utc)
    delta = instant - epoch
    return (delta.days * 86400 + delta.seconds) * 10**9 + int((fraction or '').ljust(9, '0')[:9])


def clock_group(value):
    return 'Z' if value.endswith('Z') else value[-6:]


def read_events(path):
    starts, ends, loads, counters = {}, {}, [], []
    event_count = 0
    for _, event in ET.iterparse(path, events=('end',)):
        if not event.tag.endswith('}Event'):
            continue
        event_count += 1
        system = event.find('e:System', NS)
        data = event.find('e:EventData', NS)
        if system is None or data is None:
            event.clear()
            continue
        provider = system.find('e:Provider', NS).get('Name')
        eid = system.findtext('e:EventID', namespaces=NS)
        fields = {v.get('Name'): (v.text or '').strip() for v in data}
        if provider == 'Microsoft-Windows-Kernel-Process' and eid in ('1', '2', '5'):
            fields['at'] = system.find('e:TimeCreated', NS).get('SystemTime')
            if eid == '5':
                loads.append(fields)
            else:
                target = starts if eid == '1' else ends
                seq = int(fields['ProcessSequenceNumber'])
                if seq in target:
                    raise RuntimeError('duplicate native process lifetime event')
                target[seq] = fields
        elif 'EventsLost' in fields:
            counters.append(fields)
        event.clear()
    return starts, ends, loads, counters, event_count


def native_path(name, mappings):
    matches = [(device, drive) for device, drive in mappings.items()
               if name.casefold().startswith(device.casefold() + '\\')]
    if matches:
        device, drive = max(matches, key=lambda item: len(item[0]))
        return drive + name[len(device):]
    if not PureWindowsPath(name).is_absolute():
        raise RuntimeError('native image has no qualified device mapping')
    return name


def require_wait(process, receipts, case, head, image_hash):
    """An ETW end or a PID-only `reaped` boolean never satisfies this predicate."""
    matches = []
    for row in receipts:
        child, parent = row.get('child', {}), row.get('parent', {})
        if (child.get('pid') == process['pid'] and child.get('birth_ns') == process['birth_ns']
                and parent.get('pid') == process['parent_pid']
                and parent.get('birth_ns') == process['parent_birth_ns']
                and row.get('case') == case and row.get('head') == head
                and row.get('handle', 0) > 0 and row.get('wait_api') == 'WaitForSingleObject'
                and row.get('wait_return') == 0 and child.get('end_ns') == process['end_ns']
                and str(child.get('image', '')).casefold() == process['image'].casefold()
                and row.get('executable_sha256') == image_hash
                and row.get('exit') == process['exit']
                and row.get('before_ns', 2**63) <= process['birth_ns']
                and row.get('wait_started_ns', 2**63) <= row.get('wait_finished_ns', 0)
                and row.get('wait_finished_ns', 0) >= process['end_ns']):
            matches.append(row)
    if len(matches) != 1:
        raise RuntimeError('Windows lifetime lacks one actual matching parental wait')
    return matches[0]


def assess(events, root, indexed, waits, mappings, case, head):
    """Return PASS only for exhaustive retention, native pairing and actual waits."""
    from native_input_windows import require_retained
    starts, ends, loads, counters, count = events
    errors = []
    if not counters or any(int(c.get('EventsLost', '-1')) != 0
                           or int(c.get('BuffersLost', '-1')) != 0 for c in counters):
        errors.append('native event loss counters absent or nonzero')
    groups = collections.defaultdict(list)
    # ProcessStop's header includes notification latency after ExitTime. Use
    # creation pairs to measure the clock, then check stop causality separately.
    for row in starts.values():
        groups[clock_group(row['at'])].append(ns_time(row['at']) - ns_time(row['CreateTime']))
    offsets = {}
    for group, values in groups.items():
        offset = round(statistics.median(values) / 10**9) * 10**9
        if any(abs(value - offset) > 100_000_000 for value in values):
            errors.append('inconsistent native clock group: ' + group)
        else:
            offsets[group] = offset
    end_delays = {}
    for seq, row in ends.items():
        group = clock_group(row['at'])
        if group not in offsets:
            errors.append('uncalibrated native end clock group: ' + group)
            continue
        delay = ns_time(row['at']) - offsets[group] - ns_time(row['ExitTime'])
        end_delays[seq] = delay
        if delay < -100_000_000:
            errors.append('native end event precedes exit: ' + str(seq))
    roots = {seq for seq, row in starts.items() if int(row['ProcessID']) == root['pid']
             and ns_time(row['CreateTime']) == root['birth_ns']}
    if len(roots) != 1:
        errors.append('root PID/birth has no unique native sequence')
    owned = roots.copy()
    while True:
        extended = owned | {seq for seq, row in starts.items()
                            if int(row['ParentProcessSequenceNumber']) in owned}
        if extended == owned:
            break
        owned = extended
    processes, intervals = [], collections.defaultdict(list)
    for seq in sorted(owned):
        row, end = starts[seq], ends.get(seq)
        parent_seq = int(row['ParentProcessSequenceNumber'])
        parent = starts.get(parent_seq)
        process = dict(sequence=seq, pid=int(row['ProcessID']),
                       parent_pid=int(row['ParentProcessID']), parent_sequence=parent_seq,
                       parent_birth_ns=ns_time(parent['CreateTime']) if parent else root['parent_birth_ns'],
                       birth_ns=ns_time(row['CreateTime']), end_ns=ns_time(end['ExitTime']) if end else None,
                       exit=int(end['ExitCode']) & 0xFFFFFFFF if end else None,
                       image=native_path(row['ImageName'], mappings))
        processes.append(process)
        intervals[process['pid']].append(process)
        if end is None:
            errors.append('missing native end: ' + str(seq))
            continue
        if (int(end['ProcessID']) != process['pid'] or process['end_ns'] < process['birth_ns']
                or (parent and int(parent['ProcessID']) != process['parent_pid'])):
            errors.append('native lifetime identity differs: ' + str(seq))
        try:
            retained = require_retained(indexed, process['image'], process['birth_ns'], case, head)
            require_wait(process, waits, case, head, retained['sha256'])
        except (RuntimeError, OSError) as error:
            errors.append(str(error) + ': ' + str(seq))
    images, unmatched, uncalibrated = [], [], []
    # Preserve every load event, including repetitions and foreign processes.
    # Unknown clocks cannot be dropped merely because ownership is unprovable.
    for index, row in enumerate(loads):
        group = clock_group(row['at'])
        if group not in offsets:
            uncalibrated.append(dict(index=index, event=row))
            continue
        at = ns_time(row['at']) - offsets[group]
        pid = int(row['ProcessID'])
        matches = [p for p in intervals.get(pid, []) if p['birth_ns'] <= at
                   and (p['end_ns'] is None or at <= p['end_ns'])]
        if len(matches) != 1:
            # A PID seen in this command cannot be silently ignored between
            # lives or matched by its first occurrence in an overlapping list.
            if pid in intervals:
                unmatched.append(dict(index=index, event=row, matches=len(matches)))
            images.append(dict(index=index, event=row, owned=False))
            continue
        process = matches[0]
        path = native_path(row['ImageName'], mappings)
        image = dict(index=index, event=row, owned=True, path=path,
                     sequence=process['sequence'], at_ns=at)
        try:
            image['sha256'] = require_retained(indexed, path, at, case, head)['sha256']
        except (RuntimeError, OSError) as error:
            errors.append(str(error) + ': load ' + str(index))
        images.append(image)
    if unmatched or uncalibrated:
        errors.append('native loads have unmatched lifetimes or uncalibrated clock groups')
    if len(images) + len(uncalibrated) != len(loads):
        raise RuntimeError('native load accounting lost events')
    return dict(status='PASS' if not errors else 'PENDING', errors=errors,
                event_count=count, counters=counters, clock_offsets_ns=offsets,
                end_event_delays_ns=end_delays,
                processes=processes, images=images, loads_total=len(loads),
                unmatched_loads=unmatched, uncalibrated_loads=uncalibrated,
                wait_receipts_total=len(waits), raw_events_unchanged=True)
