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
[![CI](https://img.shields.io/github/actions/workflow/status/Helvesec/rmux/ci.yml?branch=main&event=push&label=CI)](https://github.com/Helvesec/rmux/actions/workflows/ci.yml?query=branch%3Amain)
[![rmux 0.6.1](https://img.shields.io/badge/rmux-0.6.1-informational.svg)](#install)
[![Platform: Linux | macOS | Windows](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey.svg)](#platform-support)
[![Unsafe policy](https://img.shields.io/badge/unsafe-restricted-success.svg)](#verification)

<br />
<a href="https://rmux.io">
  <img src="https://rmux.io/rmux-terminal-demo.gif" width="500" alt="RMUX terminal session demo" />
</a>

</div>

> [!NOTE]
> RMUX now includes hybrid post-quantum end-to-end encrypted web multiplexing through a static frontend. [Learn more in the repository Web Share docs](docs/web-share.md).
>
> RMUX now provides an official Python SDK: [librmux](https://github.com/Helvesec/rmux-python/).
>
> If you have a feature request or want to report anything, please [file an issue](https://github.com/Helvesec/rmux/issues).

## RMUX
A modern, async typed Rust <strong>multiplexer</strong> with 90+ tmux commands natively across macOS, Linux, and Windows, with no WSL needed.

It ships with a public Rust SDK and native Ratatui integration.

Use it from the CLI, share sessions in a browser, or drive it from Rust.

## Demos

Short examples of what RMUX can be used for.

<table>
  <tr>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-orchestration"><img src="https://rmux.io/demos/demo-orchestration.png" width="150" alt="Multi Agents Orchestration demo preview"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/demo-orchestration"><strong>Multi Agents Orchestration</strong></a></sub><br><sub>≃ 514 lines</sub></td>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-broadcast"><img src="https://rmux.io/demos/demo-broadcast.png" width="150" alt="Agent Broadcast Arena demo preview"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/broadcast-demo"><strong>Agent Broadcast Arena</strong></a></sub><br><sub>≃ 2,171 lines</sub></td>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-zellij"><img src="https://rmux.io/demos/demo-zellij.png" width="150" alt="Mini-Zellij demo preview"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/mini-zellij"><strong>Mini-Zellij</strong></a></sub><br><sub>≃ 944 lines</sub></td>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-playwright"><img src="https://rmux.io/demos/demo-playwright.png" width="150" alt="Terminal automation demo preview"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/terminal-playwright-demo"><strong>Terminal Automation</strong></a></sub><br><sub>≃ 1,495 lines</sub></td>
  </tr>
</table>



## Web Multiplex (Web Share)

<p align="center">
<a href="https://rmux.io/docs/web-share/">
  <img src="https://rmux.io/web-share-browser.gif" width="500" alt="RMUX web share" />
</a>
</p>

RMUX can share a pane or session in a browser, create panes, resize splits, and keep terminal execution local.


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

<a id="install-linux"></a>
<details>
<summary><strong>Linux install</strong></summary>

#### Portable installer

```sh
curl -fsSL https://rmux.io/install.sh | sh
```

#### APT

```sh
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://packages.rmux.io/debian/rmux.asc | sudo tee /etc/apt/keyrings/rmux.asc >/dev/null
echo "deb [signed-by=/etc/apt/keyrings/rmux.asc] https://packages.rmux.io/debian stable main" | sudo tee /etc/apt/sources.list.d/rmux.list >/dev/null
sudo apt update
sudo apt install rmux
```

#### DNF

```sh
sudo curl -fsSL https://packages.rmux.io/rpm/rmux.repo -o /etc/yum.repos.d/rmux.repo
sudo dnf install rmux
```

Direct downloads are available from the [v0.6.1 GitHub Release](https://github.com/helvesec/rmux/releases/tag/v0.6.1):

- `rmux-0.6.1-linux-x86_64.tar.gz`
- `rmux_0.6.1_amd64.deb`
- `rmux-0.6.1-1.x86_64.rpm`

</details>

<a id="install-macos"></a>
<details>
<summary><strong>macOS install</strong></summary>

#### Portable installer

```sh
curl -fsSL https://rmux.io/install.sh | sh
```

#### Homebrew

```sh
brew install rmux
```

Direct downloads are available from the [v0.6.1 GitHub Release](https://github.com/helvesec/rmux/releases/tag/v0.6.1):

- `rmux-0.6.1-macos-aarch64.tar.gz`
- `rmux-0.6.1-macos-x86_64.tar.gz`

</details>

<a id="install-windows"></a>
<details>
<summary><strong>Windows install</strong></summary>

#### PowerShell installer

```powershell
irm https://rmux.io/install.ps1 | iex
```

#### Scoop

```powershell
scoop bucket add rmux https://github.com/Helvesec/scoop-rmux
scoop install rmux
```

#### WinGet

```powershell
winget install rmux
```

#### Chocolatey

```powershell
choco install rmux
```

Direct downloads are available from the [v0.6.1 GitHub Release](https://github.com/helvesec/rmux/releases/tag/v0.6.1):

- `rmux-0.6.1-windows-x86_64.zip`

</details>

<a id="install-cargo"></a>
<details>
<summary><strong>Rust / Cargo install</strong></summary>

This path works on Linux, macOS, and Windows.

#### Install Rust

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

#### Install RMUX

```sh
cargo install rmux --locked
```

#### Rust applications

```sh
cargo add rmux-sdk
cargo add ratatui-rmux
```

</details>

SHA256 checksums are published with every GitHub Release. APT, DNF, Homebrew, Scoop, Chocolatey, and WinGet metadata are generated from the same release assets.

## Documentation

The full RMUX documentation is available at [rmux.io/docs](https://rmux.io/docs/).

It includes:

- [Installation guides](https://rmux.io/docs/get-started/)
- [CLI reference](https://rmux.io/docs/cli/)
- [Examples](https://rmux.io/docs/examples/)
- [API reference](https://rmux.io/docs/api/)
- [Repository SDK overview](docs/scripting-sdk.md)
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
rmux-sdk = "0.6"
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
  <source media="(prefers-color-scheme: dark)" srcset="https://rmux.io/rmux-architecture-dark.png?v=0.6.1-web-share">
  <source media="(prefers-color-scheme: light)" srcset="https://rmux.io/rmux-architecture-light.png?v=0.6.1-web-share">
  <img src="https://rmux.io/rmux-architecture-dark.png?v=0.6.1-web-share" alt="RMUX runtime architecture" width="800">
</picture>

</div>

`rmux` keeps shells, sessions, windows, panes, and PTY processes inside the local daemon. Local clients use IPC. Web Share is explicit browser access: the daemon exposes a selected pane or session through an end-to-end encrypted WebSocket, while execution stays on your machine.

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

### `tmux.conf` compatibility

When RMUX starts with the default config search and no RMUX config file is loaded, it also checks standard `tmux.conf` locations. Explicit `-f` config files do not trigger this fallback.

Fallback files use the tmux-compatible source parser and load best-effort. Supported commands are applied; unsupported plugin lines are reported without aborting startup. Set `RMUX_DISABLE_TMUX_FALLBACK=1` to disable autoload.

On Unix, RMUX also provides a private per-socket `tmux` shim in command environments so common plugin scripts route back to RMUX. Set `RMUX_DISABLE_TMUX_SHIM=1` to disable it.

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
