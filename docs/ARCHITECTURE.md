# Architecture

RMUX is a local terminal multiplexer with an optional end-to-end encrypted web
sharing path.

## Components

- **CLI**: parses tmux-style commands, starts or connects to the daemon, and
  renders attached sessions.
- **Daemon**: owns sessions, windows, panes, layouts, hooks, options, buffers,
  status jobs, and process lifecycle.
- **PTY backend**: uses Unix PTYs on Linux and macOS, and ConPTY on Windows.
- **Local IPC**: uses owner-scoped Unix sockets on Linux and macOS, and
  per-user named pipes on Windows.
- **SDK**: provides typed Rust handles for sessions, windows, panes, snapshots,
  waits, streams, lifecycle operations, and command execution.
- **Ratatui widget**: renders pane snapshots inside Rust terminal applications.
- **Web Share**: exposes a selected pane or session to a browser through an
  encrypted WebSocket protocol.
- **Web crypto crate**: implements the Web Share handshake, key schedule, record
  layer, and WebAssembly boundary.

## Local Runtime

The daemon is the authority for terminal state. Shells, PTYs, panes, windows,
scrollback, process state, and session metadata stay on the local machine. Local
clients send typed requests through the local IPC transport and receive typed
responses or rendered output.

## CLI Package Layout

Release packages may install `rmux` as a tiny dispatcher for hot detached
commands. The tiny binary parses a narrow allowlist of tmux-compatible command
forms and sends one direct IPC request to the daemon. Complex command forms fall
back to the canonical full CLI helper installed under `libexec/rmux/rmux` on
Unix and `libexec/rmux/rmux.exe` on Windows.

The helper remains the source of truth for the complete CLI surface, config
loading, attached terminal setup, command queues, hooks, formats, and long-lived
streaming commands. Packagers must ship the public tiny binary and the private
helper together. Setting `RMUX_DISABLE_TINY_CLI=1` forces the public binary to
exec the full helper and is the supported operational kill switch for debugging
tiny-path compatibility issues.

Unix `.tar.gz` archives include an `install.sh` that installs the private helper
and daemon before replacing the public tiny binary. This preserves the `bin/`
and `libexec/` layout for user-local installs such as `~/.local`, where copying
only `bin/rmux` would strand the dispatcher without its full CLI helper.

Windows packages for `0.9.0` use the same public tiny/private helper split:
`rmux.exe` is the public tiny dispatcher, `libexec/rmux/rmux.exe` is the private
full helper, and `rmux-daemon.exe` is the daemon.

Platform-specific behavior is kept behind crate boundaries:

- `rmux-pty` owns PTY and process handling.
- `rmux-ipc` owns local endpoint and transport details.
- `rmux-os` owns small OS-specific helpers.
- `rmux-server` owns daemon state and command execution.

## Web Share Runtime

Web Share separates frontend delivery from terminal execution.

The browser frontend is static HTML, JavaScript, and WebAssembly. It can be
served from `share.rmux.io`, from another CDN, or from a user-controlled static
origin selected with `--frontend-url`. The daemon does not need to serve those
assets.

The browser connects to the daemon through a WebSocket endpoint, directly or
through a tunnel provider. The tunnel is treated as transport only. Terminal
payloads are encrypted between the browser and the local daemon.

## Trust Boundaries

- The local user account is trusted to control its own daemon.
- Other local users are outside the trust boundary.
- Tunnel providers, reverse proxies, and relays are not trusted with terminal
  plaintext.
- The browser page is trusted. Users who want to own that boundary can self-host
  the static frontend.
- Package managers, release assets, and install scripts are part of the delivery
  boundary and are checked in CI before release.

## Release Outputs

The release workflow builds crates, archives, Debian packages, RPM packages,
Windows zips, package-manager metadata, and SHA256 checksums from the same
tagged source. Package-manager metadata pins release asset URLs and checksums
instead of rebuilding unrelated sources.
