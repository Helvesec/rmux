# Changelog

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

### CI

- Bumped release-facing versions to `0.7.1` across Cargo workspace metadata,
  `Cargo.lock`, the manpage, snap metadata, README download links, and localized
  README files.
- Added a Windows package smoke that exercises portable alias fallback, PATH
  resolution, and daemon startup from a WinGet-like Links layout.

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
  bugs. RMUX keeps n-ary boolean formats, exact-zero `if-shell -F` truthiness,
  saturating integer format arithmetic, strict invalid capture bounds, and
  literal trailing `#` format text.
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
