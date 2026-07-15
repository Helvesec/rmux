# tmux Compatibility Decisions

This document is the source of truth for tmux compatibility decisions in RMUX.
A compatibility claim is accepted only after a current head-to-head probe against
tmux and the current RMUX tree.

Compatibility status values:

- `ISO required`: RMUX should match tmux for this behavior. A divergence is a bug
  unless this table is updated with a clear reason.
- `Intentional divergence`: RMUX deliberately differs from tmux because the
  public RMUX product contract is different.
- `Do not copy tmux bug`: tmux behavior is known to be defective or unsafe, and
  RMUX should keep the safer behavior.
- `Deferred`: the behavior is not release-blocking yet, but it must not be
  advertised as tmux-compatible until it is implemented and probed.

## Current Matrix

| Behavior | Status | Current RMUX state | Gate |
| :--- | :--- | :--- | :--- |
| `list-panes -s` lists panes from all windows in the target session | `ISO required` | ISO in current probes | Rust integration or shell head-to-head probe |
| `new-session -e NAME=value` stores the session environment | `ISO required` | ISO in current probes | Rust integration or shell head-to-head probe |
| `load-buffer -` reads paste buffer content from stdin | `ISO required` | ISO in current probes | Rust integration or shell head-to-head probe |
| `break-pane -d -s <only-pane> -t hidden:` moves the pane and removes the empty source session | `ISO required` | Fixed in the current working tree; covered by unit/integration tests | Rust unit/integration tests |
| Invoking the full binary with `argv[0] == "tmux"` prints a tmux-compatible `-V` line | `ISO required` | ISO for the full CLI path | Rust unit or shell head-to-head probe |
| Public `rmux -V` prints RMUX branding | `Intentional divergence` | Intended; external tmux consumers must invoke RMUX through a `tmux` shim/symlink if they require tmux branding | document only |
| Public RMUX help text and RMUX-only commands differ from tmux | `Intentional divergence` | Intended product UX when invoked as `rmux`; tmux-compatible invocation must not advertise unsupported tmux flags | document only |
| `$TMUX=<socket>,...` selects the tmux-compatible socket when no `-S` was passed | `ISO required` | Fixed for tmux-compatible invocation only; native `rmux` intentionally ignores `$TMUX` so an installed local tmux does not redirect RMUX clients | Rust unit test plus shell head-to-head probe |
| `tmux -CC new-session -A -- cmd` streams child stdout as `%output` frames | `ISO required` | ISO: stdin EOF no longer overtakes the attached session or its final pane output | `control_control_eof_waits_for_new_session_output` CLI regression test |
| `set-option -p pane-border-style` and `pane-active-border-style` are visible through `show-options -p -v` | `ISO required` | Fixed in the current working tree; pane scope is accepted and visible through pane-scope show paths | Rust option/server tests |
| `source-file` preserves pane targets for `set-option -p -t session:win.pane ...` | `ISO required` | Fixed in the current working tree; config queue target inference now respects `set-option -p` | Rust scripting tests |
| `join-pane -d -s <only-pane> -t target` moves the pane and removes the empty source session | `ISO required` | Fixed in the current working tree; covered by core and PTY integration tests | Rust unit/integration tests |
| `split-window -d -t <non-active-pane>` does not change the active pane | `ISO required` | Fixed in the current working tree; detached split restores the previously active pane | Rust integration test |
| tmux regex-compatible wording for no-server errors | `Deferred` | Mostly aligned for reviewed paths, but wording should be probed per command before claiming byte-level compatibility | release-review smokes plus targeted probes |
| tmux quirks that expose undefined state or unsafe behavior | `Do not copy tmux bug` | No standing entries yet; add a row with the exact tmux version and reproduction before relying on this status | document only |

## CI Policy

New compatibility fixes should land with focused Rust tests or POSIX shell
head-to-head probes. Avoid adding Python infrastructure for this matrix.

Known gaps are allowed to remain documented while they are intentionally
out-of-scope for a release, but any row marked `ISO required` must move to a
failing Rust/shell regression test when work starts on that behavior.
