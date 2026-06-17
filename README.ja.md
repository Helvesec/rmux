<div align="center">

<a href="https://rmux.io">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://rmux.io/rmux-header-dark.svg">
    <img src="https://rmux.io/rmux-header.svg" alt="RMUX" width="500">
  </picture>
</a>


**エージェント時代のための汎用 Rust ターミナルマルチプレクサ。デタッチ可能、スクリプト可能、検査可能で、tmux 互換 CLI、デーモンベースの SDK、ネイティブ [Ratatui](https://ratatui.rs) 統合を備えています。**

[English](README.md) · [Français](README.fr.md) · [简体中文](README.zh-CN.md) · 日本語

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
> RMUX は静的フロントエンドを通じた、ハイブリッド・ポスト量子、エンドツーエンド暗号化の Web 多重化に対応しました。詳しくは[リポジトリの Web Share ドキュメント](docs/web-share.md)を参照してください。
>
> RMUX は公式 Python SDK [librmux](https://github.com/Helvesec/rmux-python/) も提供しています。
>
> 機能要望や問題報告は [issues](https://github.com/Helvesec/rmux/issues) へ送れます。

## RMUX

RMUX は、モダンで非同期、型付きの Rust <strong>マルチプレクサ</strong>です。macOS、Linux、Windows で 90 以上の tmux コマンドをネイティブに提供し、WSL は不要です。

公開 Rust SDK とネイティブ Ratatui 統合を提供します。

CLI から使えるほか、セッションをブラウザに共有したり、Rust から操作したりできます。

## デモ

RMUX を何に使えるかを示す短い実例です。

<table>
  <tr>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-orchestration"><img src="https://rmux.io/demos/demo-orchestration.png" width="150" alt="マルチエージェント編成デモのプレビュー"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/demo-orchestration"><strong>マルチエージェント編成</strong></a></sub><br><sub>約 514 行</sub></td>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-broadcast"><img src="https://rmux.io/demos/demo-broadcast.png" width="150" alt="Agent Broadcast Arena デモのプレビュー"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/broadcast-demo"><strong>Agent Broadcast Arena</strong></a></sub><br><sub>約 2,171 行</sub></td>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-zellij"><img src="https://rmux.io/demos/demo-zellij.png" width="150" alt="Mini-Zellij デモのプレビュー"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/mini-zellij"><strong>Mini-Zellij</strong></a></sub><br><sub>約 944 行</sub></td>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-mirroring"><img src="https://rmux.io/demos/demo-mirroring.png" width="150" alt="ターミナルとブラウザのミラーリングデモのプレビュー"></a><br><sub><a href="https://rmux.io/#demo-mirroring"><strong>ターミナル &lt;&gt; ブラウザミラーリング</strong></a></sub><br><sub>約 649 行</sub></td>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-playwright"><img src="https://rmux.io/demos/demo-playwright.png" width="150" alt="Playwright テストデモのプレビュー"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/terminal-playwright-demo"><strong>Playwright テスト</strong></a></sub><br><sub>約 1,495 行</sub></td>
  </tr>
</table>

## Web Multiplex (Web Share)

<p align="center">
<a href="https://rmux.io/docs/web-share/">
  <img src="https://rmux.io/web-share-browser.gif" width="500" alt="RMUX Web Share" />
</a>
</p>

RMUX は pane や session をブラウザに共有し、pane を作成し、split のサイズを変更し、ターミナル実行をローカルに残します。

```sh
# loopback 上でローカル Web Share を開始
rmux web-share

# 名前付き session を共有
rmux new-session -d -s work
rmux web-share -t work

# localhost の外へ共有
rmux web-share --tunnel-provider localhost-run
```

tunnel provider を使う、自分の ingress を持ち込む、静的 frontend を自分のドメインでホストする、いずれも可能です。

便利な入口：

- [リポジトリの Web Share 概要](docs/web-share.md)
- [Web Share ドキュメント](https://rmux.io/docs/web-share/)
- [セキュリティモデル](https://rmux.io/docs/web-share/#/security)
- [Tunnel providers](https://rmux.io/docs/web-share/#/tunnels)

<a id="install"></a>

## インストール

macOS と Linux のビルド済みバイナリ：

```sh
curl -fsSL https://rmux.io/install.sh | sh
```

macOS Homebrew：

```sh
brew install rmux
```

Linux パッケージ：

```sh
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://packages.rmux.io/debian/rmux.asc | sudo tee /etc/apt/keyrings/rmux.asc >/dev/null
echo "deb [signed-by=/etc/apt/keyrings/rmux.asc] https://packages.rmux.io/debian stable main" | sudo tee /etc/apt/sources.list.d/rmux.list >/dev/null
sudo apt update
sudo apt install rmux
```

Windows PowerShell のビルド済みバイナリ：

```powershell
irm https://rmux.io/install.ps1 | iex
```

Windows Scoop：

```powershell
scoop bucket add rmux https://github.com/Helvesec/scoop-rmux
scoop install rmux
```

Windows WinGet：

```powershell
winget install rmux
```

Windows Chocolatey：

```powershell
choco install rmux
```

直接ダウンロードと SHA256 チェックサムは [v0.6.1 GitHub Release](https://github.com/helvesec/rmux/releases/tag/v0.6.1) で確認できます。

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

## ドキュメント

RMUX の完全なドキュメントは [rmux.io/docs](https://rmux.io/docs/) で確認できます。

[インストールガイド](https://rmux.io/docs/get-started/)、[CLI リファレンス](https://rmux.io/docs/cli/)、[SDK サンプル](https://rmux.io/docs/examples/)、[ターミナル自動化サンプル](https://rmux.io/docs/examples/#/terminal-playwright)、[API ドキュメント](https://rmux.io/docs/api/) を含みます。

## CLI クイックスタート

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

## SDK クイックスタート

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

## Ratatui ウィジェット

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
  <source media="(prefers-color-scheme: dark)" srcset="https://rmux.io/rmux-architecture-dark.png?v=0.6.1-web-share">
  <source media="(prefers-color-scheme: light)" srcset="https://rmux.io/rmux-architecture-light.png?v=0.6.1-web-share">
  <img src="https://rmux.io/rmux-architecture-dark.png?v=0.6.1-web-share" alt="RMUX ランタイムアーキテクチャ" width="800">
</picture>

</div>

`rmux` は shell、session、window、pane、PTY process をローカル daemon に残します。ローカル client は IPC を使います。Web Share は明示的なブラウザアクセスです。daemon は選択された pane または session を end-to-end encrypted WebSocket で公開し、実行はあなたのマシン上に残ります。

## ワークスペース

| Crate | 役割 | 公開 |
| :--- | :--- | :--- |
| `rmux-types` | 共有されるプラットフォーム非依存の低レベル値型 | 公開 |
| `rmux-proto` | 分離式 IPC DTO、フレーミング、wire-safe なエラー | 公開 |
| `rmux-os` | 小さな OS 境界ヘルパー | 公開 |
| `rmux-ipc` | ローカル IPC エンドポイントとトランスポート | 公開 |
| `rmux-sdk` | デーモンベースの Rust SDK | 公開 |
| `ratatui-rmux` | Ratatui 統合ウィジェット | 公開 |
| `rmux-pty` | PTY 割り当て、resize、子プロセス制御 | サポート crate |
| `rmux-core` | session、pane、layout、format、hook、buffer | サポート crate |
| `rmux-server` | Tokio デーモンとリクエスト dispatch | サポート crate |
| `rmux-client` | ローカル IPC client と attach plumbing | サポート crate |
| `rmux` | CLI と隠しデーモンエントリポイント | 公開バイナリ |
| `rmux-render-core` | 共有 snapshot レンダリングコア | ワークスペース内部 |

<a id="platform-support"></a>

## プラットフォームサポート

| プラットフォーム | PTY バックエンド | IPC バックエンド | デフォルトエンドポイント |
| :--- | :--- | :--- | :--- |
| Linux | Unix PTY | Unix socket | `/tmp/rmux-{uid}/default` |
| macOS | Unix PTY | Unix socket | `/tmp/rmux-{uid}/default` |
| Windows | ConPTY | Named pipe | ユーザーごとの named pipe |

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

### `tmux.conf` 移行フォールバック

RMUX がデフォルトの設定探索で起動し、RMUX 設定ファイルが1つも読み込まれなかった場合、
移行用にフィルタ済みの `tmux.conf` を読み込めます。`-f` で設定ファイルを明示した場合、
このフォールバックは使われません。

フォールバックの探索場所：

- Linux と macOS：`/etc/tmux.conf`、`~/.tmux.conf`、`$XDG_CONFIG_HOME/tmux/tmux.conf`、`~/.config/tmux/tmux.conf`
- Windows：`%XDG_CONFIG_HOME%\tmux\tmux.conf`、`%USERPROFILE%\.tmux.conf`、`%APPDATA%\tmux\tmux.conf`

読み込み対象は意図的に絞っています。RMUX はサポート済みの静的オプションとキー割り当ての解除だけを取り込み、
tmux のキーバインド、環境変数や端末機能の変更、プラグイン用ユーザーオプション、hooks、
シェルコマンド、コマンドブロック、条件ブロック、`#(cmd)` のようなフォーマットジョブ、
再帰的な `source-file` エントリ、未サポートのオプションは実行せずにスキップします。
完全に無効化するには `RMUX_DISABLE_TMUX_FALLBACK=1` を設定してください。
フォールバックファイルは可能な範囲で読み込まれ、通常ファイルではないものと 1 MiB を超えるものは無視されます。

### ターミナル互換性のメモ

RMUX は、fish のようにターミナル機能を問い合わせる shell と連携できます。
端末属性問い合わせに応答し、Escape キーのタイミングも扱うため、RMUX pane 内でも
fish のプロンプトやキーシーケンスが通常どおり動作します。

Kitty graphics passthrough は、Kitty graphics protocol をサポートする外側のターミナルで利用できます。
対象には Kitty、Ghostty、WezTerm が含まれます。これは明示的に有効化します：

```tmux
set -g allow-passthrough on
```

ターミナルが Kitty graphics をサポートしているのに自動検出されない場合は、
terminal feature override を追加してください：

```tmux
set -as terminal-features 'xterm-kitty:kitty-graphics'
```

Windows では、OS が対応していれば RMUX は modern ConPTY passthrough を有効にします。
診断のためにこの backend mode を無効化するには `RMUX_CONPTY_NO_PASSTHROUGH=1` を設定してください。

<a id="verification"></a>

## 検証

このワークスペースは、ロックされた依存関係を使ってソースから検証できるように設計されています：

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked --no-fail-fast
```

追加のローカルチェック：

```sh
scripts/cfg-check.sh
scripts/unsafe-check.sh
scripts/no-network-in-runtime.sh
scripts/check-platform-neutrality.sh
scripts/ratatui-rmux-budget.sh
scripts/verify-package.sh
```

リリースアーティファクトのチェックは次のスクリプトで実行されます：

```sh
scripts/release-local.sh
scripts/package-unix.sh
```

上位 crate では `#![forbid(unsafe_code)]` を使用しています。OS とターミナル境界のコードは低レイヤーのランタイム crate に隔離されています。

## ライセンス

RMUX は次のいずれかのライセンスで利用できます：

- [MIT License](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)
