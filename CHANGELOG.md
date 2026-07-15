# Changelog

## 0.9.0

### Security

- Authenticates Windows named-pipe servers by the expected user SID and
  integrity level on CLI and SDK connections, and compares canonical pipe
  names without opening them as filesystem paths. A client and daemon running
  at different integrity levels now fail closed with an actionable error
  instead of trusting a same-name endpoint (issue #82).

### Compatibility

- Honors the effective `default-command` for `new-window` and `split-window`
  across direct, queued, sourced, binding, and SDK entry paths while explicit
  commands still win (the remaining command-spawn half of issue #59).
- Inherits the detached caller's working directory for `new-window` and
  `split-window` when `-c` is absent, while `#{session_path}` remains the
  session creation directory rather than following the active pane
  (issues #99 and #100).
- Keeps SDK split targets stable under nonzero `pane-base-index`: slot handles
  resolve through visible coordinates and the returned handle is normalized
  to the new pane's stable id (issue #94).
- Disables Kitty keyboard negotiation by default, including explicit
  set/push/pop/query sequences, rather than activating an incomplete encoder;
  xterm `modifyOtherKeys` behavior remains available (issue #102).
- Identifies XTVERSION replies as `rmux 0.9.0` instead of impersonating a tmux
  release while preserving the compatible DCS response framing; this
  intentional product-identity divergence is recorded as C-D57.
- Makes `send-keys --wait-pane-exit` report the waited pane's exit outcome,
  including a pane removed before the caller observes it, instead of returning
  a false success or losing the retained result after a session rename
  (issue #57).
- Reports each attached client's effective key table after prefix and table
  switches instead of returning the session default, so `list-clients` and
  formats reflect the table that actually handles the next key.
- Applies `pane-base-index` consistently to default `list-panes` output and
  mode-tree pane labels while retaining stable internal pane ids, so a printed
  pane target can be fed back to the CLI without selecting a different pane
  (issues #18 and #94).
- Preserves the resolved target and working directory of queued background
  shell jobs after the initiating client exits, including sourced and binding
  entry paths.
- Aligns compact-flag parsing, command aliases, source-file dispatch, and
  control-mode errors with the advertised command inventory instead of
  accepting flags that are later ignored.
- Retains parse-time assignments while resolving a configured command alias,
  including cold-start queues; tmux 3.7b rejects this ordering, so the scoped
  RMUX extension is recorded as C-D62 rather than claimed as parity.
- Preserves Windows PowerShell profile startup and recovers from incomplete
  tmux-wrapped terminal strings so the reported PowerShell and opencode paths
  remain live (issues #76 and #77).
- Enables outer mouse reporting when the active pane's application requests a
  tracking mode (`?1000`/`?1002`/`?1003`), matching tmux: vim or htop over SSH
  now receive mouse events with the `mouse` option off, and the outer enable
  is dropped again when the pane resets its mode, backed by
  [outer terminal mouse tests](crates/rmux-server/src/outer_terminal/tests.rs).
- Resolves client-less `display-message` targets through the requester's
  `TMUX_PANE`/`RMUX_PANE` environment ahead of any attached client's session,
  pinned end to end (a real child process carrying the environment) by
  [display-message requester tests](crates/rmux-server/src/handler_display_message_tests.rs)
  (issue #83).
- Executes every command of a root mouse binding sequence
  (`select-pane -t = \; run-shell ...`), including the `run-shell` tail, once a
  decoded mouse event reaches the live-attach dispatcher, backed by
  [mouse binding sequence tests](crates/rmux-server/src/handler_send_keys_tests/live_attach.rs).
  For the issue #96 outer-tmux input shape on Windows, a client whose inherited
  parent is tmux now forwards a fully drained batch made exclusively of complete,
  bounded SGR mouse frames instead of classifying it as pasted text. A native
  Windows 10 build 19045 test injects that batch atomically through a real attach
  and observes the live mouse-binding sentinel in
  [the nested-mouse regression](tests/windows_nested_mouse_issue96.rs). Mixed,
  fragmented, malformed, bracketed-paste, non-tmux, and multi-batch input remains
  fail-closed. This does not claim that Windows 10 ConPTY now propagates mouse
  DECSET for RMUX-in-RMUX nesting: a separate native probe still received no SGR
  `KEY_EVENT` data because the enable did not reach the outer pane.
  Note that `run-shell` executes in the daemon's working directory, so
  relative redirection targets land there.
- Adds `Pane::foreground_state_with_revision` to the Rust SDK so best-effort
  foreground snapshots can be ordered against a `state_events` stream, and
  pins the foreground event flow end to end with
  [Unix stream tests](crates/rmux-sdk/tests/pane_queries.rs) and the
  [Windows root-process contract test](crates/rmux-sdk/tests/pane_foreground_events_windows.rs).
  Foreground detection is a periodic probe (about one second) and reports the
  pane root process on Windows, labeled through the per-field sources.
- Explains itself when `rmux claude` cannot attach interactively instead of
  launching silently: a Git Bash/MSYS pty stdin gets a dedicated hint (the
  Windows attach client needs a Win32 console), any other non-terminal stdin
  reports the direct-launch fallback, and `RMUX_CLAUDE_DIRECT=1` keeps the
  explicit direct launch quiet, backed by
  [launcher detection tests](src/cli/claude_launcher.rs).

- Matches tmux 3.7b format expansion, target resolution, and command parser
  precedence for the 0.9.0 compatibility surface, backed by
  [format oracle tests](tests/formats.rs),
  [target/format matrix tests](tests/acceptance_target_format_matrix.rs), and
  [CLI matrix tests](tests/acceptance_cli_matrix.rs).
- Handles source-file and startup config edge cases including BOM, nested
  includes, gpakosz/oh-my-tmux corpus handling, and parse-only modes. RMUX
  deliberately normalizes CRLF and lone CR to LF before lexing, including
  inside quoted text; this is a portability behavior rather than byte-identical
  tmux 3.7b parity. Backed by
  [source-file oracle tests](tests/unix_source_file_tmux_oracle.rs) and
  [source/config matrix tests](tests/acceptance_source_config_matrix.rs).
- Matches tmux 3.7b hook lifecycle, status rendering, root mouse bindings,
  copy-mode defaults, attach passthrough, and control-mode framing where RMUX
  advertises support, backed by
  [hook lifecycle tests](tests/scripting.rs),
  [attach flow tests](tests/cli_attach_flow.rs), and
  [control-mode oracle rows](tests/tmux_compat_surface_matrix/client_control.rs).
- Accepts `%`, `%%`, and `m` as expression-format modulo spellings and
  normalizes the undefined expression-arithmetic cases (divide/modulo by zero,
  NaN/inf and out-of-range operands, and the `-9223372036854775808 / -1`
  overflow) to RMUX's deterministic Linux-oracle sentinel behavior. tmux 3.7b
  leaves these cases to CPU-dependent C conversions, so they are documented as
  an intentional divergence in the
  [tmux divergence ledger entry C-D49](docs/compat/tmux-3.7-divergences.md)
  rather than claimed byte-identical on every CPU.
- Resolves SDK index-based pane handles against the visible pane
  coordinates that `list-panes` reports: slot snapshots now route through the
  resolved stable pane id, so sessions using `base-index`/`pane-base-index`
  no longer return silent all-blank revision-0 snapshots where by-id
  resolution succeeds, backed by
  [SDK pane query tests](crates/rmux-sdk/tests/pane_queries.rs) and the
  [slot snapshot transport pin](crates/rmux-sdk/tests/extract.rs).
- Paints copy-mode selections again: the default
  `copy-mode-selection-style` (`#{E:mode-style}`) now expands through the
  format engine instead of reaching the cell style parser as a raw template,
  which silently dropped every selection highlight, and the selected cells
  compose fg/bg/attributes the way the pinned tmux 3.7b oracle does, backed
  by
  [selection overlay tests](crates/rmux-core/src/screen/tests.rs),
  [renderer expansion tests](crates/rmux-server/src/renderer/tests.rs), and
  [attached copy-mode render tests](crates/rmux-server/src/handler_attach_tests/copy_mode_render.rs)
  (issue #90).
- Tracks application-set OSC 10/11/12 colours per pane and round-trips only
  those known values. Queries for an unknown outer-terminal palette remain
  unanswered instead of returning an invented dark theme, recorded in
  [ledger entry C-D50](docs/compat/tmux-3.7-divergences.md) and backed by
  [emulator reply tests](crates/rmux-core/src/input/tests/osc_dcs_misc.rs)
  (contributed by @nymph-ai).
- Emits mouse-reporting and bracketed-paste enables for any VT outer terminal
  used by a Windows attach client, not only Windows Terminal: non-`WT_SESSION`
  VT outers (including OpenSSH-into-Windows, WezTerm, Alacritty, and the VS
  Code terminal) now take the same RMUX capability path, with no host-brand
  gating. Interactive SSH hosted through ConPTY remains host-dependent:
  ConPTY can consume these DECSET enables before they reach the remote outer
  terminal, so this release fixes and pins RMUX's emission half of issue #93
  without claiming that host limitation is solved, backed by
  [client capability tests](src/client_terminal.rs),
  [daemon capability tests](crates/rmux-server/src/handler_client_runtime.rs),
  and
  [outer-terminal attach-sequence tests](crates/rmux-server/src/outer_terminal/tests.rs).
- Fixes a Windows-only stall where `select-pane` blocked for up to two seconds:
  the command waited for every session's panes to finish starting, so an
  unrelated session's still-starting pane delayed it, and on a detached session
  `select-pane` also waited on its own just-spawned pane with no client to draw.
  Single-pane commands (`select-pane` and its directional / last-pane /
  select-mark variants, plus `resize-pane`, `pipe-pane`, `paste-buffer`) now
  scope that wait to their own target pane, and `select-pane` and every
  sibling now skip the attached-client refresh entirely when no client is
  attached to the session so a still-starting sibling cannot stall a detached
  select or resize either. The dispatch-level cap is `DEFERRED_PANE_WAIT`
  (10 s), reached only when the addressed pane genuinely never registers.
  Backed by
  [the deferred-pane session-scope test](tests/windows_terminal_matrix.rs).
- Emits OSC 52 clipboard writes from panes toward the outer terminal on
  Windows: under `set-clipboard on` an application's write is sent through the
  attach path and stored in a paste buffer (kept for a detached client, shown
  in `list-buffers`, and firing the pane-set-clipboard hook after the buffer is
  stored so the hook can read the new content via `#{buffer_sample}`), while
  the `external` default and `off` create nothing so untrusted pane output
  cannot drive the clipboard — matching tmux's on-only gate on application
  clipboard writes. `set-buffer -w` uses the same outbound OSC 52 path. RMUX
  advertises that capability for every Windows attach, but effective system
  clipboard delivery is host-dependent: Windows 10 build 19045 and other
  pre-22621 ConPTY paths can consume OSC 52 before the outer terminal sees it.
  Neither path is therefore claimed to update the system clipboard on every
  Windows host. Two behaviour changes ship on every platform so the daemon
  matches the tmux 3.7b oracle exactly: under the `external` default a
  pane's OSC 52 write no longer reaches the outer clipboard on Unix (tmux drops
  external pane-origin OSC 52 writes; use `set -g set-clipboard on` to restore
  forwarding to the outer and start creating paste buffers), and malformed or
  empty-payload OSC 52 writes are validated then dropped instead of forwarded
  verbatim. Recorded in
  [tmux divergence ledger entry C-D52](docs/compat/tmux-3.7-divergences.md) and
  backed by
  [outer-terminal gate tests](crates/rmux-server/src/outer_terminal/tests.rs),
  [client and daemon capability tests](src/client_terminal.rs), and
  [inbound clipboard buffer tests](crates/rmux-server/src/handler_alert_tests.rs)
  (issue #91).
- Delivers bracketed paste to Windows attach panes: a paste into a pane that
  enabled bracketed-paste mode now arrives wrapped in `ESC[200~`/`ESC[201~`
  (and is stripped for a pane that did not) after detecting the console-input
  burst a paste produces. Native mouse records coalesced with a detected paste
  are suppressed without making its text live, while their button state still
  advances; SGR-looking bytes delivered as key records remain pasted text
  rather than being trusted as mouse provenance. Paste-marker stripping runs
  to a fixed point and carries a small tail across `ReadConsoleInputW` batches
  so a hostile clipboard cannot slip a live `ESC[201~` out of the paste
  envelope either by reassembly or by straddling a batch boundary. AltGr
  characters and the pure modifier key-downs classic conhost interleaves
  around shifted content no longer defeat burst detection. The scrub is also
  applied on every platform whenever a real terminal paste arrives while the
  command prompt, mode-tree, no-job popup, menu, or display-panes overlay is
  open — the leading `ESC` of `ESC[200~` used to close the overlay and leak the
  body to the pane's shell on Unix and inside the Windows attach client alike.
  The Windows-specific detection heuristic is recorded in
  [tmux divergence ledger entry C-D51](docs/compat/tmux-3.7-divergences.md) and
  backed by
  [console input tests](crates/rmux-client/src/attach_windows/input.rs)
  (issue #92).
- Ships a Windows installer that preserves the tiny/libexec package layout:
  `install.ps1` in the release zip and `scripts/install-windows.ps1` verify
  the release checksum, install the private helper before the public
  dispatcher, and are exercised by the
  [package verifier](scripts/verify-package-windows.ps1) (issue #86,
  contributed by @isacgalvao).
- Matches the tmux 3.7b `set-option -U` scope matrix: plain `-U` unsets the
  session copy only, `-pU` the pane copy only, and `-wU` the window copy plus
  the window's pane overrides, replacing the previous RMUX-specific
  window-scope error, backed by
  [CLI scope matrix tests](tests/cli_surface.rs) and
  [option effect tests](crates/rmux-core/src/options/tests/effects_defaults.rs).
- Matches tmux 3.7b `run-shell -C` semantics: trailing positional arguments
  are accepted and ignored, and nested `attach-session` or non-detached
  `new-session` fail with `open terminal failed: not a terminal`, backed by
  [scripting tests](tests/scripting.rs) and
  [queued run-shell tests](crates/rmux-server/src/handler_scripting_tests/run_shell.rs).
- Matches tmux 3.7b `split-window -d` semantics: a detached split no longer
  activates the new pane, fabricates a last pane, or advances the
  `list-panes -O activity` selection counter, backed by
  [window surface tests](tests/cli_window_surface.rs).
- Rejects `paste-buffer` into a dead remain-on-exit pane with the tmux
  `target pane has exited` error while `send-keys` and attached input keep
  tmux's silent tolerance for dead panes, backed by
  [buffer handler tests](crates/rmux-server/src/handler_buffer_tests.rs).
- Resolves `show-options -gp` and `set-option -gp` against the real global
  pane scope instead of treating the combination as a silent no-op, backed by
  [CLI scope matrix tests](tests/cli_surface.rs).
- Handles queued attach sequencing: `attach-session ; detach-client` and
  `attach-session ; <command> ; detach-client` use a real queued attached
  client when a terminal is present, `attach-session ; kill-server` exits with
  the server gone, and an `attach-session` that is final or has no terminal
  tail still performs the normal terminal attach instead of being dropped,
  backed by [queued attach flow tests](tests/cli_attach_flow.rs).
- On plain `-C` input EOF, emits `%exit` for the active terminal frame and
  closes the control transport; clients treat transport closure, not
  control-looking pane output, as authoritative. `-CC` keeps an attached
  session alive through its final pane output, matching tmux 3.7b
  control-control lifecycle semantics. Plain control mode retains the
  requester's identity and permissions long enough to finish already accepted
  finite automation. Frames that would wait indefinitely are cancelled,
  shutdown still wins, and a replacement client with the same PID cannot
  overtake the detached drain. tmux 3.7b instead drops later queued frames
  after EOF, so the intentional automation-preserving behavior is recorded in
  [ledger entry C-D54](docs/compat/tmux-3.7-divergences.md) and backed by
  [control EOF tests](crates/rmux-server/src/control/tests.rs).
- Starts control-mode subscriptions for an existing session at the pane output
  cursor captured by `attach-session`, `new-session -A`, or `switch-client`, so
  historical pane output is not replayed while output from a newly created
  session remains available. This matches tmux 3.7b and is backed by
  [control CLI tests](tests/cli_surface.rs).
- Bounds every retained attached-input family: ambiguous keyboard prefixes use
  `escape-time`, recognized streaming paste/OSC/APC bodies use an eight-second
  idle budget, malformed or oversized SGR mouse frames are consumed within a
  fixed syntax bound, and a timed-out paste cannot release a partial delimiter
  as live input. These safer-than-tmux choices are recorded in
  [ledger entries C-D53 and C-D55](docs/compat/tmux-3.7-divergences.md) and
  backed by
  [pending-input tests](crates/rmux-server/src/pane_io/tests.rs),
  [mouse decoder tests](crates/rmux-server/src/input_keys/tests.rs), and
  [paste timeout tests](crates/rmux-server/src/handler_pane/attached_input/bracketed_paste.rs).
- Updated the command inventory so `list-commands`, help text, parser
  acceptance, source-file handling, and runtime support stay coherent for the
  tmux-compatible 0.9.0 surface.
- Pinned the blocking product oracle to tmux 3.7b and made the differential
  harness fail when the oracle is missing instead of silently skipping.

### Reliability

- Revalidates the token and expiry of an owned-session lease under the state
  lock before reaping it, so a concurrent renewal cannot remove a still-owned
  session.
- Preserves Web pre-authentication fairness through loopback tunnels, retains
  authenticated viewers while they answer pings, expires silent peers, and
  accounts outbound backlog before publishing it.
- Cancels abandoned tunnel creation and revokes a newly created Web share when
  its response cannot be delivered, preventing an SDK timeout or disconnect
  from leaving an unreachable share active.
- Owns attach shell commands and `pipe-pane` helpers as complete Unix process
  groups or Windows Jobs before they run. Shutdown, replacement, and stopped
  shell paths clean up the group and restore the terminal; Unix popup shutdown
  now uses a bounded HUP/TERM/KILL escalation.
- Relays `set-clipboard on` OSC 52 writes from a visible inactive pane to the
  outer terminal of each matching attach client without duplicating the active
  pane's normal output path. Routing is keyed by stable session and window
  identity, and slow-client control backlogs remain bounded (issue #91).
- Serializes popup PTY writes and resizes through one FIFO worker outside the
  async runtime and never waits for blocking PTY I/O while holding attach
  state. Windows popup exit drops the owning Job Object before closing ConPTY,
  so a blocked reader cannot strand the server runtime.
- Isolates live attach output by pane source so a multi-client switch cannot
  replay terminal passthroughs or render frames from the previous target, and
  keeps SDK pane-state subscriptions alive across respawn generations.
- Keeps copy-mode refresh fanout, owned-session lease renewal, Windows attach
  resume, and output draining ordered across their asynchronous boundaries.
- Carries stable session/window/pane identities through queued destructive
  commands and control-mode pane subscriptions, preventing a same-name
  recreate from receiving or destroying state that belonged to its predecessor.
- Migrates deferred Windows pane-start records with a renamed session instead
  of stranding the eventual process under the old name.
- Bounds the latency-sensitive caller wait for ordered lifecycle hooks while
  leaving the hook and FIFO completion drain alive; a slow foreground hook no
  longer freezes every attach client for the full hook timeout.
- Hardens Windows attach and input recovery: completed lock actions always
  release their local lock, isolated key records cannot be misclassified as a
  paste solely because another console event remains queued, the temporary
  Ctrl-C ignore guard remains alive through console detach, and large
  `WriteConsoleW` output is emitted in UTF-16-safe bounded chunks.
- Requires Windows daemon auto-start to escape restrictive job objects instead
  of silently starting a daemon that dies with its launcher, and reports the
  unsupported host policy when breakaway is denied.
- Expands `~` in Windows configuration from nonempty `HOME`, falling back to
  `USERPROFILE`, while retaining the existing Unix environment policy.
- Makes Windows archive upgrades transactional: installation is staged and
  verified before replacement, with the previous working package restored if
  the swap cannot complete.
- Hardened Unix socket startup and cleanup, including bare relative `-S` paths,
  daemon churn, exit-empty behavior, and worktree socket hygiene.
- Added RMUX-native SDK pane option APIs, pane-state event streams, and
  best-effort pane foreground state behind explicit detached RPC capabilities.
  The state stream uses an initial snapshot plus monotone long-poll revisions
  for pane titles, pane-local options, foreground changes, and close events
  without claiming tmux compatibility for these RMUX extensions.
- Kept native Windows daemon upgrade, shell launch, queued quoting, input, Ctrl,
  mouse, and package smokes in the release gate so Windows regressions remain
  review-blocking.
- Integrated post-0.8.0 Windows attach hardening, first released in 0.9.0,
  including PowerShell profile-preserving shell startup, startup render
  recovery, lossless attach overflow handling, and detached control delivery
  under terminal backpressure.
- Documented the detached RPC 0.9.0 wire policy as exact-versioned and added
  fuzz coverage for the detached frame decoder.
- Bumped the detached RPC frame envelope from wire version 3 to 5 across the
  0.9.0 SDK pane-state/pane-option boundary (wire 4) and explicit initial
  control-command framing boundary (wire 5), while retaining the wire-4
  compatibility fixture ledger. This is a hard cut for ordinary RPCs: clients
  and SDKs must use the matching 0.9.0 daemon; an
  already-running older server must be
  restarted during upgrade. As a shutdown-recovery exception, the listener
  accepts only the published wire 1–3 zero-sized `kill-server` frames; no other
  legacy request bypasses the exact-version decoder.
- Locked SDK armed waits behind capabilities, including the Windows 250ms
  post-ACK settle and the documented best-effort cancellation transport.

### Release

- Makes installer rollback cover the complete package: Windows restores the
  dispatcher and private runtime together, Unix restores packaged assets
  without overwriting unrelated local files, and WinGet preserves the nested
  archive layout.
- Added release-0.9.0 performance baselines for attach render, large
  source-file corpus, status format-heavy expansion, hook storm, and daemon
  churn, with SHA, environment, and version stamps.
- Extended resource gates to cover daemon churn process/socket leaks and
  rejected debug-tree binaries for perf baselines and resource smokes.
- Finalized the intentional tmux 3.7b divergence ledger and package artifact
  checks, including release artifact smokes, package hygiene, and the
  root-crate crates.io decision.

## 0.8.0

### Security

- Escaped control-mode window and paste-buffer names before emitting
  notifications, preventing injected control frames from newline-bearing names.
- Rejected overflowing and non-minimal varint frame lengths in the protocol
  decoder while preserving the existing minimal encoder output.
- Preserved UTF-8 character boundaries when truncating WebSocket close reasons.
- Capped negative format padding widths before allocation to avoid pathological
  padding requests.
- Disabled persisted GitHub checkout credentials in the release workflow.

### Reliability

- Added panic-safe connection cleanup so subscriptions and SDK waits are removed
  even if a connection task unwinds.
- Gated two-phase SDK wait armed acknowledgements behind an explicit
  `sdk.waits.armed` capability and kept dropped SDK waits from blocking the
  shared transport.
- Hardened Windows Ctrl-C delivery for attached clients and `send-keys` so
  foreground console programs such as Python and `ping.exe` receive native
  interrupts while raw-mode console applications still receive Ctrl-C bytes.
- Moved blocking Windows console attach writes behind a dedicated output queue
  and cleared QuickEdit when entering raw mode so paused console output does not
  freeze input forwarding.
- Routed multi-token Windows `send-keys` sequences containing `C-c`, such as
  `send-keys C-c Enter`, through the native Ctrl-C path instead of falling back
  to a raw `0x03` byte.
- Added a Unix archive installer that preserves the tiny/full `bin` and
  `libexec` layout and installs the public tiny binary last during upgrades.
- Made `rmux claude install-skill` back up existing user skills before
  replacing them, while refusing to overwrite symlinks.
- Made connection subscription/wait cleanup tolerate poisoned cleanup locks.
- Spawned popup waiters before popup readers so popup children are reaped even
  if reader setup fails.
- Relaxed the Windows ConPTY Ctrl-D timeout smoke when the host leaves
  `timeout.exe` running, preserving coverage without failing on runner-specific
  behavior.
- Resolved tiny CLI helpers through canonical executable paths, covering
  portable aliases such as WinGet Links while keeping packaged layouts first.
- Resolved hidden daemon siblings through canonical executable paths so portable
  aliases can cold-start the packaged daemon directly.
- Resolved SDK daemon candidates from installed `rmux` binaries before falling
  back to PATH names, avoiding extra tiny-helper hops in portable layouts.

### Compatibility

- Matched tmux `source-file -n -v` behavior for implicit-target commands such
  as `clear-history`, keeping parse-only diagnostics usable on public tmux
  configurations.
- Reported missing optional nested config files sourced through the tmux shim as
  plain client messages instead of startup `config error:` diagnostics, matching
  tmux-style fallback config workflows such as oh-my-tmux.
- Matched tmux 3.4 missing-path `source-file` diagnostics (`<path>: No such
  file or directory`) and let a later successful queued `run-shell` clear an
  earlier non-zero source-file status.
- Routed public clients launched from RMUX panes through RMUX-owned inherited
  `$TMUX` socket paths, while still ignoring foreign tmux sockets, so nested
  `rmux has-session` and `new-session -A` target the calling pane's server.
- Restored sparse-map deserialization defaults for `NewSessionExtRequest` and
  related request types without changing the bincode wire layout.
- Added a defensive `with-session` empty-command guard before lease creation.
- Restored tmux 3.4-compatible binary boolean format semantics for `&&:` and
  `||:`; use nested boolean formats for portable multi-operand conditions.
- Matched tmux 3.4 expression-format arithmetic for empty operands, `0x`
  integer literals, and integer divide-by-zero sentinel results covered by the
  0.8 compatibility gate.
- Matched tmux 3.4 substitution-format behavior for empty and zero-width
  regex patterns, avoiding synthetic insertions that tmux leaves untouched.
- Matched tmux control-mode escaping for DEL and fixed HEAD responses to omit
  bodies.
- Corrected `input-buffer-size` validation and rejected bare non-boolean choice
  toggles such as `set -g mode-keys`.
- Added `Display` and `Error` implementations for `StartServerError`, and
  corrected public split-direction documentation.
- Shared detected client terminal features between the full and tiny attach
  paths, including Windows Terminal rendering and input capabilities.
- Improved SDK and CLI daemon startup diagnostics when no daemon binary can be
  found or a hidden daemon exits before creating its endpoint.
- Matched tmux `resize-pane` precedence for repeated relative directions and
  composed absolute `-x`/`-y` dimensions with later relative adjustments across
  CLI, tiny, and `source-file` paths.
- Matched tmux 3.4 `join-pane`/`move-pane` legacy `-p` handling: bare
  `-p` reports `size missing`, while `-p`/`-p50` paired with `-l` is accepted
  as a compatibility modifier and preserves the explicit `-l` size.
- Matched tmux pane-transfer defaults across CLI and `source-file` paths:
  `join-pane`, `move-pane`, and `swap-pane` now fall back from `{marked}` to
  the current pane when `-s` is omitted, and same-window `join-pane`/`move-pane`
  apply `-l` sizing to the same side of the split as tmux.
- Preserved tmux `swap-pane -d` active-pane behavior when the active pane is
  neither the source nor the target pane.
- Aligned pane lifecycle hooks with tmux by firing `pane-exited` for natural
  pane exits while no longer synthesizing it for `kill-pane`/`respawn-pane -k`,
  and by avoiding synthetic `pane-focus-in`/`pane-focus-out` hooks on detached
  `select-pane` changes.
- Aligned stable pane-id SDK/Web kill and respawn paths with the CLI by no
  longer synthesizing `pane-exited` for explicit `kill-pane` or
  `respawn-pane -k` operations.
- Preserved coalesced `pane-title-changed` notifications when title changes are
  batched with activity or bell alerts for the same pane.
- Parsed compact queued `set-hook`/`show-hooks` flag clusters such as `-ga` and
  `-gpR` in tmux config/source-file paths.
- Bumped the detached RPC frame envelope to wire version 3 and refreshed the
  compatibility fixtures so stale v1/v2 frames are rejected explicitly. This is
  an intentional 0.8 line boundary: restart 0.7.x daemons after upgrading, and
  run 0.8 SDK/client binaries against 0.8 daemons.
- Preserved pending `DoubleClick1Pane` dispatch when a later mouse event arrives
  after the click timeout but before the async timer task runs.
- Restored tmux 3.4 root mouse bindings for pager/alternate-screen copy
  interactions by removing the non-tmux `alternate_on` branch from
  `MouseDrag1Pane`, `MouseDown2Pane`, `WheelUpPane`, `DoubleClick1Pane`, and
  `TripleClick1Pane`.
- Parsed compact queued `display-message` clusters such as `-pt target` the same
  way as tmux, fixing source-file and binding commands that combine print and
  target flags.
- Rejected extra `display-message` message operands in both CLI and queued
  `source-file` paths instead of silently joining them.
- Consumed bracketed paste while a pane is in copy-mode instead of leaking the
  pasted payload into the underlying PTY.

### CI

- Bumped release-facing versions to `0.8.0` across Cargo workspace metadata,
  `Cargo.lock`, the manpage, snap metadata, README download links, and localized
  README files.
- Serialized the Windows attach/control-key integration probe group under
  nextest and extended the Windows Ctrl matrix smoke script to exercise
  multi-token `send-keys C-c Enter`.
- Hardened the Windows Ctrl matrix `send-keys` harness so single-token controls
  such as `C-a` and `Escape` are passed as native command arguments rather than
  as literal probe text.
- Added a Windows package smoke that exercises portable alias fallback, PATH
  resolution, and daemon startup from a WinGet-like Links layout.
- Exposed `rmux-daemon` in the Scoop manifest and expanded package-manager
  metadata smokes to cover the installed daemon path.
- Added reusable installed-package smokes that force the tiny helper fallback
  before exercising daemon startup.
- Extended Unix archive verification to install the archive into a temporary
  prefix and smoke the installed `bin/rmux` through its packaged helper.
- Added release-gate coverage for `rmux-server --lib` and SDK wait-cancellation
  regressions so deterministic red tests cannot pass through review-only gates.

## 0.7.1

### Security

- Escaped control-mode window and paste-buffer names before emitting
  notifications, preventing injected control frames from newline-bearing names.
- Rejected overflowing and non-minimal varint frame lengths in the protocol
  decoder while preserving the existing minimal encoder output.
- Preserved UTF-8 character boundaries when truncating WebSocket close reasons.
- Capped negative format padding widths before allocation to avoid pathological
  padding requests.
- Disabled persisted GitHub checkout credentials in the release workflow.

### Reliability

- Added panic-safe connection cleanup so subscriptions and SDK waits are removed
  even if a connection task unwinds.
- Added a Unix archive installer that preserves the tiny/full `bin` and
  `libexec` layout and installs the public tiny binary last during upgrades.
- Made connection subscription/wait cleanup tolerate poisoned cleanup locks.
- Spawned popup waiters before popup readers so popup children are reaped even
  if reader setup fails.
- Relaxed the Windows ConPTY Ctrl-D timeout smoke when the host leaves
  `timeout.exe` running, preserving coverage without failing on runner-specific
  behavior.
- Resolved tiny CLI helpers through canonical executable paths, covering
  portable aliases such as WinGet Links while keeping packaged layouts first.
- Resolved hidden daemon siblings through canonical executable paths so portable
  aliases can cold-start the packaged daemon directly.
- Resolved SDK daemon candidates from installed `rmux` binaries before falling
  back to PATH names, avoiding extra tiny-helper hops in portable layouts.

### Compatibility

- Restored sparse-map deserialization defaults for `NewSessionExtRequest` and
  related request types without changing the bincode wire layout.
- Added a defensive `with-session` empty-command guard before lease creation.
- Matched tmux control-mode escaping for DEL and fixed HEAD responses to omit
  bodies.
- Corrected `input-buffer-size` validation and rejected bare non-boolean choice
  toggles such as `set -g mode-keys`.
- Added `Display` and `Error` implementations for `StartServerError`, and
  corrected public split-direction documentation.
- Shared detected client terminal features between the full and tiny attach
  paths, including Windows Terminal rendering and input capabilities.
- Improved SDK and CLI daemon startup diagnostics when no daemon binary can be
  found or a hidden daemon exits before creating its endpoint.

### CI

- Bumped release-facing versions to `0.7.1` across Cargo workspace metadata,
  `Cargo.lock`, the manpage, snap metadata, README download links, and localized
  README files.
- Added a Windows package smoke that exercises portable alias fallback, PATH
  resolution, and daemon startup from a WinGet-like Links layout.
- Exposed `rmux-daemon` in the Scoop manifest and expanded package-manager
  metadata smokes to cover the installed daemon path.
- Added reusable installed-package smokes that force the tiny helper fallback
  before exercising daemon startup.
- Extended Unix archive verification to install the archive into a temporary
  prefix and smoke the installed `bin/rmux` through its packaged helper.

## 0.7.0

- Added the tiny public CLI package layout for hot detached commands, with the
  full canonical CLI installed as a private libexec helper for complex paths.
- Added direct tiny CLI paths for common operations including session creation,
  split/resize, capture, display-message, send-keys, source-file, list-sessions,
  and kill-server, while preserving helper fallback for unsupported forms.
- Added `RMUX_DISABLE_TINY_CLI=1` as an operational kill switch for reverting to
  the full CLI helper when diagnosing tiny-path compatibility issues.
- Improved detached command performance and release benchmarking discipline with
  baseline metadata, perf-diff tooling, and release-review smoke gates for the
  tiny package layout.
- Added additive protocol variants and capabilities for target-action and
  capture fast paths while keeping legacy wire variants available.
- Hardened tmux compatibility for repeated short flags, queue separators, tiny
  error surfaces, source-file exit status, and mutating target-action retry
  safety.
- Updated release packaging, snap metadata, manpage/version surfaces, and
  localized download references for `v0.7.0`.

## 0.6.5

- Added release artifacts for `linux-aarch64` alongside the existing Linux,
  macOS, and Windows archives.
- Added Sigstore keyless signing for `SHA256SUMS` and GitHub build provenance
  attestations for release assets. The documented provenance level is SLSA Build
  Level 2.
- Updated release documentation, direct-download filenames, and package-manager
  examples for `v0.6.5`.
- Added APT repository metadata for both `amd64` and `arm64` Debian packages.
- Declared the Microsoft Visual C++ runtime dependency in Windows package
  manager metadata for MSVC release builds.
- Hardened incomplete terminal parser input and SDK line-stream buffering
  against unbounded growth.

## 0.6.1

- Published the first patch release in the 0.6 line with the 0.6.0 feature set
  and release packaging/documentation fixes.

## 0.6.0

- Added the typed Rust SDK surface for session, window, pane, snapshot, locator,
  expectation, stream, and web-share automation workflows.
- Added `capabilities` discovery output for SDK and scripting clients.
- Added hybrid post-quantum, end-to-end encrypted `web-share` for pane and
  session sharing through a static browser frontend.
- Added generated shell completions for bash, zsh, fish, PowerShell, and
  Elvish without moving the tmux-compatible runtime parser to clap subcommands.
- Hardened web-share backpressure handling with atomic session keyframes,
  prioritized control frames, protocol capability negotiation, and pane scroll
  patching.
- Bumped the detached daemon wire protocol to v2. Existing v0.5.x daemons must
  be restarted after upgrading clients to v0.6.0.
- Improved tmux compatibility across command parsing, target resolution,
  copy-mode, resize/layout behavior, options, hooks, list-keys, and JSON output.
- Kept product semantics where tmux-compatible behavior would copy known tmux
  bugs. RMUX keeps exact-zero `if-shell -F` truthiness, saturating integer
  format arithmetic, strict invalid capture bounds, and literal trailing `#`
  format text.
- Kept RMUX-native foreground `run-shell` stdout forwarding and raw attached
  input byte preservation, rather than copying tmux behaviors that hide command
  output or drop malformed/non-UTF-8 input bytes.
- Removed unsupported tmux-incompatible parser extensions from listing commands,
  including `list-keys -r`, `list-clients -r`, and list buffer/client sort flags.
- Removed the earlier rmux-only multi-pair conditional format extension; use
  nested conditionals for portable configuration.
- Fixed native attach render coalescing so full repaint frames are latest-wins
  without starving the terminal under continuous output.
- Fixed OSC52 passthrough delivery races for native attached clients.
- Fixed client environment propagation for unset tombstones and non-UTF-8
  process environment values.
- Kept `rmux -V` branded as `rmux 0.6.0`; use `rmux diagnose --json` for build
  and platform diagnostics.

## 0.5.0

- Initial public release of RMUX as a tmux-compatible Rust terminal multiplexer
  with Unix and Windows daemon/client support.
