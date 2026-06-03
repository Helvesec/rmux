<div align="center">

<a href="https://rmux.io">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://rmux.io/rmux-header-dark.svg">
    <img src="https://rmux.io/rmux-header.svg" alt="RMUX" width="500">
  </picture>
</a>

**A modern Rust terminal multiplexer for local shells, long-running agents, typed automation, and browser-shared terminal sessions.**

English · [Français](README.fr.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja.md)

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![Release validation](https://github.com/Helvesec/rmux/actions/workflows/ci.yml/badge.svg)](https://github.com/Helvesec/rmux/actions/workflows/ci.yml)
[![rmux 0.4.0](https://img.shields.io/badge/rmux-0.4.0-informational.svg)](#install)
[![Platform: Linux | macOS | Windows](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey.svg)](#platform-support)
[![Unsafe policy](https://img.shields.io/badge/unsafe-restricted-success.svg)](#verification)

<br />
<a href="https://rmux.io">
  <img src="https://rmux.io/rmux-terminal-demo.gif" width="500" alt="RMUX terminal session demo" />
</a>

</div>

> [!IMPORTANT]
> Current release: **v0.4.0**, published on **3 June 2026**. This release brings **[Web Multiplex (Web Share)](#web-multiplex-web-share)**, RMUX's web multiplexing feature: share a pane or session in a browser with `rmux web-share`, keep the PTY local, and relay only end-to-end encrypted terminal frames.
>
> RMUX is still moving fast. If you hit a bug, a platform edge case, or have a feature request, please [file an issue](https://github.com/Helvesec/rmux/issues). Reports are welcome.

## Demos

Short, real examples of what RMUX can be used for.

<table>
  <tr>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-orchestration"><img src="https://rmux.io/demos/demo-orchestration.png" width="150" alt="Multi Agents Orchestration demo preview"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/demo-orchestration"><strong>Multi Agents Orchestration</strong></a></sub><br><sub>≃ 514 lines</sub></td>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-broadcast"><img src="https://rmux.io/demos/demo-broadcast.png" width="150" alt="Agent Broadcast Arena demo preview"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/broadcast-demo"><strong>Agent Broadcast Arena</strong></a></sub><br><sub>≃ 2,171 lines</sub></td>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-zellij"><img src="https://rmux.io/demos/demo-zellij.png" width="150" alt="Mini-Zellij demo preview"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/mini-zellij"><strong>Mini-Zellij</strong></a></sub><br><sub>≃ 944 lines</sub></td>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-playwright"><img src="https://rmux.io/demos/demo-playwright.png" width="150" alt="Terminal automation demo preview"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/terminal-playwright-demo"><strong>Terminal Automation</strong></a></sub><br><sub>≃ 1,495 lines</sub></td>
  </tr>
</table>

## RMUX

RMUX is a modern Rust multiplexer built on async principles with Tokio. It supports over 90 tmux commands natively across macOS, Linux, and Windows — no WSL required.

Designed for the agentic AI era, it features a public Rust SDK to build and script anything you can imagine. Use it as a normal multiplexer, a daemon-backed terminal runtime, a typed automation layer, or the terminal substrate for your own tools.

RMUX also enables web multiplexing: with one seamless command, `rmux web-share`, you can beam any pane or session directly to the web. Bring your own domain. Host the static frontend anywhere; terminal data stays end-to-end encrypted with a hybrid post-quantum key exchange, even through blind tunnels.

## Web Multiplex (Web Share)

```sh
# Start a local Web Share over loopback
rmux web-share

# Share a named session
rmux new-session -d -s work
rmux web-share -t work

# Share beyond localhost
rmux web-share --tunnel-provider localhost-run
```

Use a tunnel provider, bring your own ingress, or host the static frontend on your own domain.

Useful entry points:

- [Repository Web Share overview](docs/web-share.md)
- [Web Share docs](https://rmux.io/docs/web-share/)
- [Security model](https://rmux.io/docs/web-share/#/security)
- [Tunnel providers](https://rmux.io/docs/web-share/#/tunnels)

## Install

Choose your platform:

- [Linux](#linux)
- [macOS](#macos)
- [Windows](#windows)
- [Rust / Cargo](#rust--cargo)

### Linux

Portable installer:

```sh
curl -fsSL https://rmux.io/install.sh | sh
```

Direct downloads are available from the [v0.4.0 GitHub Release](https://github.com/helvesec/rmux/releases/tag/v0.4.0):

- `rmux-0.4.0-linux-x86_64.tar.gz`
- `rmux_0.4.0_amd64.deb`
- `rmux-0.4.0-1.x86_64.rpm`

### macOS

Portable installer:

```sh
curl -fsSL https://rmux.io/install.sh | sh
```

Direct downloads are available from the [v0.4.0 GitHub Release](https://github.com/helvesec/rmux/releases/tag/v0.4.0):

- `rmux-0.4.0-macos-aarch64.tar.gz`
- `rmux-0.4.0-macos-x86_64.tar.gz`

### Windows

PowerShell installer:

```powershell
irm https://rmux.io/install.ps1 | iex
```

Direct downloads are available from the [v0.4.0 GitHub Release](https://github.com/helvesec/rmux/releases/tag/v0.4.0):

- `rmux-0.4.0-windows-x86_64.zip`

SHA256 checksums are published with every GitHub Release. APT, DNF, Homebrew, WinGet, Scoop, and Chocolatey metadata is prepared for each release and published separately once the corresponding repositories are live.

### Rust / Cargo

This path works on Linux, macOS, and Windows.

Install Rust if you do not already have it:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then install RMUX from crates.io:

```sh
cargo install rmux --locked
```

For Rust applications:

```sh
cargo add rmux-sdk
cargo add ratatui-rmux
```

## Documentation

The full RMUX documentation is available at [rmux.io/docs](https://rmux.io/docs/).

It includes:

- [Installation guides](https://rmux.io/docs/get-started/)
- [CLI reference](https://rmux.io/docs/cli/)
- [Examples](https://rmux.io/docs/examples/)
- [API reference](https://rmux.io/docs/api/)
- [Web Share](https://rmux.io/docs/web-share/)

For an ergonomic, human-oriented profile that keeps native terminal selection intuitive while adding easier split bindings and clipboard integration, see [docs/human-friendly-config.md](docs/human-friendly-config.md).

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
rmux web-share --help
```

Use `rmux -V` for the RMUX package version. For build and support details, use `rmux diagnose --human` or `rmux diagnose --json`.

## SDK Quickstart

```toml
[dependencies]
rmux-sdk = "0.4"
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

Three public surfaces — the `rmux` CLI, the `rmux-sdk` Rust crate, and the `ratatui-rmux` widget — share one local protocol to talk to the daemon. Anything one surface can do, the others can do too.

## Workspace

| Crate | Role | Publication |
| :--- | :--- | :--- |
| `rmux-types` | Shared platform-neutral value types | public |
| `rmux-proto` | Detached IPC DTOs, framing, wire-safe errors | public |
| `rmux-os` | Small OS boundary helpers | public |
| `rmux-ipc` | Local IPC endpoints and transports | public |
| `rmux-sdk` | Daemon-backed Rust SDK | public |
| `ratatui-rmux` | Ratatui integration widget | public |
| `rmux-web-crypto` | Web Share E2EE core and WASM crypto boundary | public |
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

On Windows, RMUX reads `.rmux.conf` from:

1. `%XDG_CONFIG_HOME%\rmux\rmux.conf`
2. `%USERPROFILE%\.rmux.conf`
3. `%APPDATA%\rmux\rmux.conf`
4. `%RMUX_CONFIG_FILE%`

### `tmux.conf` migration fallback

When RMUX starts with the default config search and no RMUX config file is loaded, it can import a filtered `tmux.conf` as a migration fallback. Explicit config loading with `-f` does not use this fallback.

Fallback paths:

- Linux and macOS: `/etc/tmux.conf`, `~/.tmux.conf`, `$XDG_CONFIG_HOME/tmux/tmux.conf`, `~/.config/tmux/tmux.conf`
- Windows: `%XDG_CONFIG_HOME%\tmux\tmux.conf`, `%USERPROFILE%\.tmux.conf`, `%APPDATA%\tmux\tmux.conf`

The import is intentionally narrow: RMUX keeps supported static options and key unbindings, but skips tmux key bindings, environment or terminal capability mutations, plugin user options and hooks, shell commands, command blocks, conditionals, format jobs such as `#(cmd)`, recursive `source-file` entries, and unsupported options instead of executing them. Set `RMUX_DISABLE_TMUX_FALLBACK=1` to disable it entirely.

## Terminal Compatibility Notes

RMUX works with shells that query terminal capabilities, including fish. It answers terminal device-attribute probes and handles Escape-key timing so fish prompts and key sequences behave normally inside RMUX panes.

Graphics passthrough is available for outer terminals that support Kitty graphics or SIXEL. RMUX detects Kitty graphics for Kitty, Ghostty, and WezTerm, and detects SIXEL for terminals such as foot, mintty, mlterm, and WezTerm. It is opt-in:

```tmux
set -g allow-passthrough on
```

The tmux value `all` is accepted for configuration compatibility. RMUX renders the attached pane, so `all` currently behaves like `on` rather than adding passthrough for unattached panes.

If your terminal supports either protocol but is not detected automatically, add a terminal feature override:

```tmux
set -as terminal-features 'xterm-kitty:kitty-graphics'
set -as terminal-features 'xterm*:sixel'
```

SIXEL passthrough is covered by the automated Unix PTY attach regression suite. On Windows, RMUX enables modern ConPTY passthrough when the OS supports it, but SIXEL display still depends on the outer terminal. Set `RMUX_CONPTY_NO_PASSTHROUGH=1` to disable that backend mode for troubleshooting.

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
scripts/package-debian.sh
scripts/verify-debian-package.sh
scripts/package-rpm.sh
scripts/verify-rpm-package.sh
scripts/package-windows.ps1
scripts/verify-package-windows.ps1
scripts/generate-apt-repository.sh
scripts/generate-rpm-repository.sh
scripts/generate-homebrew-formula.sh
scripts/generate-winget-manifest.sh
scripts/generate-scoop-manifest.sh
scripts/generate-chocolatey-package.sh
```

`#![forbid(unsafe_code)]` is used in the upper-level crates. OS and terminal boundary code is isolated in the lower-level runtime crates.

## License

RMUX is dual-licensed under either:

- [MIT License](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)

at your option.
