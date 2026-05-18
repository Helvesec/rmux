<div align="center">

<a href="https://rmux.io">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://rmux.io/rmux-header-dark.svg">
    <img src="https://rmux.io/rmux-header.svg" alt="RMUX" width="500">
  </picture>
</a>


**Universal Rust multiplexer for the agentic era: detachable, scriptable, and inspectable, with a tmux-compatible CLI, daemon-backed SDK, and native [Ratatui](https://ratatui.rs) integration.**

English · [Français](README.fr.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja.md)

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![Release validation](https://github.com/Helvesec/rmux/actions/workflows/ci.yml/badge.svg)](https://github.com/Helvesec/rmux/actions/workflows/ci.yml)
[![rmux 0.2.0](https://img.shields.io/badge/rmux-0.2.0-informational.svg)](#install)
[![Platform: Linux | macOS | Windows](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey.svg)](#platform-support)
[![Unsafe policy](https://img.shields.io/badge/unsafe-restricted-success.svg)](#verification)

<br />
<a href="https://rmux.io">
  <img src="https://rmux.io/rmux-terminal-demo.gif" width="500" alt="RMUX terminal session demo" />
</a>

</div>

> [!IMPORTANT]
> Released on **16 May 2026**. All 90 tmux-compatible commands are implemented, but bugs are expected — this is a fresh public preview. Please [file issues](https://github.com/helvesec/rmux/issues) if you hit one.

## What RMUX Provides

- **A tmux-style CLI** for sessions, windows, panes, buffers, hooks, formats, copy mode, control mode, and common terminal workflows.
- **A Rust SDK** for creating sessions, splitting panes, sending typed input, reading snapshots, subscribing to pane output, waiting for text or bytes, and shutting down cleanly.
- **A ratatui widget** that renders pane snapshots into a `ratatui::buffer::Buffer` without requiring async work in the draw path.
- **Native local runtime support**: Unix PTYs and Unix sockets on Linux/macOS; ConPTY and named pipes on Windows — no WSL required.
- **A small published crate set** with internal implementation crates kept out of the public package surface.

## Install

Prebuilt binary for macOS and Linux:

```sh
curl -fsSL https://rmux.io/install.sh | sh
```

Prebuilt binary for Windows PowerShell:

```powershell
irm https://rmux.io/install.ps1 | iex
```

Direct downloads and SHA256 checksums are available from the [v0.2.0 GitHub Release](https://github.com/helvesec/rmux/releases/tag/v0.2.0).

From crates.io with Cargo:

```sh
cargo install rmux --locked
```

From a local checkout:

```sh
cargo install --path . --locked
```

For Rust applications:

```sh
cargo add rmux-sdk
cargo add ratatui-rmux
```

## CLI Quickstart

```sh
rmux new-session -d -s work
rmux split-window -h -t work
rmux send-keys -t work 'echo "hello from rmux"' Enter
rmux attach-session -t work
```

Use command help locally:

```sh
rmux list-commands
rmux new-session --help
rmux split-window --help
```

## SDK Quickstart

```toml
[dependencies]
rmux-sdk = "0.2"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

```rust
use std::time::Duration;

use rmux_sdk::{
    EnsureSession, EnsureSessionPolicy, Rmux, SessionName, TerminalSizeSpec,
};

#[tokio::main]
async fn main() -> rmux_sdk::Result<()> {
    let rmux = Rmux::builder()
        .default_timeout(Duration::from_secs(5))
        .connect_or_start()
        .await?;

    let session_name = SessionName::new("work").expect("valid session name");
    let session = rmux
        .ensure_session(
            EnsureSession::named(session_name)
                .policy(EnsureSessionPolicy::CreateOrReuse)
                .detached(true)
                .size(TerminalSizeSpec::new(120, 32)),
        )
        .await?;

    let pane = session.pane(0, 0);
    pane.send_text("printf 'ready\\n' && sleep 1\n").await?;

    pane.wait_for_text("ready").await?;
    let snapshot = pane.snapshot().await?;
    println!("{}x{}", snapshot.cols, snapshot.rows);

    Ok(())
}
```

## Ratatui Widget

```rust
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};
use ratatui_rmux::{PaneState, PaneWidget};
use rmux_sdk::PaneSnapshot;

fn render(snapshot: PaneSnapshot, area: Rect, buffer: &mut Buffer) {
    let state = PaneState::from_snapshot(snapshot);
    PaneWidget::new(&state).render(area, buffer);
}
```

## Architecture

<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://rmux.io/rmux-architecture-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="https://rmux.io/rmux-architecture-light.png">
  <img src="https://rmux.io/rmux-architecture-dark.png" alt="RMUX runtime architecture" width="800">
</picture>

</div>

Three public surfaces — a `rmux` CLI, a `rmux-sdk` Rust crate, and a `ratatui-rmux` widget — share a single local protocol to talk to the daemon. Anything one surface can do, the others can do too.

## Workspace

| Crate | Role | Publication |
| :--- | :--- | :--- |
| `rmux-types` | Shared platform-neutral value types | public |
| `rmux-proto` | Detached IPC DTOs, framing, wire-safe errors | public |
| `rmux-os` | Small OS boundary helpers | public |
| `rmux-ipc` | Local IPC endpoints and transports | public |
| `rmux-sdk` | Daemon-backed Rust SDK | public |
| `ratatui-rmux` | Ratatui integration widget | public |
| `rmux-pty` | PTY allocation, resize, child process control | support crate |
| `rmux-core` | Sessions, panes, layouts, formats, hooks, buffers | support crate |
| `rmux-server` | Tokio daemon and request dispatch | support crate |
| `rmux-client` | Local IPC client and attach plumbing | support crate |
| `rmux` | CLI and hidden daemon entrypoint | public binary |
| `rmux-render-core` | Shared snapshot rendering core | workspace-internal |

## Platform Support

| Platform | PTY backend | IPC backend | Default endpoint |
| :--- | :--- | :--- | :--- |
| Linux | Unix PTY | Unix socket | `/tmp/rmux-{uid}/default` |
| macOS | Unix PTY | Unix socket | `/tmp/rmux-{uid}/default` |
| Windows | ConPTY | Named pipe | per-user named pipe |

## Configuration

On Linux and macOS, RMUX reads `.rmux.conf` from the standard system and user locations:

1. `/etc/rmux.conf`
2. `~/.rmux.conf`
3. `$XDG_CONFIG_HOME/rmux/rmux.conf`
4. `~/.config/rmux/rmux.conf`

On Windows, RMUX reads `.rmux.conf` as well, from the following locations:

1. `%XDG_CONFIG_HOME%\rmux\rmux.conf`
2. `%USERPROFILE%\.rmux.conf`
3. `%APPDATA%\rmux\rmux.conf`
4. `%RMUX_CONFIG_FILE%`

## Verification

The workspace is designed to be checked from source with locked dependencies:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked --no-fail-fast
```

Additional local checks:

```sh
scripts/cfg-check.sh
scripts/unsafe-check.sh
scripts/no-network-in-runtime.sh
scripts/check-platform-neutrality.sh
scripts/ratatui-rmux-budget.sh
scripts/verify-package.sh
```

Release artifact checks are driven by:

```sh
scripts/release-local.sh
scripts/package-unix.sh
```

`#![forbid(unsafe_code)]` is used in the upper-level crates. OS and terminal boundary code is isolated in the lower-level runtime crates.

## License

RMUX is dual-licensed under either:

- [MIT License](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)

at your option.
