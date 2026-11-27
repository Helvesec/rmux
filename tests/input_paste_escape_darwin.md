# Native Darwin input supplement

`input_paste_escape_darwin.py` adapts only the native observations and lifecycle
of the accepted Linux `fc30ad456` P1/P2/P3/E1 reference. It imports private copies
of `input_paste_escape_reference.py` and `g6_selection_copy_reference.py`, verifies
their SHA-256 digests, and reuses `InputCase.run_inputs`, payloads and expectations.
The copies may be obtained from the respective accepted Git versions; they must
be adjacent in the directory supplied with `--reference-dir`. Shared Linux
sources and the accepted Darwin process driver are not modified.

Run only on Darwin with Python 3.12+ (local `tty.cfmakeraw` is required). Under
L5-006, first run the negative witnesses and the owned Python/PTY positive
control. Each invocation requires a new directory and preserves its results;
neither command starts a product or oracle:

```sh
python3 -B tests/input_paste_escape_darwin_calibration.py mock /private/new-mocks
python3 -B tests/input_paste_escape_darwin_calibration.py native /private/new-native
```

The frozen contract and binary digests are mandatory. For the authorized single
functional lot, supply a new, short private root (encoded socket path <100 bytes):

```sh
python3 -B tests/input_paste_escape_darwin.py \
  --contract /private/CONTRACT.json \
  --restoration-freeze /private/FROZEN.json \
  --reference-dir /private/pinned-linux-reference \
  --root /private/new-run
```

The four tmux 3.7b cases run first and their successful observations are frozen
and hashed before any RMUX case. The first behavioral or cleanup failure stops
the lot and preserves an incomplete summary. There is no automatic retry.
The runtime binary source is recorded separately from the test-driver commit.
This fixture pins the composition `8ea6218f`; no old binary is reattributed.

The Darwin adapter checks `proc_pidinfo` and `proc_pidfdinfo` ABI sizes, records
PID/birth/executable identities, verifies application fd0 and raw termios, and
observes only known processes. A fresh FIFO acknowledgement plus server-observed
title/content establishes readiness. Input is written to the client PTY master;
application reads and timestamps remain separate evidence. P3 read boundaries
are unrestricted. The 1500 ms bound establishes functional delivery only;
I003-T1 and the meaning of escape-time remain separate work.

A private terminal-host process owns the controlling session while the real
client runs as its child. This is required to inspect the same Darwin slave's
termios after normal client exit: releasing its session leader first can make
those ioctls unavailable. The host is identified and observed like the other
owned processes, reaps only its client, and waits for an explicit release after
the driver has checked restoration. The outer driver reaps only the host.
Master output is drained while waiting for exit, including the final 2004
restoration sequences; otherwise Darwin terminal exit can remain pending.
These are fixture adaptations, not changes to expected product bytes.

L5-006 adopts the complete prospective R1-009 A–G protocol. It admits only the
exact initial profile in `darwin_terminal_restore.PROFILE`, including every cc
and both speeds. The attached snapshot must show ICANON and ECHO cleared. After
normal detach, client exit and reap, the driver drains and archives the product
stream and records the raw-after snapshot on the original, still-open slave.
The host has observed only its control FIFO and child exit, with no slave read
or readiness observer at this point. Raw snapshots and the historical strict
comparison remain evidence even when a later check fails.

Classification permits only exact equality or the exclusive addition of PENDIN;
this grants no restoration PASS. A FIFO command then gives input to the living
host. Its first slave availability observer is explicitly timestamped. The
driver writes one fresh, pre-frozen ASCII line, `restore:` plus 32 lowercase hex
digits and LF (41 bytes). The host reads once, at most 64 bytes, and acknowledges
those bytes through a file plus the events FIFO. The driver drains the existing
master after this acknowledgement, requiring exactly the marker with CRLF
(42 bytes), and compares every final termios value without a mask. This phase
has at most five seconds inside the unchanged twelve-second cleanup budget.
There is no flush, repair tcsetattr, reopened slave, FIONREAD, repair sleep or
second marker. Product bytes and restoration echo are separate files.

The native control simulates the client's normal raw/restore transition using
only its own PTY. The observer never writes terminal attributes. Negative
witnesses cover all 64 bits of each Darwin flag word, all cc values and speeds,
missing or corrupt byte/echo traces, event ordering, 2004, identities and cleanup.
The older `--calibrate` ABI fixture is retained, but cannot qualify this protocol.
Before a product run the pilot mailbox must contain the FROZEN artifact pinning
the scripts, interpreter, native environment, binaries, input contract, exact
markers and valid calibration evidence. Both source and runtime provenance are
kept explicit. An incomplete L5-005 remains incomplete; nothing here changes
its historical verdict or proves its particular kernel execution path.

Cleanup records nonzero status, output and process exit events. Primary and
cleanup errors remain separate; emergency disposal of a known identity never
turns a failed case into PASS. Terminal restoration is inspected before any
emergency release of the fixture. A residual owned socket is removed only after
known exits and identity checks. No process discovery, shared server operation,
performance campaign, whole 3.input certificate or exhaustive R6 claim is made.
