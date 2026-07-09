# Changelog

## 0.9.0

### Compatibility

- Matches tmux 3.7b format expansion, target resolution, and command parser
  precedence for the 0.9.0 compatibility surface, backed by
  [format oracle tests](tests/formats.rs),
  [target/format matrix tests](tests/acceptance_target_format_matrix.rs), and
  [CLI matrix tests](tests/acceptance_cli_matrix.rs).
- Matches tmux 3.7b source-file and startup config edge cases including CRLF,
  BOM, nested includes, gpakosz/oh-my-tmux corpus handling, and parse-only
  modes, backed by
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
- Matches tmux queued attach sequencing: `attach-session ; detach-client` and
  `attach-session ; <command> ; detach-client` use a real queued attached
  client when a terminal is present, `attach-session ; kill-server` exits with
  the server gone, and an `attach-session` that is final or has no terminal
  tail still performs the normal terminal attach instead of being dropped.
- Updated the command inventory so `list-commands`, help text, parser
  acceptance, source-file handling, and runtime support stay coherent for the
  tmux-compatible 0.9.0 surface.
- Pinned the blocking product oracle to tmux 3.7b and made the differential
  harness fail when the oracle is missing instead of silently skipping.

### Reliability

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
- Integrated the 0.8.1 Windows attach hardening into the 0.9.0 line, including
  PowerShell profile-preserving shell startup, startup render recovery, lossless
  attach overflow handling, and detached control delivery under terminal
  backpressure.
- Documented the detached RPC 0.9.0 wire policy as exact-versioned and added
  fuzz coverage for the detached frame decoder.
- Bumped the detached RPC frame envelope from wire version 3 to 4 for the
  0.9.0 SDK pane-state and pane-option extension boundary.
- Locked SDK armed waits behind capabilities, including the Windows 250ms
  post-ACK settle and the documented best-effort cancellation transport.

### Release

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
