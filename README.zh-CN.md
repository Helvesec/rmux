<div align="center">

<a href="https://rmux.io">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://rmux.io/rmux-header-dark.svg">
    <img src="https://rmux.io/rmux-header.svg" alt="RMUX" width="500">
  </picture>
</a>


**面向智能体时代的通用 Rust 终端复用器：可分离、可脚本化、可检查，提供兼容 tmux 的 CLI、由守护进程支撑的 SDK，以及原生 [Ratatui](https://ratatui.rs) 集成。**

[English](README.md) · [Français](README.fr.md) · 简体中文 · [日本語](README.ja.md)

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![Release validation](https://github.com/Helvesec/rmux/actions/workflows/ci.yml/badge.svg)](https://github.com/Helvesec/rmux/actions/workflows/ci.yml)
[![rmux 0.6.0](https://img.shields.io/badge/rmux-0.6.0-informational.svg)](#install)
[![Platform: Linux | macOS | Windows](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey.svg)](#platform-support)
[![Unsafe policy](https://img.shields.io/badge/unsafe-restricted-success.svg)](#verification)

<br />
<a href="https://rmux.io">
  <img src="https://rmux.io/rmux-terminal-demo.gif" width="500" alt="RMUX 终端会话演示" />
</a>

</div>

> [!NOTE]
> RMUX 现在包含通过静态前端实现的混合后量子、端到端加密 Web 复用能力。可在[仓库 Web Share 文档](docs/web-share.md)中了解更多。
>
> 功能请求或问题反馈可提交到 [issues](https://github.com/Helvesec/rmux/issues)。

## RMUX

RMUX 是一个现代、异步、类型化的 Rust <strong>复用器</strong>，在 macOS、Linux 和 Windows 上原生提供 90 多条 tmux 命令，无需 WSL。

它提供公共 Rust SDK 和原生 Ratatui 集成。

它可以从 CLI 使用，把会话分享到浏览器，也可以从 Rust 驱动。

## 演示

一些简短、真实的例子，展示 RMUX 可以用来做什么。

<table>
  <tr>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-orchestration"><img src="https://rmux.io/demos/demo-orchestration.png" width="150" alt="多智能体编排演示预览"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/demo-orchestration"><strong>多智能体编排</strong></a></sub><br><sub>≃ 514 行</sub></td>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-broadcast"><img src="https://rmux.io/demos/demo-broadcast.png" width="150" alt="Agent Broadcast Arena 演示预览"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/broadcast-demo"><strong>Agent Broadcast Arena</strong></a></sub><br><sub>≃ 2,171 行</sub></td>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-zellij"><img src="https://rmux.io/demos/demo-zellij.png" width="150" alt="Mini-Zellij 演示预览"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/mini-zellij"><strong>Mini-Zellij</strong></a></sub><br><sub>≃ 944 行</sub></td>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-mirroring"><img src="https://rmux.io/demos/demo-mirroring.png" width="150" alt="终端浏览器镜像演示预览"></a><br><sub><a href="https://rmux.io/#demo-mirroring"><strong>终端 &lt;&gt; 浏览器镜像</strong></a></sub><br><sub>≃ 649 行</sub></td>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-playwright"><img src="https://rmux.io/demos/demo-playwright.png" width="150" alt="Playwright 测试演示预览"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/terminal-playwright-demo"><strong>Playwright 测试</strong></a></sub><br><sub>≃ 1,495 行</sub></td>
  </tr>
</table>

## Web Multiplex (Web Share)

<p align="center">
<a href="https://rmux.io/docs/web-share/">
  <img src="https://rmux.io/web-share-browser.gif" width="500" alt="RMUX Web Share" />
</a>
</p>

RMUX 可以在浏览器中分享 pane 或 session，创建 pane，调整 split 大小，并让终端执行保持在本地。

```sh
# 在 loopback 上启动本地 Web Share
rmux web-share

# 分享一个命名 session
rmux new-session -d -s work
rmux web-share -t work

# 分享到 localhost 之外
rmux web-share --tunnel-provider localhost-run
```

可以使用 tunnel provider，接入自己的 ingress，或把静态前端部署在自己的域名上。

有用入口：

- [仓库 Web Share 概览](docs/web-share.md)
- [Web Share 文档](https://rmux.io/docs/web-share/)
- [安全模型](https://rmux.io/docs/web-share/#/security)
- [Tunnel providers](https://rmux.io/docs/web-share/#/tunnels)

<a id="install"></a>

## 安装

macOS 和 Linux 预构建二进制：

```sh
curl -fsSL https://rmux.io/install.sh | sh
```

macOS Homebrew：

```sh
brew install helvesec/rmux/rmux
```

Linux 软件包：

```sh
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://packages.rmux.io/debian/rmux.asc | sudo tee /etc/apt/keyrings/rmux.asc >/dev/null
echo "deb [signed-by=/etc/apt/keyrings/rmux.asc] https://packages.rmux.io/debian stable main" | sudo tee /etc/apt/sources.list.d/rmux.list >/dev/null
sudo apt update
sudo apt install rmux
```

Windows PowerShell 预构建二进制：

```powershell
irm https://rmux.io/install.ps1 | iex
```

Windows Scoop：

```powershell
scoop bucket add rmux https://github.com/Helvesec/scoop-rmux
scoop install rmux
```

直接下载和 SHA256 校验和可在 [v0.6.0 GitHub Release](https://github.com/helvesec/rmux/releases/tag/v0.6.0) 找到。

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

## 文档

完整 RMUX 文档可在 [rmux.io/docs](https://rmux.io/docs/) 查看。

其中包括[安装指南](https://rmux.io/docs/get-started/)、[CLI 参考](https://rmux.io/docs/cli/)、[SDK 示例](https://rmux.io/docs/examples/)、[终端自动化示例](https://rmux.io/docs/examples/#/terminal-playwright)，以及 [API 文档](https://rmux.io/docs/api/)。

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

## Ratatui 组件

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
  <source media="(prefers-color-scheme: dark)" srcset="https://rmux.io/rmux-architecture-dark.png?v=0.6.0-web-share">
  <source media="(prefers-color-scheme: light)" srcset="https://rmux.io/rmux-architecture-light.png?v=0.6.0-web-share">
  <img src="https://rmux.io/rmux-architecture-dark.png?v=0.6.0-web-share" alt="RMUX 运行时架构" width="800">
</picture>

</div>

`rmux` 把 shell、session、window、pane 和 PTY process 保留在本地 daemon 中。本地 client 使用 IPC。Web Share 是显式的浏览器访问：daemon 通过端到端加密的 WebSocket 暴露选中的 pane 或 session，而执行仍留在你的机器上。

## 工作区

| Crate | 角色 | 发布状态 |
| :--- | :--- | :--- |
| `rmux-types` | 共享的跨平台低层值类型 | 公开 |
| `rmux-proto` | 分离式 IPC DTO、帧格式、适合 wire 传输的错误 | 公开 |
| `rmux-os` | 小型 OS 边界辅助工具 | 公开 |
| `rmux-ipc` | 本地 IPC endpoint 和传输 | 公开 |
| `rmux-sdk` | 由守护进程支撑的 Rust SDK | 公开 |
| `ratatui-rmux` | Ratatui 集成组件 | 公开 |
| `rmux-pty` | PTY 分配、resize、子进程控制 | 支持 crate |
| `rmux-core` | session、pane、layout、format、hook、buffer | 支持 crate |
| `rmux-server` | Tokio 守护进程和请求分发 | 支持 crate |
| `rmux-client` | 本地 IPC client 和 attach plumbing | 支持 crate |
| `rmux` | CLI 和隐藏的守护进程入口 | 公开二进制 |
| `rmux-render-core` | 共享 snapshot 渲染核心 | workspace 内部 |

<a id="platform-support"></a>

## 平台支持

| 平台 | PTY 后端 | IPC 后端 | 默认 endpoint |
| :--- | :--- | :--- | :--- |
| Linux | Unix PTY | Unix socket | `/tmp/rmux-{uid}/default` |
| macOS | Unix PTY | Unix socket | `/tmp/rmux-{uid}/default` |
| Windows | ConPTY | 命名管道 | 每用户命名管道 |

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

### `tmux.conf` 迁移回退

当 RMUX 使用默认配置搜索启动，并且没有加载任何 RMUX 配置文件时，它可以读取经过过滤的
`tmux.conf` 作为迁移回退。通过 `-f` 显式指定配置文件时，不会使用这个回退。

回退查找位置：

- Linux 和 macOS：`/etc/tmux.conf`、`~/.tmux.conf`、`$XDG_CONFIG_HOME/tmux/tmux.conf`、`~/.config/tmux/tmux.conf`
- Windows：`%XDG_CONFIG_HOME%\tmux\tmux.conf`、`%USERPROFILE%\.tmux.conf`、`%APPDATA%\tmux\tmux.conf`

导入范围刻意保持很窄：RMUX 只保留已支持的静态配置项和取消按键绑定，
并跳过 tmux 按键绑定、环境或终端能力修改、插件用户配置项、hooks、
shell 命令、命令块、条件块、`#(cmd)` 这样的格式任务、
递归 `source-file` 条目，以及不支持的配置项，而不是执行它们。
设置 `RMUX_DISABLE_TMUX_FALLBACK=1` 可以完全禁用该回退。
回退文件按尽力原则读取；非普通文件和超过 1 MiB 的文件会被忽略。

### 终端兼容性说明

RMUX 可以配合会查询终端能力的 shell 使用，包括 fish。它会响应终端属性查询，
并处理 Escape 键的时序，因此 fish prompt 和按键序列可以在 RMUX pane 中正常工作。

Kitty 图形透传可用于支持 Kitty 图形协议的外层终端，包括 Kitty、Ghostty 和 WezTerm。
该功能需要显式开启：

```tmux
set -g allow-passthrough on
```

如果你的终端支持 Kitty 图形，但没有被自动检测到，可以添加终端能力覆盖：

```tmux
set -as terminal-features 'xterm-kitty:kitty-graphics'
```

在 Windows 上，如果系统支持，RMUX 会启用现代 ConPTY 透传模式。排查问题时可以设置
`RMUX_CONPTY_NO_PASSTHROUGH=1` 来禁用该后端模式。

<a id="verification"></a>

## 验证

该工作区设计为可从源码使用锁定依赖进行检查：

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

发布产物检查由以下脚本驱动：

```sh
scripts/release-local.sh
scripts/package-unix.sh
```

上层 crate 使用 `#![forbid(unsafe_code)]`。OS 和终端边界代码被隔离在较低层运行时 crate 中。

## 许可证

RMUX 采用双许可证，可任选其一：

- [MIT License](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)
