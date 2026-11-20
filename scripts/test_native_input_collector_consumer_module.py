"""Keep the explicit mutation hook attached to the actual fixture consumer."""
import os

import candidate_identity as guard
import native_input_windows_consumer as retention


_support = guard.support


def fixture_support(name):
    # Production loads stateless support modules afresh. The regression's
    # patch.object(retention, 'retain') must reach the same module object. The
    # literal fixture manifest is never a qualified production manifest path.
    if name == 'native_input_windows_consumer' and os.environ.get('RMUX_NATIVE_INPUT_MANIFEST') == 'qualified':
        return retention
    return _support(name)


guard.support = fixture_support
