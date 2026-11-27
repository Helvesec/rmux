"""Finite Kitty fixture checks, not a runtime codec or general encoder."""
import copy
import re


def require(condition, message):
    if not condition:
        raise ValueError(message)


def integer(value, low, high):
    return type(value) is int and low <= value <= high


def scalar(value):
    return integer(value, 0, 0x10ffff) and not 0xd800 <= value <= 0xdfff


def unhex(value):
    require(isinstance(value, str) and re.fullmatch(r'(?:[0-9a-f]{2})*', value),
            'hex: expected lowercase octets')
    return bytes.fromhex(value)


def decimal(value, default=None):
    if not value and default is not None:
        return default
    require(bool(re.fullmatch(rb'[0-9]+', value)), 'grammar: decimal field')
    # Avoid Python-version-dependent integer string limits, including leading zeros.
    digits = value.lstrip(b'0') or b'0'
    require(len(digits) <= 10, 'policy: uint32 decimal limit')
    number = int(digits)
    require(number <= 0xffffffff, 'policy: uint32 decimal limit')
    return number


def encoding(raw):
    """Uniform fields for u, tilde and letters; CSI R belongs to CPR, not keys.

    Accept source-permitted omissions. This is a whole-frame fixture parser;
    recovery, fragmentation and native stream parsing remain future tests.
    """
    require(len(raw) <= 4096, 'policy: frame byte limit')
    match = re.fullmatch(rb'\x1b\[([0-9:;]*)([u~ABCDEFHPQS])', raw)
    require(match is not None, 'grammar: recognized keyboard encoding')
    fields = match[1].split(b';')
    final = match[2].decode('ascii')
    require(len(fields) <= (3 if final == 'u' else 2), 'grammar: field count')
    keys = fields[0].split(b':')
    require(len(keys) <= (3 if final == 'u' else 1), 'grammar: alternate fields')
    number = decimal(keys[0], 1 if final not in ('u', '~') else None)
    require(scalar(number), 'grammar: key scalar')
    if final not in ('u', '~'):
        require(number == 1, 'grammar: letter identity')
    modifiers = fields[1].split(b':') if len(fields) > 1 else [b'1']
    require(len(modifiers) <= 2, 'grammar: modifier/event fields')
    mod = decimal(modifiers[0], 1)
    kind = decimal(modifiers[1], 1) if len(modifiers) == 2 else 1
    require(1 <= mod <= 256, 'grammar: modifier range')
    require(1 <= kind <= 3, 'grammar: event range')
    shifted = decimal(keys[1]) if len(keys) > 1 and keys[1] else None
    base = decimal(keys[2]) if len(keys) > 2 and keys[2] else None
    require(shifted is None or ((mod - 1) & 1 and scalar(shifted)),
            'grammar: shifted scalar requires Shift')
    require(base is None or scalar(base), 'grammar: base scalar')
    text = ([decimal(x) for x in fields[2].split(b':')]
            if len(fields) == 3 and fields[2] else [])
    require(len(text) <= 256, 'policy: text scalar limit')
    require(all(scalar(x) and x >= 32 and not 127 <= x <= 159 for x in text),
            'grammar: associated text scalar')
    return dict(number=number, final=final, modifiers=mod-1, event_type=kind,
                shifted=shifted, base=base, text=text)


def event_shape(event, names):
    require(isinstance(event, dict) and set(event) ==
            {'key', 'modifiers', 'event_type', 'shifted', 'base', 'text'},
            'schema: event fields')
    key = event['key']
    require(scalar(key) or (isinstance(key, str) and key in names), 'schema: key')
    require(integer(event['modifiers'], 0, 255), 'schema: modifiers')
    require(integer(event['event_type'], 1, 3), 'schema: event kind')
    for field in ('shifted', 'base'):
        require(event[field] is None or scalar(event[field]), 'schema: alternate scalar')
    require(event['shifted'] is None or event['modifiers'] & 1, 'schema: Shift')
    require(isinstance(event['text'], list) and all(
        scalar(x) and x >= 32 and not 127 <= x <= 159 for x in event['text']),
        'schema: text scalars')


def state_shape(state):
    require(isinstance(state, dict) and state, 'schema: state object')
    if any(k in state for k in ('screen', 'main', 'alternate')):
        require(state.get('screen') in ('main', 'alternate'), 'schema: active screen')
        for screen in ('main', 'alternate'):
            stack = state.get(screen)
            require(isinstance(stack, list) and len(stack) <= 16 and
                    all(integer(x, 0, 31) for x in stack), 'schema: pane stack')
        require(isinstance(state.get('pane'), str) and
                integer(state.get('runtime_generation'), 0, 0xffffffff),
                'schema: pane identity')
    if 'outer_screen' in state:
        require({'client','attach_generation','known_flags','owned_push','status','pane_flags'} <= state.keys(),
                'schema: outer state fields')
    for key, value in state.items():
        if key.endswith('generation'):
            require(integer(value, 0, 0xffffffff), 'schema: generation')
        if key in ('flags', 'known_flags', 'saved_flags', 'pane_flags'):
            require((value is None and key in ('known_flags', 'flags')) or integer(value, 0, 31),
                    'schema: state flags')
        if key == 'owned_push':
            require(integer(value, 0, 1), 'schema: owned push count')
        if key == 'outer_screen':
            require(value in ('main', 'alternate'), 'schema: outer screen')
        if key in ('held', 'clients', 'other_panes'):
            require(isinstance(value, (dict, list)), 'schema: state collection')


def flags(state):
    stack = state[state['screen']]
    return stack[-1] if stack else 0


def negotiation(initial, raw):
    """Exact easy query/set/stack arithmetic; no terminal lifecycle simulation."""
    state = copy.deepcopy(initial)
    matches = list(re.finditer(rb'\x1b\[([=<>?])([0-9;]*)u', raw))
    require(b''.join(m[0] for m in matches) == raw, 'grammar: negotiation')
    output = b''
    for match in matches:
        op, body = match[1], match[2]
        parts = body.split(b';')
        stack = state[state['screen']]
        if op == b'?':
            require(not body, 'grammar: query')
            output += f'\x1b[?{flags(state)}u'.encode('ascii')
            continue
        require(len(parts) <= (2 if op == b'=' else 1), 'grammar: command fields')
        value = decimal(parts[0], 1 if op == b'<' else 0)
        if op == b'=':
            mode = decimal(parts[1], 1) if len(parts) == 2 else 1
            require(1 <= mode <= 3, 'grammar: mode range')
            old = flags(state)
            new = (value if mode == 1 else old | value if mode == 2 else old & ~value) & 31
            if stack:
                stack[-1] = new
            else:
                stack.append(new)
        elif op == b'>':
            if len(stack) == 16:
                stack.pop(0)
            stack.append(value & 31)
        elif value:
            del stack[max(0, len(stack)-value):]
    return state, output


def matches_event(parsed, event, identities):
    forms = identities[event['key']] if isinstance(event['key'], str) else {(event['key'], 'u')}
    require((parsed['number'], parsed['final']) in forms, 'semantic: functional identity')
    require(all(parsed[k] == event[k] for k in
                ('modifiers', 'event_type', 'shifted', 'base', 'text')),
            'semantic: decoded event fields')


def selected_output(row):
    return row.get('policy_output_hex') if row['output_hex'] is None else row['output_hex']


def output_fields(row, identities):
    """Check represented escape fields, not whether arbitrary events should emit."""
    value = selected_output(row)
    if value is None or not row.get('event'):
        return
    raw = unhex(value)
    if not raw.startswith(b'\x1b[') or raw[-1:] not in (b'u', b'~', *[bytes([x]) for x in b'ABCDEFHPQS']):
        return
    parsed = encoding(raw)
    event = row['event']
    # Legacy terminfo representatives can differ from the functional table.
    # Their bytes stay digest-bound; F1..F4 are also checked literally below.
    if row.get('profile') == 'pinned-kitty-legacy':
        return
    f = flags(row['initial']) & ~16 if 16 <= flags(row['initial']) <= 23 else flags(row['initial'])
    forms = identities[event['key']] if isinstance(event['key'], str) else {(event['key'], 'u')}
    require((parsed['number'], parsed['final']) in forms, 'semantic: encoded identity')
    require(parsed['modifiers'] == event['modifiers'], 'semantic: encoded modifiers')
    require(parsed['event_type'] == (event['event_type'] if f & 2 else 1),
            'semantic: encoded event kind')
    for field in ('shifted', 'base'):
        require(parsed[field] is None or (f & 4 and parsed[field] == event[field]),
                'semantic: encoded alternate')
    require(parsed['text'] == (event['text'] if f & 16 and event['event_type'] != 3 else []),
            'semantic: associated-text output')


def finite_policies(rows):
    """Literal F3 neighborhood and C0 partition; intentionally not an encoder."""
    c0 = set()
    tab = set()
    fkeys = set()
    cpr = False
    for row in rows:
        event = row.get('event', {})
        if 'screen' not in row['initial']:
            continue
        f, key = flags(row['initial']), event.get('key')
        mod, kind = event.get('modifiers'), event.get('event_type')
        out = selected_output(row)
        if row.get('profile') == 'pinned-kitty-legacy' and key in ('F1','F2','F3','F4') and mod in (0,4):
            suffix = {'F1':'P','F2':'Q','F3':'R','F4':'S'}[key]
            expected = '\x1bO' + suffix if mod == 0 else '\x1b[1;5' + suffix
            if key == 'F3' and mod == 4:
                expected = '\x1b[13;5~'
                require(row['authority'] == 'ambiguity' and row['decision'] == 'A-LEGACY'
                        and row['output_hex'] is None, 'policy: F3 interpretation')
            require(out == expected.encode().hex(), 'semantic: legacy F1/F2/F3/F4')
            fkeys.add((key,mod))
        if row['direction'] == 'event-reencode' and key in (9,13,127) and mod == 0 and f in (2,3,8,10):
            if kind == 3 and not f & 8:
                expected = ''
            elif kind == 3 and not f & 2:
                expected = ''
            elif kind == 2 and f & 2:
                expected = f'\x1b[{key};1:2u'.encode().hex()
                if not f & 8:
                    require(row['decision'] == 'A-C0-REPEAT' and row['output_hex'] is None,
                            'policy: C0 repeat interpretation')
            elif f & 8:
                expected = (f'\x1b[{key};1:3u' if kind == 3 else f'\x1b[{key}u').encode().hex()
            else:
                expected = bytes([key]).hex()
            require(out == expected, 'semantic: C0 partition output')
            c0.add((f,key,kind))
        if row['direction'] == 'event-reencode' and key == 9 and (f,mod) in ((2,5),(3,5),(8,5),(10,5),(1,1),(3,1)):
            canonical = f & 8 or (mod == 5 and f & 1) or (kind == 2 and f & 2)
            expected = (f'\x1b[9;{mod+1}' + (f':{kind}' if kind != 1 and f & 2 else '') + 'u') if canonical else '\x1b[Z'
            if kind == 3 and not (f & 8 and f & 2):
                expected = ''
            require(out == expected.encode().hex(), 'semantic: modified Tab partition')
            tab.add((f,mod,kind))
        if row['input_hex'] == '1b5b313b3552':
            require(row.get('keyboard_events') == [] and row['output_hex'] == ''
                    and row['initial'].get('pending_outer_cpr') is True
                    and row['next'].get('pending_outer_cpr') is False,
                    'semantic: CPR is not F3')
            cpr = True
        if row['direction'] == 'rmux-to-application' and f == 0 and mod == 4 and kind == 1 and integer(key,97,122):
            require(out == bytes([key-96]).hex(), 'semantic: Ctrl letter mapping')
    require(c0 == {(f,k,e) for f in (2,3,8,10) for k in (9,13,127) for e in (1,2,3)},
            'partition: missing C0 press/repeat/release')
    require(tab == {(f,m,e) for f,m in ((2,5),(3,5),(8,5),(10,5),(1,1),(3,1)) for e in (1,2,3)},
            'partition: missing modified Tab sibling')
    require(fkeys == {(k,m) for k in ('F1','F2','F3','F4') for m in (0,4)} and cpr,
            'partition: F3/CPR neighborhood')
