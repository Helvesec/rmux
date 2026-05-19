<div align="center">

<a href="https://rmux.io">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://rmux.io/rmux-header-dark.svg">
    <img src="https://rmux.io/rmux-header.svg" alt="RMUX" width="500">
  </picture>
</a>


**Multiplexeur Rust universel pour l'ère des agents : détachable, scriptable et inspectable, avec CLI compatible tmux, SDK adossé à un daemon, et intégration native [Ratatui](https://ratatui.rs).**

[English](README.md) · Français · [简体中文](README.zh-CN.md) · [日本語](README.ja.md)

[![Licence : MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![Validation release](https://github.com/Helvesec/rmux/actions/workflows/ci.yml/badge.svg)](https://github.com/Helvesec/rmux/actions/workflows/ci.yml)
[![rmux 0.2.0](https://img.shields.io/badge/rmux-0.2.0-informational.svg)](#install)
[![Plateformes : Linux | macOS | Windows](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey.svg)](#platform-support)
[![Politique unsafe](https://img.shields.io/badge/unsafe-restricted-success.svg)](#verification)

<br />
<a href="https://rmux.io">
  <img src="https://rmux.io/rmux-terminal-demo.gif" width="500" alt="Démo de session terminal RMUX" />
</a>

</div>

> [!IMPORTANT]
> Version actuelle : **v0.2.0**, publiée le **18 mai 2026**. Les 90 commandes compatibles tmux sont implémentées, mais des bugs restent possibles : cette version est un aperçu public récent. [Signaler les problèmes](https://github.com/helvesec/rmux/issues) rencontrés.

## Pourquoi RMUX

RMUX existe parce que je crois que le cas d'usage de tmux n'a ete explore qu'en partie. Mon point de depart etait simple : je voulais lancer des agents longue duree via SSH sans perdre leurs terminaux, tout en pouvant inspecter, scripter et orchestrer ce qui les entoure.

J'ai donc reconstruit cette idee from scratch en Rust : un multiplexeur ultra-rapide compatible tmux, avec SDK type, sessions persistantes, snapshots structures et transports locaux natifs sur Linux, macOS et Windows, y compris les Windows Named Pipes. Pas besoin de WSL.

RMUX est utilisable par les agents, les workflows CLI headless et les humains : vous pouvez donner une execution detachable aux applications terminal, vous reconnecter plus tard, inspecter leur etat, les piloter depuis du code, ou simplement l'utiliser pour du travail terminal classique façon tmux.

<table>
  <tr>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-orchestration"><img src="https://rmux.io/demos/demo-orchestration.png" width="150" alt="Aperçu de la démo orchestration multi-agents"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/demo-orchestration"><strong>Orchestration multi-agents</strong></a></sub><br><sub>≃ 514 lines</sub></td>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-broadcast"><img src="https://rmux.io/demos/demo-broadcast.png" width="150" alt="Aperçu de la démo Agent Broadcast Arena"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/demo-broadcast"><strong>Agent Broadcast Arena</strong></a></sub><br><sub>≃ 2,171 lines</sub></td>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-zellij"><img src="https://rmux.io/demos/demo-zellij.png" width="150" alt="Aperçu de la démo Mini-Zellij"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/demo-zellij"><strong>Mini-Zellij</strong></a></sub><br><sub>≃ 944 lines</sub></td>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-mirroring"><img src="https://rmux.io/demos/demo-mirroring.png" width="150" alt="Aperçu de la démo miroir terminal navigateur"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/demo-mirroring"><strong>Miroir terminal &lt;&gt; navigateur</strong></a></sub><br><sub>≃ 649 lines</sub></td>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-playwright"><img src="https://rmux.io/demos/demo-playwright.png" width="150" alt="Aperçu de la démo tests Playwright"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/demo-playwright"><strong>Tests Playwright</strong></a></sub><br><sub>≃ 1,495 lines</sub></td>
  </tr>
</table>

<a id="install"></a>

## Installation

Binaire précompilé pour macOS et Linux :

```sh
curl -fsSL https://rmux.io/install.sh | sh
```

Binaire précompilé pour Windows PowerShell :

```powershell
irm https://rmux.io/install.ps1 | iex
```

Les téléchargements directs et checksums SHA256 sont disponibles dans la [GitHub Release v0.2.0](https://github.com/helvesec/rmux/releases/tag/v0.2.0).

Depuis crates.io avec Cargo :

```sh
cargo install rmux --locked
```

Depuis un checkout local :

```sh
cargo install --path . --locked
```

Pour les applications Rust :

```sh
cargo add rmux-sdk
cargo add ratatui-rmux
```

## Docs

La documentation complète est disponible sur [rmux.io/docs](https://rmux.io/docs/) · [Démarrer](https://rmux.io/docs/get-started/) · [Exemples](https://rmux.io/docs/examples/) · [API](https://rmux.io/docs/api/) · [CLI](https://rmux.io/docs/cli/).

## Démarrage rapide CLI

```sh
rmux new-session -d -s work
rmux split-window -h -t work
rmux send-keys -t work 'echo "hello from rmux"' Enter
rmux attach-session -t work
```

Aide locale des commandes :

```sh
rmux list-commands
rmux new-session --help
rmux split-window --help
```

## Démarrage rapide SDK

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

## Widget Ratatui

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
  <img src="https://rmux.io/rmux-architecture-dark.png" alt="Architecture runtime RMUX" width="800">
</picture>

</div>

Trois surfaces publiques — une CLI `rmux`, une crate Rust `rmux-sdk` et un widget `ratatui-rmux` — partagent un protocole local unique pour parler au daemon. Ce qu'une surface peut faire, les autres peuvent le faire aussi.

## Workspace

| Crate | Rôle | Publication |
| :--- | :--- | :--- |
| `rmux-types` | Types de valeurs bas niveau partagés | publique |
| `rmux-proto` | DTO IPC détachés, framing, erreurs sûres sur le fil | publique |
| `rmux-os` | Petits helpers à la frontière OS | publique |
| `rmux-ipc` | Endpoints et transports IPC locaux | publique |
| `rmux-sdk` | SDK Rust adossé au daemon | publique |
| `ratatui-rmux` | Widget d'intégration Ratatui | publique |
| `rmux-pty` | Allocation PTY, resize et contrôle de processus enfant | crate de support |
| `rmux-core` | Sessions, panes, layouts, formats, hooks, buffers | crate de support |
| `rmux-server` | Daemon Tokio et dispatch des requêtes | crate de support |
| `rmux-client` | Client IPC local et plomberie du mode attach | crate de support |
| `rmux` | CLI et point d'entrée daemon masqué | binaire public |
| `rmux-render-core` | Core de rendu partagé pour snapshots | interne au workspace |

<a id="platform-support"></a>

## Plateformes

| Plateforme | Backend PTY | Backend IPC | Endpoint par défaut |
| :--- | :--- | :--- | :--- |
| Linux | PTY Unix | Socket Unix | `/tmp/rmux-{uid}/default` |
| macOS | PTY Unix | Socket Unix | `/tmp/rmux-{uid}/default` |
| Windows | ConPTY | Named pipe | named pipe par utilisateur |

## Configuration

Sur Linux et macOS, RMUX lit `.rmux.conf` depuis les emplacements système et utilisateur standards :

1. `/etc/rmux.conf`
2. `~/.rmux.conf`
3. `$XDG_CONFIG_HOME/rmux/rmux.conf`
4. `~/.config/rmux/rmux.conf`

Sur Windows, RMUX lit également `.rmux.conf`, depuis les emplacements suivants :

1. `%XDG_CONFIG_HOME%\rmux\rmux.conf`
2. `%USERPROFILE%\.rmux.conf`
3. `%APPDATA%\rmux\rmux.conf`
4. `%RMUX_CONFIG_FILE%`

<a id="verification"></a>

## Vérification

Le workspace est conçu pour être vérifié depuis les sources avec des dépendances verrouillées :

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked --no-fail-fast
```

Vérifications locales supplémentaires :

```sh
scripts/cfg-check.sh
scripts/unsafe-check.sh
scripts/no-network-in-runtime.sh
scripts/check-platform-neutrality.sh
scripts/ratatui-rmux-budget.sh
scripts/verify-package.sh
```

Les vérifications d'artefacts de release sont pilotées par :

```sh
scripts/release-local.sh
scripts/package-unix.sh
```

`#![forbid(unsafe_code)]` est utilisé dans les crates de haut niveau. Le code lié à l'OS et au terminal est isolé dans les crates runtime de plus bas niveau.

## Licence

RMUX est distribué sous double licence, au choix :

- [Licence MIT](LICENSE-MIT)
- [Licence Apache 2.0](LICENSE-APACHE)
