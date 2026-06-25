<picture><source media="(max-width: 1359px)" srcset="docs/sidebar/readme-mobile-spacer-v3.svg"><img align="left" alt="" src="docs/sidebar/readme-desktop-left-spacer.svg"></picture>
<div>
<picture><source media="(max-width: 1359px)" srcset="docs/sidebar/readme-mobile-spacer-v3.svg"><source media="(prefers-color-scheme: dark)" srcset="docs/sidebar/readme-desktop-sidebar-dark-v3.svg"><img align="right" alt="README table of contents" usemap="#rmux-readme-sidebar-map" src="docs/sidebar/readme-desktop-sidebar-light-v3.svg"></picture>
<map name="rmux-readme-sidebar-map">
  <area shape="rect" coords="110,53,340,79" href="#what-is-rmux" alt="What is RMUX?">
  <area shape="rect" coords="110,79,340,104" href="#features" alt="Features">
  <area shape="rect" coords="110,104,340,130" href="#quick-start" alt="Quick Start">
  <area shape="rect" coords="110,130,340,155" href="#demos" alt="Demos">
  <area shape="rect" coords="110,230,340,256" href="#installation" alt="Installation">
  <area shape="rect" coords="110,256,340,281" href="#web-sharing" alt="Web Sharing">
  <area shape="rect" coords="110,281,340,307" href="#claude-teammate-mode" alt="Claude Agents">
  <area shape="rect" coords="110,307,340,332" href="https://pypi.org/project/librmux/" alt="Python SDK">
  <area shape="rect" coords="110,332,340,358" href="https://www.npmjs.com/package/@rmux/sdk" alt="TypeScript SDK">
  <area shape="rect" coords="110,433,340,458" href="#documentation" alt="Documentation">
  <area shape="rect" coords="110,458,340,484" href="docs/benchmarks.md" alt="Benchmarks">
  <area shape="rect" coords="110,484,340,509" href="https://rmux.io/docs/examples/" alt="Examples">
  <area shape="rect" coords="110,509,340,535" href="https://rmux.io/docs/faq/" alt="FAQ">
  <area shape="rect" coords="110,535,340,560" href="CONTRIBUTING.md" alt="Contributing">
</map>
</div>

<div align="center">

<p align="center">
  <a href="https://rmux.io/">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="docs/rmux-logo-dark.svg">
      <img src="docs/rmux-logo-light.svg" width="238" alt="RMUX logo">
    </picture>
  </a>
</p>

<p align="center">
  <a href="https://rmux.io/">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="docs/rmux-wordmark-dark.svg">
      <img src="docs/rmux-wordmark-light.svg" width="276" alt="RMUX">
    </picture>
  </a>
</p>

<p align="center"><small><strong>The Universal Multiplexer Engine.</strong></small></p>

<p align="center">
  <picture><source media="(prefers-color-scheme: dark)" srcset="docs/readme-hero-native-dark.svg"><img src="docs/readme-hero-native-light.svg" width="340" alt="Native on Windows, Linux, and macOS"></picture>
</p>

<p align="center">
  <picture><source media="(prefers-color-scheme: dark)" srcset="docs/readme-hero-rule-dark.svg"><img src="docs/readme-hero-rule-light.svg" width="340" alt=""></picture>
</p>

<p align="center"><small>English · <a href="docs/i18n/README.fr.md">Français</a> · <a href="docs/i18n/README.zh-CN.md">简体中文</a> · <a href="docs/i18n/README.ja.md">日本語</a></small></p>

<p align="center">
  <a href="#verification"><img src="https://img.shields.io/badge/unsafe-restricted-success.svg" alt="Unsafe policy"></a>
  <a href="https://github.com/Helvesec/rmux/actions/workflows/ci.yml?query=branch%3Amain"><img src="https://img.shields.io/github/actions/workflow/status/Helvesec/rmux/ci.yml?branch=main&amp;event=push&amp;label=CI" alt="CI"></a>
  <a href="https://www.bestpractices.dev/projects/13290"><img src="https://www.bestpractices.dev/projects/13290/badge" alt="OpenSSF Best Practices"></a>
  <a href="https://github.com/Helvesec/rmux/releases/tag/v0.7.0"><img src="https://img.shields.io/badge/rmux-0.7.0-informational.svg" alt="rmux 0.7.0"></a>
</p>

</div>

<br clear="all">

> [!NOTE]
> RMUX now has an E2E web multiplexing feature. [Learn more in the docs.](docs/web-share.md)
>
> RMUX now provides Python and TypeScript SDKs: [librmux](https://pypi.org/project/librmux/), [@rmux/sdk](https://www.npmjs.com/package/@rmux/sdk).
>
> If you have a feature request or want to report anything, please [file an issue](https://github.com/Helvesec/rmux/issues).

<p align="center">
  <a href="https://rmux.io/docs/web-share/">
    <img width="700" src="https://rmux.io/web-share-browser.gif" alt="RMUX web share">
  </a>
</p>

<a id="what-is-rmux"></a>

## 🧭 What is RMUX?

RMUX is an async, typed terminal multiplexer engine written in Rust. It implements 90+ `tmux` commands and runs natively on Linux, macOS, and Windows with no WSL required.

Use it as a standalone CLI, embed it in Rust terminal apps, or drive it through typed SDKs for Rust, Python, and TypeScript.

<a id="features"></a>

## ✨ Features

- **Universal engine:** typed SDKs for Rust, Python, and TypeScript.
- **Native cross-platform runtime:** Linux, macOS, and Windows backends.
- **tmux-compatible command surface:** 90+ commands covered by focused compatibility tests.
- **Engineered and optimized for speed:** see [Benchmarks](docs/benchmarks.md).
- **Web Share:** browser-shared sessions with hybrid post-quantum end-to-end encryption.
- **Ratatui widget:** render live RMUX panes inside Rust terminal applications.
- **Local daemon architecture:** shells, panes, windows, sessions, and scrollback stay on your machine.

<a id="quick-start"></a>

## 🚀 Quick Start

```sh
rmux list-commands
rmux new-session --help
rmux split-window --help
rmux web-share --help
rmux diagnose --human
```

Use `rmux -V` for the installed version.

<a id="demos"></a>
<a id="screenshots"></a>

## 🎬 Demos

<div align="center">

<table align="center">
  <tr>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-orchestration"><img src="https://rmux.io/demos/demo-orchestration.png" width="150" alt="Multi Agents Orchestration demo preview"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/demo-orchestration"><strong>Multi Agents Orchestration</strong></a></sub><br><sub>≃ 514 lines</sub></td>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-broadcast"><img src="https://rmux.io/demos/demo-broadcast.png" width="150" alt="Agent Broadcast Arena demo preview"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/broadcast-demo"><strong>Agent Broadcast Arena</strong></a></sub><br><sub>≃ 2,171 lines</sub></td>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-zellij"><img src="https://rmux.io/demos/demo-zellij.png" width="150" alt="Mini-Zellij demo preview"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/mini-zellij"><strong>Mini-Zellij</strong></a></sub><br><sub>≃ 944 lines</sub></td>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-playwright"><img src="https://rmux.io/demos/demo-playwright.png" width="150" alt="Terminal automation demo preview"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/terminal-playwright-demo"><strong>Terminal Automation</strong></a></sub><br><sub>≃ 1,495 lines</sub></td>
  </tr>
</table>

</div>

<a id="installation"></a>

## 📦 Installation

| Platform / manager | Command |
| :--- | :--- |
| <picture><source media="(prefers-color-scheme: dark)" srcset="docs/install/apple.svg"><img src="docs/install/apple-light.svg" width="28" alt="macOS"></picture> / Homebrew | `brew install rmux` |
| <picture><source media="(prefers-color-scheme: dark)" srcset="docs/install/windows.svg"><img src="docs/install/windows-light.svg" width="28" alt="Windows"></picture> / installer | `irm https://rmux.io/install.ps1 \| iex` |
| <picture><source media="(prefers-color-scheme: dark)" srcset="docs/install/windows.svg"><img src="docs/install/windows-light.svg" width="28" alt="Windows"></picture> / WinGet | `winget install rmux` |
| <picture><source media="(prefers-color-scheme: dark)" srcset="docs/install/windows.svg"><img src="docs/install/windows-light.svg" width="28" alt="Windows"></picture> / Scoop | `scoop bucket add rmux https://github.com/Helvesec/scoop-rmux && scoop install rmux` |
| <picture><source media="(prefers-color-scheme: dark)" srcset="docs/install/windows.svg"><img src="docs/install/windows-light.svg" width="28" alt="Windows"></picture> / Chocolatey | `choco install rmux` |
| <picture><source media="(prefers-color-scheme: dark)" srcset="docs/install/linux.svg"><img src="docs/install/linux-light.svg" width="28" alt="Linux"></picture> / APT | See the [APT setup guide](https://rmux.io/docs/get-started/) |
| <picture><source media="(prefers-color-scheme: dark)" srcset="docs/install/linux.svg"><img src="docs/install/linux-light.svg" width="28" alt="Linux"></picture> / DNF | See the [DNF setup guide](https://rmux.io/docs/get-started/) |
| <picture><source media="(prefers-color-scheme: dark)" srcset="docs/install/linux.svg"><img src="docs/install/linux-light.svg" width="28" alt="Linux"></picture> <picture><source media="(prefers-color-scheme: dark)" srcset="docs/install/apple.svg"><img src="docs/install/apple-light.svg" width="28" alt="macOS"></picture> / Nix | `nix profile install github:Helvesec/rmux` |
| <picture><source media="(prefers-color-scheme: dark)" srcset="docs/install/rust.svg"><img src="docs/install/rust-light.svg" width="28" alt="Rust"></picture> / Cargo | `cargo install rmux --locked` |

Direct downloads (`.tar.gz`, `.deb`, `.rpm`, `.zip`) are available from the [v0.7.0 GitHub Release](https://github.com/helvesec/rmux/releases/tag/v0.7.0).

Package managers can lag while registries review new releases; direct downloads and the rmux.io installers track the current GitHub Release.

Release packages may use a tiny public CLI for hot detached commands and a
private full CLI helper for complex tmux-compatible command forms. Windows
packages ship `rmux.exe` as the tiny dispatcher and keep the full CLI under
`libexec/rmux/rmux.exe`. Set `RMUX_DISABLE_TINY_CLI=1` to force the full helper
while diagnosing CLI compatibility issues.

<a id="claude-teammate-mode"></a>

## 🤝 Claude Teammate Mode

Run Claude Code inside a local RMUX workspace with
[tmux teammate mode](https://code.claude.com/docs/en/agent-teams) enabled.

<p align="center">
  <img src="docs/teammate.jpg" alt="Claude teammate mode in RMUX" width="900">
</p>

```bash
rmux claude [args]
# e.g., rmux claude --dangerously-skip-permissions
```

RMUX opens an attached session and automatically passes `--teammate-mode tmux`
along with your `[args]` straight to Claude.

How it works under the hood: to route commands properly, RMUX prepends a
private `tmux` shim to Claude's `PATH`. This is strictly scoped to the Claude
process and will not conflict with your system `tmux` installation.

Note: Requires `claude` to be installed on your machine.

<a id="configuration"></a>

## ⚙️ Configuration

RMUX reads `.rmux.conf` from standard system and user locations.

On Linux and macOS:

```text
/etc/rmux.conf
~/.rmux.conf
$XDG_CONFIG_HOME/rmux/rmux.conf
~/.config/rmux/rmux.conf
```

On Windows:

```text
%XDG_CONFIG_HOME%\rmux\rmux.conf
%USERPROFILE%\.rmux.conf
%APPDATA%\rmux\rmux.conf
%RMUX_CONFIG_FILE%
```

If no RMUX config is found, RMUX can parse standard `tmux.conf` paths on a best-effort basis. Unsupported plugin lines are reported without aborting startup. Disable this fallback with `RMUX_DISABLE_TMUX_FALLBACK=1`.

<a id="web-sharing"></a>

## 🌐 Web Sharing

Share a pane or session in a browser while terminal execution remains local.

```sh
rmux web-share
rmux new-session -d -s work
rmux web-share -t work
rmux web-share --tunnel-provider localhost-run
```

Web Share uses hybrid post-quantum end-to-end encryption and can run over loopback, a tunnel provider, or your own ingress.

- [Repository Web Share overview](docs/web-share.md)
- [Web Share docs](https://rmux.io/docs/web-share/)
- [Security model](https://rmux.io/docs/web-share/#/security)
- [Tunnel providers](https://rmux.io/docs/web-share/#/tunnels)

<a id="scripting-api"></a>

## 🧰 Scripting & API

The SDKs connect to the local RMUX daemon and expose sessions, panes, streams, waits, and snapshots for automation.

```sh
cargo add rmux-sdk
pip install librmux
npm install @rmux/sdk
```

- Rust SDK: [`rmux-sdk`](https://crates.io/crates/rmux-sdk)
- Python SDK: [`librmux`](https://pypi.org/project/librmux/)
- TypeScript SDK: [`@rmux/sdk`](https://www.npmjs.com/package/@rmux/sdk)
- [API reference](https://rmux.io/docs/api/)
- [Examples](https://rmux.io/docs/examples/)
- [Repository SDK overview](docs/scripting-sdk.md)

<a id="documentation"></a>

## 📚 Documentation

The full documentation is available at [rmux.io/docs](https://rmux.io/docs/).

- [Installation guides](https://rmux.io/docs/get-started/)
- [CLI reference](https://rmux.io/docs/cli/)
- [Examples](https://rmux.io/docs/examples/)
- [API reference](https://rmux.io/docs/api/)
- [Human-friendly config](docs/human-friendly-config.md)
- [Web Share](https://rmux.io/docs/web-share/)

## 🏗️ Architecture

<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://rmux.io/rmux-architecture-dark.png?v=0.7.0-web-share">
  <source media="(prefers-color-scheme: light)" srcset="https://rmux.io/rmux-architecture-light.png?v=0.7.0-web-share">
  <img src="https://rmux.io/rmux-architecture-dark.png?v=0.7.0-web-share" alt="RMUX runtime architecture" width="800">
</picture>

</div>

RMUX keeps shells, sessions, windows, panes, and PTY processes inside the local daemon. Local clients attach via IPC. Web Share exposes only the selected pane or session through an end-to-end encrypted WebSocket.

## 🧪 Verification

The workspace is designed to be checked from source with locked dependencies:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked --no-fail-fast
scripts/unsafe-check.sh
```

`#![forbid(unsafe_code)]` is used in the upper-level crates. OS and terminal boundary code is isolated in lower-level runtime crates.

## ⚖️ License

RMUX is dual-licensed under either:

- [MIT License](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)

at your option.
