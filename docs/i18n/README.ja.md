<div align="center">

<p align="center">
  <a href="https://rmux.io/">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="../rmux-logo-dark.svg">
      <img src="../rmux-logo-light.svg" width="238" alt="RMUX logo">
    </picture>
  </a>
</p>

<p align="center">
  <a href="https://rmux.io/">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="../rmux-wordmark-dark.svg">
      <img src="../rmux-wordmark-light.svg" width="276" alt="RMUX">
    </picture>
  </a>
</p>

<p align="center"><small><strong>ユニバーサルなマルチプレクサエンジン。</strong></small></p>

<p align="center">
  <picture><source media="(prefers-color-scheme: dark)" srcset="../readme-hero-native-dark.svg"><img src="../readme-hero-native-light.svg" width="340" alt="Native on Windows, Linux, and macOS"></picture>
</p>

<p align="center">
  <picture><source media="(prefers-color-scheme: dark)" srcset="../readme-hero-rule-dark.svg"><img src="../readme-hero-rule-light.svg" width="340" alt=""></picture>
</p>

<p align="center"><small><a href="../../README.md">English</a> · <a href="README.fr.md">Français</a> · <a href="README.zh-CN.md">简体中文</a> · 日本語</small></p>

<p align="center">
  <a href="#verification"><img src="https://img.shields.io/badge/unsafe-restricted-success.svg" alt="Unsafe policy"></a>
  <a href="https://github.com/Helvesec/rmux/actions/workflows/ci.yml?query=branch%3Amain"><img src="https://img.shields.io/github/actions/workflow/status/Helvesec/rmux/ci.yml?branch=main&amp;event=push&amp;label=CI" alt="CI"></a>
  <a href="https://www.bestpractices.dev/projects/13290"><img src="https://www.bestpractices.dev/projects/13290/badge" alt="OpenSSF Best Practices"></a>
  <a href="https://github.com/Helvesec/rmux/releases/tag/v0.8.0"><img src="https://img.shields.io/badge/rmux-0.8.0-informational.svg" alt="rmux 0.8.0"></a>
</p>

</div>


> [!NOTE]
> RMUX には E2E Web 多重化機能があります。[詳しくはドキュメントを参照してください。](../web-share.md)
>
> RMUX は現在 Python と TypeScript の SDK を提供しています: [librmux](https://pypi.org/project/librmux/), [@rmux/sdk](https://www.npmjs.com/package/@rmux/sdk)。
>
> 機能要望や問題報告は [issue を作成](https://github.com/Helvesec/rmux/issues)してください。

<p align="center">
  <a href="https://rmux.io/docs/web-share/">
    <img width="700" src="https://rmux.io/web-share-browser.gif" alt="RMUX Web Share">
  </a>
</p>

<a id="what-is-rmux"></a>

## 🧭 RMUX とは？

RMUX は、macOS、Linux、Windows で 90 以上の tmux コマンドをネイティブに提供する、モダンで非同期、型付きの Rust <strong>マルチプレクサ</strong>です。WSL は不要です。

公開 Rust SDK とネイティブ Ratatui 統合も提供します。

CLI から使うことも、セッションをブラウザに共有することも、Rust から操作することもできます。

<a id="features"></a>

## ✨ 機能

- shell、pane、window、session、scrollback のためのローカル daemon アーキテクチャ。
- 集中的な互換性テストを備えた tmux 風コマンドサーフェス。
- Linux、macOS、Windows のネイティブバックエンド。
- 型付き自動化とターミナル状態アサーションのための公開 Rust SDK。
- Rust ターミナルアプリケーションで RMUX pane を描画する Ratatui ウィジェット。
- ハイブリッド・ポスト量子エンドツーエンド暗号化を備えたブラウザ Web Share。
- GitHub Releases、APT、RPM、Homebrew、WinGet、Scoop、Chocolatey、crates.io 向けリリースパッケージング。

<a id="quick-start"></a>

## 🚀 CLI クイックスタート

ローカルのコマンドヘルプを確認します：

```sh
rmux list-commands
rmux new-session --help
rmux split-window --help
rmux web-share --help
```

`rmux -V` でインストール済みのバージョンを確認できます。

<a id="demos"></a>
<a id="screenshots"></a>

## 🎬 デモ

RMUX を何に使えるかを示す短い例です。

<div align="center">

<table align="center">
  <tr>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-orchestration"><img src="https://rmux.io/demos/demo-orchestration.png" width="150" alt="マルチエージェント編成デモのプレビュー"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/demo-orchestration"><strong>マルチエージェント編成</strong></a></sub><br><sub>約 514 行</sub></td>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-broadcast"><img src="https://rmux.io/demos/demo-broadcast.png" width="150" alt="Agent Broadcast Arena デモのプレビュー"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/broadcast-demo"><strong>Agent Broadcast Arena</strong></a></sub><br><sub>約 2,171 行</sub></td>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-zellij"><img src="https://rmux.io/demos/demo-zellij.png" width="150" alt="Mini-Zellij デモのプレビュー"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/mini-zellij"><strong>Mini-Zellij</strong></a></sub><br><sub>約 944 行</sub></td>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-playwright"><img src="https://rmux.io/demos/demo-playwright.png" width="150" alt="ターミナル自動化デモのプレビュー"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/terminal-playwright-demo"><strong>ターミナル自動化</strong></a></sub><br><sub>約 1,495 行</sub></td>
  </tr>
</table>

</div>

<a id="installation"></a>

## 📦 インストール

| プラットフォーム / マネージャー | コマンド |
| :--- | :--- |
| <picture><source media="(prefers-color-scheme: dark)" srcset="../install/apple.svg"><img src="../install/apple-light.svg" width="28" alt="macOS"></picture> / Homebrew | `brew install rmux` |
| <picture><source media="(prefers-color-scheme: dark)" srcset="../install/windows.svg"><img src="../install/windows-light.svg" width="28" alt="Windows"></picture> / installer | `irm https://rmux.io/install.ps1 \| iex` |
| <picture><source media="(prefers-color-scheme: dark)" srcset="../install/windows.svg"><img src="../install/windows-light.svg" width="28" alt="Windows"></picture> / WinGet | `winget install rmux` |
| <picture><source media="(prefers-color-scheme: dark)" srcset="../install/windows.svg"><img src="../install/windows-light.svg" width="28" alt="Windows"></picture> / Scoop | `scoop bucket add rmux https://github.com/Helvesec/scoop-rmux && scoop install rmux` |
| <picture><source media="(prefers-color-scheme: dark)" srcset="../install/windows.svg"><img src="../install/windows-light.svg" width="28" alt="Windows"></picture> / Chocolatey | `choco install rmux` |
| <picture><source media="(prefers-color-scheme: dark)" srcset="../install/linux.svg"><img src="../install/linux-light.svg" width="28" alt="Linux"></picture> / APT | [APT セットアップガイド](https://rmux.io/docs/get-started/)を参照 |
| <picture><source media="(prefers-color-scheme: dark)" srcset="../install/linux.svg"><img src="../install/linux-light.svg" width="28" alt="Linux"></picture> / DNF | [DNF セットアップガイド](https://rmux.io/docs/get-started/)を参照 |
| <picture><source media="(prefers-color-scheme: dark)" srcset="../install/linux.svg"><img src="../install/linux-light.svg" width="28" alt="Linux"></picture> <picture><source media="(prefers-color-scheme: dark)" srcset="../install/apple.svg"><img src="../install/apple-light.svg" width="28" alt="macOS"></picture> / Nix | `nix profile install github:Helvesec/rmux` |
| <picture><source media="(prefers-color-scheme: dark)" srcset="../install/rust.svg"><img src="../install/rust-light.svg" width="28" alt="Rust"></picture> / Cargo | `cargo install rmux --locked` |

直接ダウンロード（`.tar.gz`、`.deb`、`.rpm`、`.zip`）は [v0.8.0 GitHub Release](https://github.com/helvesec/rmux/releases/tag/v0.8.0) から利用できます。

パッケージマネージャはレジストリ審査中に遅れることがあります。直接ダウンロードと rmux.io インストーラは現在の GitHub Release に追従します。

Unix の `.tar.gz` を直接ダウンロードした場合は、展開したアーカイブ内で
`./install.sh --prefix ~/.local` を実行してください。このインストーラは、
小さな公開 CLI が完全な helper に必ず到達できるよう、必要な `bin/` と
`libexec/` の配置を保ちます。

リリースパッケージでは、高速な detached コマンドに小さな公開 CLI を使い、複雑な tmux 互換コマンド形式には非公開の完全 CLI helper を使う場合があります。Windows パッケージでは `rmux.exe` が軽量 dispatcher になり、完全 CLI は `libexec/rmux/rmux.exe` に配置されます。CLI 互換性の診断中に完全 helper を強制するには `RMUX_DISABLE_TINY_CLI=1` を設定してください。

<a id="claude-teammate-mode"></a>

## 🤝 Claude Teammate モード

ローカル RMUX workspace で Claude Code を実行し、
[tmux teammate mode](https://code.claude.com/docs/en/agent-teams) を有効にします。

<p align="center">
  <img src="../teammate.jpg" alt="RMUX の Claude Teammate モード" width="900">
</p>

```bash
rmux claude [args]
# 例: rmux claude --dangerously-skip-permissions
```

RMUX は attached session を開き、`--teammate-mode tmux` と `[args]` を
そのまま Claude に渡します。

内部の仕組み: コマンドを正しくルーティングするため、RMUX は Claude の
`PATH` の先頭にプライベートな `tmux` shim を追加します。これは Claude
プロセス内に厳密に限定され、システムの `tmux` インストールとは競合しません。

このリポジトリの外でも Claude が RMUX CLI、SDK、web-share、automation
patterns を参照できるようにするには、RMUX の user-level Claude Code
skill をインストールします:

```bash
rmux claude install-skill
```

この skill は Claude profile 配下にインストールされます（Linux/macOS では
`~/.claude/skills/rmux`、Windows では
`%USERPROFILE%\.claude\skills\rmux`）。プロジェクトが隠し `.claude`
source-tree directory を持たずに package/install できるよう、
リポジトリ内の source copy は `resources/claude/skills/rmux/SKILL.md` に
保持されています。

注: マシンに `claude` がインストールされている必要があります。

<a id="configuration"></a>

## ⚙️ 設定

Linux と macOS では、RMUX は標準の system / user locations から `.rmux.conf` を読み込みます：

1. `/etc/rmux.conf`
2. `~/.rmux.conf`
3. `$XDG_CONFIG_HOME/rmux/rmux.conf`
4. `~/.config/rmux/rmux.conf`

Windows では、RMUX は次の場所から `.rmux.conf` を読み込みます：

1. `%XDG_CONFIG_HOME%\rmux\rmux.conf`
2. `%USERPROFILE%\.rmux.conf`
3. `%APPDATA%\rmux\rmux.conf`
4. `%RMUX_CONFIG_FILE%`

### `tmux.conf` 互換性

RMUX がデフォルト設定検索で起動し、RMUX 設定ファイルが読み込まれなかった場合、標準の `tmux.conf` の場所も確認します。`-f` で明示された設定ファイルではこの fallback は発生しません。

Fallback ファイルは tmux 互換の source parser を使い、best-effort で読み込まれます。サポート済みコマンドは適用され、未サポートの plugin 行は起動を中断せずに報告されます。autoload を無効化するには `RMUX_DISABLE_TMUX_FALLBACK=1` を設定してください。

Unix では、RMUX はコマンド環境内に socket ごとのプライベート `tmux` shim も提供し、一般的な plugin script が RMUX に戻るようにします。無効化するには `RMUX_DISABLE_TMUX_SHIM=1` を設定してください。

<a id="web-sharing"></a>

## 🌐 Web Multiplex (Web Share)

RMUX は pane や session をブラウザに共有し、pane を作成し、split をリサイズし、ターミナル実行をローカルに保ちます。

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

- [リポジトリの Web Share 概要](../web-share.md)
- [Web Share ドキュメント](https://rmux.io/docs/web-share/)
- [セキュリティモデル](https://rmux.io/docs/web-share/#/security)
- [Tunnel providers](https://rmux.io/docs/web-share/#/tunnels)

<a id="scripting-api"></a>

## 🧰 スクリプト/API

SDK はローカル RMUX daemon に接続し、自動化向けに sessions、panes、
streams、waits、snapshots を公開します。

```sh
cargo add rmux-sdk
pip install librmux
npm install @rmux/sdk
```

- Rust SDK: [`rmux-sdk`](https://crates.io/crates/rmux-sdk)
- Python SDK: [`librmux`](https://pypi.org/project/librmux/)
- TypeScript SDK: [`@rmux/sdk`](https://www.npmjs.com/package/@rmux/sdk)

<a id="documentation"></a>

## 📚 ドキュメント

RMUX の完全なドキュメントは [rmux.io/docs](https://rmux.io/docs/) で利用できます。

含まれるもの：

- [インストールガイド](https://rmux.io/docs/get-started/)
- [CLI リファレンス](https://rmux.io/docs/cli/)
- [例](https://rmux.io/docs/examples/)
- [API reference](https://rmux.io/docs/api/)
- [リポジトリ SDK 概要](../scripting-sdk.md)
- [Web Share](https://rmux.io/docs/web-share/)

ネイティブなターミナル選択の直感性を保ちつつ、より簡単な split binding と clipboard integration を追加する、人間向けの ergonomic profile については [docs/human-friendly-config.md](../human-friendly-config.md) を参照してください。

## 🧩 Ratatui ウィジェット

```rust
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};
use ratatui_rmux::{PaneState, PaneWidget};
use rmux_sdk::PaneSnapshot;

fn render(snapshot: PaneSnapshot, area: Rect, buffer: &mut Buffer) {
    let state = PaneState::from_snapshot(snapshot);
    PaneWidget::new(&state).render(area, buffer);
}
```

## 🏗️ アーキテクチャ

<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://rmux.io/rmux-architecture-dark.png?v=0.8.0-web-share">
  <source media="(prefers-color-scheme: light)" srcset="https://rmux.io/rmux-architecture-light.png?v=0.8.0-web-share">
  <img src="https://rmux.io/rmux-architecture-dark.png?v=0.8.0-web-share" alt="RMUX ランタイムアーキテクチャ" width="800">
</picture>

</div>

`rmux` は shell、session、window、pane、PTY process をローカル daemon に残します。ローカル client は IPC を使います。Web Share は明示的なブラウザアクセスです。daemon は選択された pane または session を end-to-end encrypted WebSocket で公開し、実行はあなたのマシン上に残ります。

## 🧱 ワークスペース

| Crate | 役割 | 公開 |
| :--- | :--- | :--- |
| `rmux-types` | 共有されるプラットフォーム非依存の値型 | 公開 |
| `rmux-proto` | 分離式 IPC DTO、framing、wire-safe な error | 公開 |
| `rmux-os` | 小さな OS 境界 helper | 公開 |
| `rmux-ipc` | ローカル IPC endpoint と transport | 公開 |
| `rmux-sdk` | daemon-backed Rust SDK | 公開 |
| `ratatui-rmux` | Ratatui integration widget | 公開 |
| `rmux-web-crypto` | Web Share E2EE core と WASM crypto boundary | 公開 |
| `rmux-pty` | PTY allocation、resize、child process control | support crate |
| `rmux-core` | session、pane、layout、format、hook、buffer | support crate |
| `rmux-server` | Tokio daemon と request dispatch | support crate |
| `rmux-client` | ローカル IPC client と attach plumbing | support crate |
| `rmux` | CLI と隠し daemon entrypoint | public binary |
| `rmux-render-core` | 共有 snapshot rendering core | workspace-internal |

<a id="platform-support"></a>

## 🖥️ プラットフォームサポート

| プラットフォーム | PTY backend | IPC backend | デフォルト endpoint |
| :--- | :--- | :--- | :--- |
| Linux | Unix PTY | Unix socket | `/tmp/rmux-{uid}/default` |
| macOS | Unix PTY | Unix socket | `/tmp/rmux-{uid}/default` |
| Windows | ConPTY | named pipe | ユーザーごとの named pipe |

## 🧾 ターミナル互換性のメモ

RMUX は、fish などターミナル機能を問い合わせる shell と連携できます。端末属性問い合わせに応答し、Escape キーのタイミングも扱うため、RMUX pane 内でも fish prompt と key sequence が通常どおり動作します。

Graphics passthrough は、Kitty graphics または SIXEL をサポートする外側の terminal で利用できます。RMUX は Kitty、Ghostty、WezTerm で Kitty graphics を検出し、foot、mintty、mlterm、WezTerm などで SIXEL を検出します。これは opt-in です：

```tmux
set -g allow-passthrough on
```

tmux の値 `all` は設定互換性のために受け付けられます。RMUX は attached pane を描画するため、`all` は現在、unattached pane に passthrough を追加するのではなく `on` と同じように動作します。

terminal がいずれかの protocol をサポートしているのに自動検出されない場合は、terminal feature override を追加してください：

```tmux
set -as terminal-features 'xterm-kitty:kitty-graphics'
set -as terminal-features 'xterm*:sixel'
```

SIXEL passthrough は自動化された Unix PTY attach regression suite でカバーされています。Windows では、OS が対応していれば RMUX は modern ConPTY passthrough を有効にしますが、SIXEL display は外側の terminal に依存します。トラブルシュート時にこの backend mode を無効化するには `RMUX_CONPTY_NO_PASSTHROUGH=1` を設定してください。

<a id="verification"></a>

## 🧪 検証

この workspace は、ロックされた依存関係を使って source から確認できるように設計されています：

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

Release artifact checks は次で実行されます：

```sh
scripts/release-local.sh
scripts/package-unix.sh
scripts/package-debian.sh
scripts/verify-debian-package.sh
scripts/package-rpm.sh
scripts/verify-rpm-package.sh
scripts/smoke-snap-package.sh
scripts/package-windows.ps1
scripts/verify-package-windows.ps1
scripts/generate-apt-repository.sh
scripts/generate-rpm-repository.sh
scripts/generate-homebrew-formula.sh
scripts/generate-winget-manifest.sh
scripts/generate-scoop-manifest.sh
scripts/generate-chocolatey-package.sh
```

上位 crate では `#![forbid(unsafe_code)]` を使用しています。OS と terminal boundary code は低レイヤーの runtime crate に隔離されています。

## ⚖️ ライセンス

RMUX は次のいずれかのライセンスで利用できます：

- [MIT License](../../LICENSE-MIT)
- [Apache License 2.0](../../LICENSE-APACHE)
