"""D65 production rules. No path exemptions and no semantic reduction credit."""
import hashlib
import re

REFERENCE_REVISION = 'ea435dddd3fba0e2c9e8e7c8d97150b0712ed21b'
REFERENCE_LINES = 343521
# The published tree carries no D65 private word in any form: no clear text,
# digest, prefix or length, because a short word is recovered from its digest
# by brute force in seconds. The list is explicitly empty here; the generic
# pattern below stays active. Each row, where a private list supplies one, is
# (folded 2-character prefix, length, sha256 of the folded word).
TOKENS = ()
GENERIC = r'(?i)(fleet|orchestrat|dispatch[_-]id(?![a-z0-9])|agent[_-](role|session)|build[_-]slot)'
# Characters outside ASCII that re.IGNORECASE equates with an ASCII letter
# (identical on CPython 3.9 to 3.15). They are also the only characters outside
# ASCII whose lower() contains ASCII, so lower() cannot equate any other one.
FOLD = {'\u0130': 'i', '\u0131': 'i', '\u017f': 's', '\u212a': 'k'}


def fold(text):
    return ''.join(FOLD.get(char, char.lower()) for char in text)


def token_digest(word):
    return hashlib.sha256(fold(word).encode()).hexdigest()


def compile_matcher(tokens):
    """Same matches, in the same order, as re.finditer over GENERIC|word|word...
    under re.IGNORECASE: at each position GENERIC first, then the first token."""
    if not GENERIC.startswith('(?i)'):
        raise ValueError('D65 generic pattern must stay case-insensitive')
    generic = re.compile(GENERIC)
    prefixes = sorted({prefix for prefix, _, _ in tokens})
    starts = re.compile('(?=' + GENERIC[4:] + ''.join('|' + re.escape(p) for p in prefixes) + ')',
                        re.IGNORECASE)

    def matches(text):
        found, end = [], 0
        for start in starts.finditer(text):
            index = start.start()
            if index < end:
                continue
            hit = generic.match(text, index)
            if hit:
                found.append(hit.group())
                end = hit.end()
                continue
            for prefix, length, digest in tokens:
                window = text[index:index + length]
                if (len(window) == length and fold(window[:2]) == prefix
                        and token_digest(window) == digest):
                    found.append(window)
                    end = index + length
                    break
        return found
    return matches


blacklist_matches = compile_matcher(TOKENS)


def check_envelope(delta, mandate):
    envelope = mandate.get('production_growth_envelope', 0)
    if type(envelope) is not int or envelope < 0:
        raise ValueError('production envelope must be a nonnegative integer')
    justification = mandate.get('production_growth_justification', '')
    decision = mandate.get('production_growth_decision', '')
    if envelope and (not isinstance(justification, str) or not justification.strip()):
        raise ValueError('positive production envelope requires mandate justification')
    if envelope > 1000 and (not isinstance(decision, str) or not decision.strip()):
        raise ValueError('production envelope above 1000 requires sponsor decision')
    if delta > envelope:
        raise ValueError(f'lot production RED: net delta {delta} exceeds envelope {envelope}')
    return envelope


def check_freeze(total):
    if total > REFERENCE_LINES:
        raise ValueError(f'freeze production RED: {total} exceeds {REFERENCE_LINES}')


def scan_blacklist(report, tree):
    matches = []
    for row in report['files']:
        path = row['path']
        manifest = path == 'Cargo.toml' or (path.startswith('crates/') and path.endswith('/Cargo.toml'))
        lines = tree[path]['data'].decode('utf-8', errors='replace').split('\n')
        checked = set(range(1, len(lines) + 1)) if manifest else set()
        for region in row['regions']:
            if region['class'] in ('production', 'assets'):
                checked.update(range(region['start'], region['end'] + 1))
        for number in sorted(checked):
            for token in blacklist_matches(lines[number - 1]):
                matches.append({'path': path, 'line': number, 'token': token})
    return matches


def check_ratchets(delta):
    if (delta['literal_ratchet_increases'] or delta['semantic_attribute_additions']
            or delta['production_size_violations']):
        raise ValueError('source ratchet RED')
