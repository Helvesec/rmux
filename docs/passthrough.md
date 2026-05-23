# Passthrough sessions

A rmux session mode that gives up multi-pane composition in exchange for
**bit-identical terminal fidelity**: programs running inside the session see
the host terminal as if rmux were not in the loop. The motivating use case is
streaming TUIs like `claude` whose scrollback, mouse, and rendering should
behave exactly the way they do under a bare terminal.

## Motivation

Outside rmux, `claude` runs in the host terminal's main screen buffer.
Output streams line-by-line; the host's native scrollback accumulates the
whole session; mouse wheel scrolls through history; copy/paste, hyperlinks,
graphics, and kitty keyboard protocol all work because the host sees the
program directly.

Inside the normal rmux session, the *outer* terminal is switched into
DECSET 1049 (alternate screen) the moment a client attaches, and rmux owns
the grid: every emitted byte is parsed, rendered into rmux's screen model,
and re-emitted as a paint frame addressing absolute cursor positions.
That model is fine for multi-pane composition; it is destructive for "I just
want my host terminal back, plus persistence across detach".

Passthrough mode is the second mode: keep persistence, lose composition.

## Design

### What passthrough is

A **session-level** property. Set at session creation, immutable for the
lifetime of the session (changing it mid-flight would require resetting the
host terminal in ways that fight whatever the inner program is doing).

In a passthrough session:

* the host (outer) terminal is **not** put into alt-screen;
* there is **no** rmux status bar, no pane borders, no overlay chrome of
  any kind;
* pane operations are rejected;
* window operations are permitted (`new-window`, `select-window`,
  `next-window`, `previous-window`, `rename-window`, `kill-window`,
  `last-window`, plus `detach-client`);
* a session holds 1..N windows, each with exactly one pane;
* bytes from the active window's inner PTY are forwarded **verbatim** to
  the attached client; the client writes them straight to the host
  terminal;
* host terminal accumulates the stream natively — scrollback, search,
  selection, mouse wheel, link-following, and graphics passthrough all
  behave as if rmux were not there.

### Per-window byte log + grid snapshot

Each window owns a bounded **raw byte log** (default 1 MiB, configurable
via `passthrough-replay-bytes`). The log holds the inner PTY's emitted
bytes from a known-safe checkpoint to the present.

Because raw bytes are not safely truncatable mid-stream (you can land
inside a CSI sequence, after an unbalanced `?1049h`, with SGR state
implied by earlier bytes), we cannot just drop oldest bytes on overflow.
Instead each log carries a **snapshot prefix**: a sequence of escape
codes that, when emitted into a freshly-reset terminal, restores the
inner program's current visible state. The snapshot is derived from
rmux-core's existing `TerminalScreen` grid (already maintained per pane
for capture/copy-mode purposes).

```
window log = [snapshot_bytes] ++ [raw_log_since_snapshot]
```

On overflow (raw log exceeding the budget), we:
1. take a fresh snapshot of the current grid,
2. replace the log with `[snapshot] ++ []`, discarding earlier raw bytes,
3. continue appending.

Snapshot bytes are produced by walking the grid: SGR0, clear screen,
cursor home, then a per-cell repaint encoding character + colour/style
attributes, then a final cursor positioning to the inner program's
last known cursor. The same grid render code that `capture-pane`
already uses is reused with a "full reset prefix" option.

### Data flow when active

```
inner PTY → bytes ──┬──► append to window's raw log
                    │
                    ├──► feed TerminalScreen (state tracking)
                    │
                    └──► (if this window is active) forward to socket
                          → client → host terminal
```

* Inactive windows: still feed grid + log, do **not** forward.
* Active window: tee into both.

The `TerminalScreen` is still kept up to date because we need it for
snapshots, for `capture-pane`, and for window-switch replay.

### Window switch

On `select-window` (or `Ctrl-b 0..9`, `Ctrl-b n`, `Ctrl-b p`,
`Ctrl-b l`) inside a passthrough session:

1. **Reset host terminal state** so the outgoing window can't leave the
   host with SGR or mouse-tracking turned on:
   ```
   ESC c                   (RIS — full reset; harmless on modern terms)
   ESC [ ! p                (DECSTR — soft reset, fallback)
   ESC [ ? 1049 l           (ensure not in alt-screen)
   ESC [ ? 1000;1002;1003;1006 l   (disable mouse modes)
   ESC [ ? 25 h             (cursor visible)
   ESC [ 0 m                (SGR reset)
   ```
   In practice we issue a small canonical reset string and rely on the
   replay to re-establish whatever the inner program wanted.

2. **Replay the new window's log**: write `snapshot ++ raw_log` to the
   socket as one frame.

3. **Set OSC title** to `rmux: <session>/<window-name>` so the host
   tab/title bar reflects context.

4. **SIGWINCH the inner program** at the new window with the current
   size, so any TUI that does lazy redraw on resize repaints.

5. Resume direct forwarding from the new active window.

Outgoing window: keep buffering into its log silently. Its state is
preserved; it does not see a resize.

### What about scrollback on switch?

The host terminal's scrollback gets the replayed bytes appended. So
switching to a window you used 10 minutes ago lands the last ~1 MiB of
its output back into your kitty scrollback. Within a window, while you
stay there, host scrollback grows naturally (no replay overhead, no
truncation). Switching away and back resets the host scrollback for
that window to "replay buffer worth of history" — bounded, but on the
order of hours for a typical Claude session.

### Detach and reattach

Same flow as window switch, applied to the active window: on attach,
reset host, replay active window's log, set title, resume.

### Pane operations

Rejected with `not available in passthrough sessions: <op>`. Rejected
ops: `split-window`, `select-pane`, `swap-pane`, `kill-pane`, `break-
pane`, `join-pane`, `pipe-pane`, `resize-pane`, `display-panes`,
`select-layout`, `next-layout`, `previous-layout`.

Allowed: any `*-window` op, `new-session`, `kill-session`, `detach-
client`, `rename-session`, `rename-window`, `attach-session`,
`switch-client`, `show-options`, `set-option`, copy-buffer ops.

### Configuration surface

* New session-scope option: `passthrough` (boolean, default `off`).
  Set at session creation; immutable thereafter.
* New CLI flag on `new-session`: `--passthrough` / `-P`.
* New server-scope option: `passthrough-replay-bytes` (integer, default
  `1048576`). Per-window log budget.
* Existing `status` / `status-bar` options ignored in passthrough.
* Existing pane-split keybindings emit the rejection error.

### What this doesn't do

* **No multi-pane.** By design.
* **No copy-mode in rmux.** The host terminal owns selection. `capture-
  pane` still works for scripting (it reads the grid).
* **No outer scrollback persistence across switches** beyond replay
  budget. If you scroll back deep in window A, switch to B, switch
  back: A's scrollback restart from the snapshot point. Live with it.
* **No graphical chrome.** No status bar. Window identity surfaces via
  the host's title bar (OSC 0/2).

## Implementation plan

### Phase 1 — Foundation (landed)

1. **rmux-core**: `PassthroughReplayLog` (snapshot bytes + raw log,
   bounded). Snapshot rendering via `render_screen_snapshot(&Screen)`
   reuses the existing grid capture path with a reset-prefix.
   Helpers: `is_passthrough_session(opts, &name)`,
   `reject_pane_op_if_passthrough(opts, &name, op)`.
2. **rmux-proto**: `OptionName::Passthrough`,
   `OptionName::PassthroughReplayBytes`.
3. **rmux-core/options**: registry entries —
   `passthrough` (session scope, flag, default `off`) and
   `passthrough-replay-bytes` (server scope, number, default 1 MiB).
4. **rmux-server**: `handler_support::reject_pane_op_in_passthrough`
   wired into 12 pane-op handlers — `split-window`, `split-window -e`,
   `swap-pane`, `join-pane`, `break-pane`, `kill-pane`, `pipe-pane`,
   `select-pane` (selection only, not title), `select-pane -m`,
   `select-pane-adjacent`, `select-layout`, `select-custom-layout`,
   `select-old-layout`, `spread-layout`, `next-layout`,
   `previous-layout`, `resize-pane`.
5. **Tests** (landed):
   - `passthrough_log::tests::*` — log + snapshot + gating helpers
     (13 unit tests).
   - `handler::pane_command_tests::passthrough_session_rejects_*` —
     end-to-end via the handler dispatch (3 tests).

### Phase 2 — Attach integration (pending)

The hard part. Requires a second attach codepath that bypasses
`pane_io::forward_attach`'s renderer/overlay/status machinery:

* Detect `is_passthrough_session` at the listener attach hand-off
  (`listener.rs`, around the existing `pane_io::forward_attach`
  call) and route to a new `forward_attach_passthrough`.
* In that path:
  - Do **not** emit `attach_start_sequence` (no `?1049h`).
  - Skip render-frame emission, status bar, overlays.
  - Replay the active window's `PassthroughReplayLog` to the socket
    as a single frame, then resume verbatim forwarding from the
    pane output channel.
  - Continue feeding inner PTY bytes into both the screen grid (for
    state tracking + future snapshots) and the per-window
    `PassthroughReplayLog`.
* On detach: nothing to undo on the host terminal — we never put it
  in alt-screen.

### Phase 3 — Window switch (pending)

* On `select-window` in a passthrough session, the handler emits the
  outer-state reset sequence + the new window's
  `PassthroughReplayLog::replay_bytes()` over the socket.
* Set OSC 0/2 title to `rmux: <session>/<window-name>` on attach,
  switch, and rename.
* SIGWINCH the new window's inner program with its current size to
  coax lazy-redraw TUIs.

### Phase 4 — Hardening (pending)

* Per-window log allocation tied to the session option budget;
  re-snapshot on overflow.
* CLI flag: `new-session --passthrough` / `-P`.
* End-to-end test: attach a passthrough session, inner program
  emits ANSI bytes, assert the socket bytes are exactly the inner
  bytes (no 1049h, no status row, no border) plus the snapshot
  prefix.
* End-to-end test: window switch replay reproduces the visible
  state of the previous window's last screen.

## Out-of-scope follow-ups

* User-controllable replay budget per session.
* Replay throttling/chunking for very large logs (current plan: one
  socket frame, accept the ~10 ms latency at 1 MiB).
* "Quiet" mode for inactive windows that emit lots of output
  (currently they buffer freely up to the log budget; consider a
  drop-oldest-on-overflow option vs. snapshot-and-truncate).
* Bell / activity notifications surfaced as title-bar prefix or
  terminal bell.
