<div align="center">

<a href="https://rmux.io">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://rmux.io/rmux-header-dark.svg">
    <img src="https://rmux.io/rmux-header.svg" alt="RMUX" width="500">
  </picture>
</a>


**面向智能体时代的通用 Rust 终端复用器：可分离、可脚本化、可检查，提供兼容 tmux 的 CLI、daemon-backed SDK，以及原生 [Ratatui](https://ratatui.rs) 集成。**

[English](README.md) · [Français](README.fr.md) · 简体中文 · [日本語](README.ja.md)

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![Release validation](https://github.com/Helvesec/rmux/actions/workflows/ci.yml/badge.svg)](https://github.com/Helvesec/rmux/actions/workflows/ci.yml)
[![rmux 0.2.0](https://img.shields.io/badge/rmux-0.2.0-informational.svg)](#install)
[![Platform: Linux | macOS | Windows](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey.svg)](#platform-support)
[![Unsafe policy](https://img.shields.io/badge/unsafe-restricted-success.svg)](#verification)

<br />
<a href="https://rmux.io">
  <img src="https://rmux.io/rmux-terminal-demo.gif" width="500" alt="RMUX 终端会话演示" />
</a>

</div>

> [!IMPORTANT]
> 发布日期：**2026 年 5 月 16 日**。90 条 tmux-compatible commands 已全部实现，但这仍是新的公开预览版本，可能存在 bug。遇到问题时可在 [issues](https://github.com/helvesec/rmux/issues) 中反馈。

## RMUX 提供什么

- **tmux 风格 CLI**，覆盖 sessions、windows、panes、buffers、hooks、formats、copy mode、control mode 和常见终端工作流。
- **Rust SDK**，用于创建 sessions、拆分 panes、发送类型化输入、读取 snapshots、订阅 pane output、等待文本或字节，并进行干净关闭。
- **ratatui widget**，可把 pane snapshots 渲染到 `ratatui::buffer::Buffer`，绘制路径中不需要 async work。
- **原生本地 runtime 支持**：Linux/macOS 使用 Unix PTY 和 Unix sockets；Windows 使用 ConPTY 和 named pipes，无需 WSL。
- **小而清晰的已发布 crate 集合**，内部实现 crates 不暴露到公共 package surface。

<a id="install"></a>

## 安装

macOS 和 Linux 预构建二进制：

```sh
curl -fsSL https://rmux.io/install.sh | sh
```

Windows PowerShell 预构建二进制：

```powershell
irm https://rmux.io/install.ps1 | iex
```

直接下载和 SHA256 校验和可在 [v0.2.0 GitHub Release](https://github.com/helvesec/rmux/releases/tag/v0.2.0) 找到。

使用 Cargo 从 crates.io 安装：

```sh
cargo install rmux --locked
```

从本地 checkout 安装：

```sh
cargo install --path . --locked
```

Rust 应用：

```sh
cargo add rmux-sdk
cargo add ratatui-rmux
```

## CLI 快速开始

```sh
rmux new-session -d -s work
rmux split-window -h -t work
rmux send-keys -t work 'echo "hello from rmux"' Enter
rmux attach-session -t work
```

查看本地命令帮助：

```sh
rmux list-commands
rmux new-session --help
rmux split-window --help
```

## SDK 快速开始

```toml
[dependencies]
rmux-sdk = "0.2"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

```rust
use std::time::Duration;

use rmux_sdk::{
    EnsureSession, EnsureSessionPolicy, ProcessSpec, Rmux, SessionName,
    TerminalSizeSpec,
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
                .size(TerminalSizeSpec::new(120, 32))
                .process(ProcessSpec {
                    command: None,
                    environment: None,
                }),
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

## 架构

<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://rmux.io/rmux-architecture-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="https://rmux.io/rmux-architecture-light.png">
  <img src="https://rmux.io/rmux-architecture-dark.png" alt="RMUX runtime 架构" width="800">
</picture>

</div>

三个公共入口 — `rmux` CLI、`rmux-sdk` Rust crate 和 `ratatui-rmux` widget — 共享同一个本地协议与 daemon 通信。一个入口能做的事，其他入口也能做。

## Workspace

| Crate | 角色 | 发布状态 |
| :--- | :--- | :--- |
| `rmux-types` | 共享的 platform-neutral 值类型 | public |
| `rmux-proto` | Detached IPC DTOs、framing、wire-safe errors | public |
| `rmux-os` | 小型 OS 边界 helpers | public |
| `rmux-ipc` | 本地 IPC endpoints 和 transports | public |
| `rmux-sdk` | Daemon-backed Rust SDK | public |
| `ratatui-rmux` | Ratatui integration widget | public |
| `rmux-pty` | PTY allocation、resize、child process control | support crate |
| `rmux-core` | Sessions、panes、layouts、formats、hooks、buffers | support crate |
| `rmux-server` | Tokio daemon 和 request dispatch | support crate |
| `rmux-client` | 本地 IPC client 和 attach plumbing | support crate |
| `rmux` | CLI 和隐藏 daemon entrypoint | public binary |
| `rmux-render-core` | 共享 snapshot rendering core | workspace-internal |

<a id="platform-support"></a>

## 平台支持

| 平台 | PTY backend | IPC backend | 默认 endpoint |
| :--- | :--- | :--- | :--- |
| Linux | Unix PTY | Unix socket | `/tmp/rmux-{uid}/default` |
| macOS | Unix PTY | Unix socket | `/tmp/rmux-{uid}/default` |
| Windows | ConPTY | Named pipe | per-user named pipe |

## 配置

在 Linux 和 macOS 上，RMUX 会从标准系统和用户位置读取 `.rmux.conf`：

1. `/etc/rmux.conf`
2. `~/.rmux.conf`
3. `$XDG_CONFIG_HOME/rmux/rmux.conf`
4. `~/.config/rmux/rmux.conf`

在 Windows 上，RMUX 也会读取 `.rmux.conf`，位置如下：

1. `%XDG_CONFIG_HOME%\rmux\rmux.conf`
2. `%USERPROFILE%\.rmux.conf`
3. `%APPDATA%\rmux\rmux.conf`
4. `%RMUX_CONFIG_FILE%`

<a id="verification"></a>

## 验证

该 workspace 设计为可从源码使用 locked dependencies 进行检查：

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked --no-fail-fast
```

额外的本地检查：

```sh
scripts/cfg-check.sh
scripts/unsafe-check.sh
scripts/no-network-in-runtime.sh
scripts/check-platform-neutrality.sh
scripts/ratatui-rmux-budget.sh
scripts/verify-package.sh
```

Release artifact 检查由以下脚本驱动：

```sh
scripts/release-local.sh
scripts/package-unix.sh
```

上层 crates 使用 `#![forbid(unsafe_code)]`。OS 和 terminal 边界代码被隔离在较低层 runtime crates 中。

## 许可证

RMUX 采用双许可证，可任选其一：

- [MIT License](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)
