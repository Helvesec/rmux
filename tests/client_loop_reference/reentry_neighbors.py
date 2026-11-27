"""Bounded automatic-name neighbors measured on the pinned 3.7b oracle."""
from support import Client

EXPECTED = {"reentry_neighbors": {
    "seed": "AUTO=LBseed|NAME=LBseed|END",
    "empty": "AUTO=|NAME=LBseed|END",
}}


def reentry_neighbors(c):
    c.new('N', name='LBseed')
    client = Client(c, 'N', control=True)
    c.wait_clients([client])
    c.r['observations'] = {}
    def observe(label, expected):
        result = c.q('display-message', '-p', '-t', 'N:0',
                     'AUTO=#{E:automatic-rename-format}|NAME=#{window_name}|END')
        c.r['observations'][label] = result
        c.require(result == EXPECTED['reentry_neighbors'][expected], label, actual=result)
        c.handshake()
    for template in ['#{window_name}', '#{L:#{window_name}}',
                     '#{L:#{L:#{window_name}}}', '#{L:#{W:#{window_name}}}',
                     '#{S:#{L:#{window_name}}}', '#{L:#{P:#{window_name}}}',
                     '#{L:#{E:automatic-rename-format}}']:
        c.q('set-option', '-w', '-t', 'N:0', 'automatic-rename-format', template)
        c.q('set-option', '-w', '-t', 'N:0', 'automatic-rename', 'on')
        observe(template, 'empty' if 'E:' in template else 'seed')
    c.q('set-option', '-w', '-t', 'N:0', 'automatic-rename', 'off')
    c.q('set-option', '-w', '-t', 'N:0', 'automatic-rename-format', '#{L:#{window_name}}')
    observe('rename-off', 'seed')
    c.q('detach-client', '-s', 'N')
    c.until(lambda: not c.client_rows(), 'zero clients')
    c.q('set-option', '-w', '-t', 'N:0', 'automatic-rename', 'on')
    observe('zero-client', 'empty')
    c.r['qualified'] = True
