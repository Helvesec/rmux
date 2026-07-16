# Changelog

## 0.9.0

### Security

- Authenticate Windows named-pipe servers against the expected user SID,
  integrity level, and canonical pipe name.
- Bound pre-authentication, terminal parser, input, queue, and outbound backlog
  resources so malformed or stalled clients fail closed.
- Own helper processes through Unix process groups or Windows Job Objects and
  clean them up during shutdown, replacement, and failed startup.

### SDK

- Add pane option APIs, revisioned pane-state streams, foreground process
  snapshots, explicit argv or shell process specs, and stable pane identities.
- Resolve index-based pane handles through visible coordinates while retaining
  stable pane ids across base-index and pane-base-index changes.
- Preserve armed wait, cancellation, lifecycle, and ownership semantics across
  reconnects and pane respawns.

### Compatibility

- Expand the tmux 3.7b compatibility surface across command parsing,
  source-file handling, formats, hooks, control mode, copy mode, targets, and
  queued commands.
- Honor default-command, caller working directories, pane-base-index, key
  tables, and detached split semantics consistently across CLI, SDK, bindings,
  and sourced commands.
- Improve startup config compatibility, including nested includes, BOM and CRLF
  handling, aliases, assignments, and the gpakosz configuration corpus.
- Keep XTVERSION product identification honest by reporting RMUX rather than a
  tmux release.
- Disable incomplete Kitty keyboard negotiation while retaining supported
  xterm modifyOtherKeys behavior.
- Improve OSC colour and clipboard handling, Windows bracketed paste, outer
  mouse reporting, and root mouse binding sequences.
- Preserve control-mode framing and finite accepted work after input EOF while
  cancelling operations that could wait indefinitely.

### Reliability

- Harden Windows shell startup, attach recovery, console input, daemon
  breakaway, native upgrades, and package rollback.
- Stabilize pane journals, hook ordering, leases, queued background jobs,
  terminal reflow, attach transitions, and multi-client output isolation.
- Make Unix socket startup and cleanup reliable for relative socket paths,
  daemon churn, and exit-empty behavior.
- Bump the detached RPC frame envelope from wire version 3 to 5. Clients and
  SDKs must match the 0.9.0 daemon, and an already-running older server must be
  restarted during upgrade.

### Packaging

- Preserve the complete Windows dispatcher and private runtime layout in
  archives and installer upgrades (contributed by @isacgalvao).
- Add deterministic release baselines and cross-platform package, resource,
  protocol, and runtime gates.
- Keep the root `rmux` crate installable with
  `cargo install rmux --locked`.

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
