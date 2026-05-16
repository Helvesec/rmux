<div align="center">

<a href="https://rmux.io">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://rmux.io/rmux-header-dark.svg">
    <img src="https://rmux.io/rmux-header.svg" alt="RMUX" width="500">
  </picture>
</a>


**エージェント時代のための汎用 Rust ターミナルマルチプレクサ。detach 可能、スクリプト可能、検査可能で、tmux 互換 CLI、daemon-backed SDK、ネイティブ [Ratatui](https://ratatui.rs) 統合を備えています。**

[English](README.md) · [Français](README.fr.md) · [简体中文](README.zh-CN.md) · 日本語

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![Release validation](https://github.com/Helvesec/rmux/actions/workflows/ci.yml/badge.svg)](https://github.com/Helvesec/rmux/actions/workflows/ci.yml)
[![rmux 0.1.1](https://img.shields.io/badge/rmux-0.1.1-informational.svg)](#install)
[![Platform: Linux | macOS | Windows](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey.svg)](#platform-support)
[![Unsafe policy](https://img.shields.io/badge/unsafe-restricted-success.svg)](#verification)

<br />
<a href="https://rmux.io">
  <img src="https://rmux.io/rmux-terminal-demo.gif" width="500" alt="RMUX terminal session demo" />
</a>

</div>

> [!IMPORTANT]
> **2026年5月16日**に公開。tmux 互換の 90 コマンドはすべて実装済みですが、まだ新しい public preview のため bug が残る可能性があります。問題は [issues](https://github.com/helvesec/rmux/issues) へ報告できます。

## RMUX が提供するもの

- **tmux-style CLI**: sessions、windows、panes、buffers、hooks、formats、copy mode、control mode、一般的なターミナル workflow に対応。
- **Rust SDK**: sessions の作成、panes の split、型付き input の送信、snapshots の読み取り、pane output の購読、text/bytes の待機、clean shutdown に対応。
- **ratatui widget**: pane snapshots を `ratatui::buffer::Buffer` に描画。draw path で async work は不要。
- **ネイティブなローカル runtime support**: Linux/macOS は Unix PTY と Unix sockets、Windows は ConPTY と named pipes。WSL は不要。
- **小さな公開 crate set**: 内部実装 crates は public package surface の外に保持。

<a id="install"></a>

## インストール

macOS と Linux のビルド済みバイナリ：

```sh
curl -fsSL https://rmux.io/install.sh | sh
```

Windows PowerShell のビルド済みバイナリ：

```powershell
irm https://rmux.io/install.ps1 | iex
```

直接ダウンロードと SHA256 チェックサムは [v0.1.1 GitHub Release](https://github.com/helvesec/rmux/releases/tag/v0.1.1) で確認できます。

Cargo で crates.io から：

```sh
cargo install rmux --locked
```

ローカル checkout から：

```sh
cargo install --path . --locked
```

Rust アプリケーション向け：

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

ローカルのコマンドヘルプ：

```sh
rmux list-commands
rmux new-session --help
rmux split-window --help
```

## SDK Quickstart

```toml
[dependencies]
rmux-sdk = "0.1"
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

## アーキテクチャ

<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://rmux.io/rmux-architecture-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="https://rmux.io/rmux-architecture-light.png">
  <img src="https://rmux.io/rmux-architecture-dark.png" alt="RMUX runtime architecture" width="800">
</picture>

</div>

3つの public surface — `rmux` CLI、`rmux-sdk` Rust crate、`ratatui-rmux` widget — は、daemon と通信するための単一のローカル protocol を共有します。1つの surface でできることは、他の surface でもできます。

## Workspace

| Crate | 役割 | 公開 |
| :--- | :--- | :--- |
| `rmux-types` | platform-neutral な共有低レベル値型 | public |
| `rmux-proto` | Detached IPC DTOs、framing、wire-safe errors | public |
| `rmux-os` | 小さな OS boundary helpers | public |
| `rmux-ipc` | ローカル IPC endpoints と transports | public |
| `rmux-sdk` | Daemon-backed Rust SDK | public |
| `ratatui-rmux` | Ratatui integration widget | public |
| `rmux-pty` | PTY allocation、resize、child process control | support crate |
| `rmux-core` | Sessions、panes、layouts、formats、hooks、buffers | support crate |
| `rmux-server` | Tokio daemon と request dispatch | support crate |
| `rmux-client` | ローカル IPC client と attach plumbing | support crate |
| `rmux` | CLI と隠し daemon entrypoint | public binary |
| `rmux-render-core` | 共有 snapshot rendering core | workspace-internal |

<a id="platform-support"></a>

## プラットフォームサポート

| Platform | PTY backend | IPC backend | Default endpoint |
| :--- | :--- | :--- | :--- |
| Linux | Unix PTY | Unix socket | `/tmp/rmux-{uid}/default` |
| macOS | Unix PTY | Unix socket | `/tmp/rmux-{uid}/default` |
| Windows | ConPTY | Named pipe | per-user named pipe |

## 設定

Linux と macOS では、RMUX は標準の system/user locations から `.rmux.conf` を読み込みます：

1. `/etc/rmux.conf`
2. `~/.rmux.conf`
3. `$XDG_CONFIG_HOME/rmux/rmux.conf`
4. `~/.config/rmux/rmux.conf`

Windows でも RMUX は `.rmux.conf` を読み込みます。探索場所は次の通りです：

1. `%XDG_CONFIG_HOME%\rmux\rmux.conf`
2. `%USERPROFILE%\.rmux.conf`
3. `%APPDATA%\rmux\rmux.conf`
4. `%RMUX_CONFIG_FILE%`

<a id="verification"></a>

## 検証

この workspace は、locked dependencies を使って source から検証できるように設計されています：

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked --no-fail-fast
```

追加のローカル checks：

```sh
scripts/cfg-check.sh
scripts/unsafe-check.sh
scripts/no-network-in-runtime.sh
scripts/check-platform-neutrality.sh
scripts/ratatui-rmux-budget.sh
scripts/verify-package.sh
```

Release artifact checks は次の scripts で実行されます：

```sh
scripts/release-local.sh
scripts/package-unix.sh
```

上位 crates では `#![forbid(unsafe_code)]` を使用しています。OS と terminal boundary code は低レイヤー runtime crates に隔離されています。

## ライセンス

RMUX は次のいずれかのライセンスで利用できます：

- [MIT License](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)
