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
[![Validation de release](https://github.com/Helvesec/rmux/actions/workflows/ci.yml/badge.svg)](https://github.com/Helvesec/rmux/actions/workflows/ci.yml)
[![rmux 0.5.0](https://img.shields.io/badge/rmux-0.5.0-informational.svg)](#install)
[![Plateformes : Linux | macOS | Windows](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey.svg)](#platform-support)
[![Politique unsafe](https://img.shields.io/badge/unsafe-restricted-success.svg)](#verification)

<br />
<a href="https://rmux.io">
  <img src="https://rmux.io/rmux-terminal-demo.gif" width="500" alt="Démo de session terminal RMUX" />
</a>

</div>

> [!NOTE]
> RMUX intègre maintenant un multiplexage web chiffré de bout en bout, avec échange hybride post-quantique, via un frontend statique. [Voir la documentation Web Share du dépôt](docs/web-share.md).
>
> Pour une demande de fonctionnalité ou un signalement, [ouvrir une issue](https://github.com/Helvesec/rmux/issues).

## RMUX

RMUX est un <strong>multiplexeur</strong> Rust moderne, asynchrone et typé, avec plus de 90 commandes tmux en natif sur macOS, Linux et Windows, sans WSL.

Il fournit un SDK Rust public et une intégration Ratatui native.

Il peut s'utiliser en CLI, partager des sessions dans un navigateur, ou se piloter depuis Rust.

## Démos

Quelques exemples courts et concrets de ce que l'on peut faire avec RMUX.

<table>
  <tr>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-orchestration"><img src="https://rmux.io/demos/demo-orchestration.png" width="150" alt="Aperçu de la démo orchestration multi-agents"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/demo-orchestration"><strong>Orchestration multi-agents</strong></a></sub><br><sub>≃ 514 lignes</sub></td>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-broadcast"><img src="https://rmux.io/demos/demo-broadcast.png" width="150" alt="Aperçu de la démo Agent Broadcast Arena"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/broadcast-demo"><strong>Agent Broadcast Arena</strong></a></sub><br><sub>≃ 2,171 lignes</sub></td>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-zellij"><img src="https://rmux.io/demos/demo-zellij.png" width="150" alt="Aperçu de la démo Mini-Zellij"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/mini-zellij"><strong>Mini-Zellij</strong></a></sub><br><sub>≃ 944 lignes</sub></td>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-mirroring"><img src="https://rmux.io/demos/demo-mirroring.png" width="150" alt="Aperçu de la démo miroir terminal navigateur"></a><br><sub><a href="https://rmux.io/#demo-mirroring"><strong>Miroir terminal &lt;&gt; navigateur</strong></a></sub><br><sub>≃ 649 lignes</sub></td>
    <td align="center" width="20%"><a href="https://rmux.io/#demo-playwright"><img src="https://rmux.io/demos/demo-playwright.png" width="150" alt="Aperçu de la démo tests Playwright"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/terminal-playwright-demo"><strong>Tests Playwright</strong></a></sub><br><sub>≃ 1,495 lignes</sub></td>
  </tr>
</table>

## Web Multiplex (Web Share)

<p align="center">
<a href="https://rmux.io/docs/web-share/">
  <img src="https://rmux.io/web-share-browser.png" width="500" alt="Partage web RMUX" />
</a>
</p>

RMUX peut partager un pane ou une session dans un navigateur, créer des panes, redimensionner les splits, et garder l'exécution terminale en local.

```sh
# Démarrer un Web Share local sur loopback
rmux web-share

# Partager une session nommée
rmux new-session -d -s work
rmux web-share -t work

# Partager au-delà de localhost
rmux web-share --tunnel-provider localhost-run
```

Utiliser un tunnel provider, amener son propre ingress, ou héberger le frontend statique sur son propre domaine.

Points d'entrée utiles :

- [Vue d'ensemble Web Share du dépôt](docs/web-share.md)
- [Documentation Web Share](https://rmux.io/docs/web-share/)
- [Modèle de sécurité](https://rmux.io/docs/web-share/#/security)
- [Tunnel providers](https://rmux.io/docs/web-share/#/tunnels)

<a id="install"></a>

## Installation

Binaire précompilé pour macOS et Linux :

```sh
curl -fsSL https://rmux.io/install.sh | sh
```

Binaire macOS avec Homebrew :

```sh
brew install helvesec/rmux/rmux
```

Paquets Linux :

```sh
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://packages.rmux.io/debian/rmux.asc | sudo tee /etc/apt/keyrings/rmux.asc >/dev/null
echo "deb [signed-by=/etc/apt/keyrings/rmux.asc] https://packages.rmux.io/debian stable main" | sudo tee /etc/apt/sources.list.d/rmux.list >/dev/null
sudo apt update
sudo apt install rmux
```

Binaire précompilé pour Windows PowerShell :

```powershell
irm https://rmux.io/install.ps1 | iex
```

Windows avec Scoop :

```powershell
scoop bucket add rmux https://github.com/Helvesec/scoop-rmux
scoop install rmux
```

Les téléchargements directs et checksums SHA256 sont disponibles dans la [GitHub Release v0.5.0](https://github.com/helvesec/rmux/releases/tag/v0.5.0).

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

## Documentation

La documentation complète de RMUX est disponible sur [rmux.io/docs](https://rmux.io/docs/).

Elle inclut des [guides d'installation](https://rmux.io/docs/get-started/), des [références CLI](https://rmux.io/docs/cli/), des [exemples SDK](https://rmux.io/docs/examples/), des [exemples d'automatisation terminal](https://rmux.io/docs/examples/#/terminal-playwright), et la [documentation API](https://rmux.io/docs/api/).

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
rmux-sdk = "0.5"
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
  <source media="(prefers-color-scheme: dark)" srcset="https://rmux.io/rmux-architecture-dark.png?v=0.5.0-web-share">
  <source media="(prefers-color-scheme: light)" srcset="https://rmux.io/rmux-architecture-light.png?v=0.5.0-web-share">
  <img src="https://rmux.io/rmux-architecture-dark.png?v=0.5.0-web-share" alt="Architecture runtime RMUX" width="800">
</picture>

</div>

`rmux` garde les shells, sessions, fenêtres, panes et processus PTY dans le daemon local. Les clients locaux utilisent l'IPC. Web Share est un accès navigateur explicite : le daemon expose un pane ou une session sélectionnée via un WebSocket chiffré de bout en bout, pendant que l'exécution reste sur votre machine.

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

### Fallback de migration `tmux.conf`

Quand RMUX démarre avec la recherche de configuration par défaut et qu'aucun
fichier RMUX n'est chargé, il peut importer un `tmux.conf` filtré pour faciliter
la migration. Un chargement explicite avec `-f` n'utilise pas ce fallback.

Chemins de fallback :

- Linux et macOS : `/etc/tmux.conf`, `~/.tmux.conf`, `$XDG_CONFIG_HOME/tmux/tmux.conf`, `~/.config/tmux/tmux.conf`
- Windows : `%XDG_CONFIG_HOME%\tmux\tmux.conf`, `%USERPROFILE%\.tmux.conf`, `%APPDATA%\tmux\tmux.conf`

L'import est volontairement limité : RMUX conserve les options statiques
supportées et les suppressions de raccourcis, mais ignore les bindings tmux, les
modifications d'environnement ou de capacités terminal, les options utilisateur
de plugins, les hooks, les commandes shell, les blocs de commandes, les
conditions, les jobs de format comme `#(cmd)`, les `source-file` récursifs et
les options non supportées. Définissez `RMUX_DISABLE_TMUX_FALLBACK=1` pour le
désactiver entièrement. Les fichiers de fallback sont lus au mieux : les fichiers
non réguliers et les fichiers de plus de 1 MiB sont ignorés.

### Notes de compatibilité terminal

RMUX fonctionne avec les shells qui interrogent les capacités du terminal,
notamment fish. Il répond aux requêtes d'attributs terminal et gère le timing de
la touche Escape pour que les prompts fish et les séquences de touches se
comportent normalement dans les panes RMUX.

Le passthrough graphique Kitty est disponible pour les terminaux externes qui
supportent le protocole graphique Kitty, notamment Kitty, Ghostty et WezTerm. Il
est activé explicitement :

```tmux
set -g allow-passthrough on
```

Si votre terminal supporte les graphiques Kitty mais n'est pas détecté
automatiquement, ajoutez une override de capacité terminal :

```tmux
set -as terminal-features 'xterm-kitty:kitty-graphics'
```

Sur Windows, RMUX active le passthrough ConPTY moderne quand l'OS le supporte.
Définissez `RMUX_CONPTY_NO_PASSTHROUGH=1` pour désactiver ce mode backend lors
d'un diagnostic.

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
