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
