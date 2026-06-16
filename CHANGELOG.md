# Changelog

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
