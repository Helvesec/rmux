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

<p align="center"><small><strong>Le moteur universel de multiplexage.</strong></small></p>

<p align="center">
  <picture><source media="(prefers-color-scheme: dark)" srcset="../readme-hero-native-dark.svg"><img src="../readme-hero-native-light.svg" width="340" alt="Native on Windows, Linux, and macOS"></picture>
</p>

<p align="center">
  <picture><source media="(prefers-color-scheme: dark)" srcset="../readme-hero-rule-dark.svg"><img src="../readme-hero-rule-light.svg" width="340" alt=""></picture>
</p>

<p align="center"><small><a href="../../README.md">English</a> · Français · <a href="README.zh-CN.md">简体中文</a> · <a href="README.ja.md">日本語</a></small></p>

<p align="center">
  <a href="#verification"><img src="https://img.shields.io/badge/unsafe-restricted-success.svg" alt="Unsafe policy"></a>
  <a href="https://github.com/Helvesec/rmux/actions/workflows/ci.yml?query=branch%3Amain"><img src="https://img.shields.io/github/actions/workflow/status/Helvesec/rmux/ci.yml?branch=main&amp;event=push&amp;label=CI" alt="CI"></a>
  <a href="https://www.bestpractices.dev/projects/13290"><img src="https://www.bestpractices.dev/projects/13290/badge" alt="OpenSSF Best Practices"></a>
  <a href="https://github.com/Helvesec/rmux/releases/tag/v0.9.1"><img src="https://img.shields.io/badge/rmux-0.9.1-informational.svg" alt="rmux 0.9.1"></a>
</p>

</div>


> [!NOTE]
> RMUX dispose maintenant d'une fonctionnalité de multiplexage web E2E. [En savoir plus dans la documentation.](../web-share.md)
>
> RMUX fournit maintenant des SDK Python et TypeScript : [librmux](https://pypi.org/project/librmux/), [@rmux/sdk](https://www.npmjs.com/package/@rmux/sdk).
>
> Pour une demande de fonctionnalité ou un signalement, veuillez [ouvrir une issue](https://github.com/Helvesec/rmux/issues).

<p align="center">
  <a href="https://rmux.io/docs/web-share/">
    <img width="700" src="https://rmux.io/web-share-browser.gif" alt="Partage web RMUX">
  </a>
</p>

<a id="what-is-rmux"></a>

## 🧭 Qu'est-ce que RMUX ?

RMUX est un <strong>multiplexeur</strong> Rust moderne, asynchrone et typé, avec plus de 90 commandes tmux natives sur macOS, Linux et Windows, sans WSL.

Il fournit un SDK Rust public et une intégration Ratatui native.

Utilisez-le depuis la CLI, partagez des sessions dans un navigateur, ou pilotez-le depuis Rust.

<a id="features"></a>

## ✨ Fonctionnalités

- Architecture daemon locale pour shells, panes, windows, sessions et scrollback.
- Surface de commandes de style tmux avec tests de compatibilité ciblés.
- Backends natifs Linux, macOS et Windows.
- SDK Rust public pour automatisation typée et assertions d'état terminal.
- Widget Ratatui pour afficher des panes RMUX dans des applications terminal Rust.
- Web Share navigateur avec chiffrement de bout en bout hybride post-quantique.
- Packaging de release pour GitHub Releases, APT, RPM, Homebrew, WinGet, Scoop, Chocolatey et les crates SDK publiques sur crates.io.

<a id="quick-start"></a>

## 🚀 Démarrage rapide CLI

Consultez l'aide locale des commandes :

```sh
rmux list-commands
rmux new-session --help
rmux split-window --help
rmux web-share --help
```

Utilisez `rmux -V` pour connaître la version installée.

<a id="demos"></a>
<a id="screenshots"></a>

## 🎬 Démos

Quelques exemples courts de ce que RMUX permet de faire.

<div align="center">

<table align="center">
  <tr>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-orchestration"><img src="https://rmux.io/demos/demo-orchestration.png" width="150" alt="Aperçu de la démo orchestration multi-agents"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/demo-orchestration"><strong>Orchestration multi-agents</strong></a></sub><br><sub>≃ 514 lignes</sub></td>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-broadcast"><img src="https://rmux.io/demos/demo-broadcast.png" width="150" alt="Aperçu de la démo Agent Broadcast Arena"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/broadcast-demo"><strong>Agent Broadcast Arena</strong></a></sub><br><sub>≃ 2,171 lignes</sub></td>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-zellij"><img src="https://rmux.io/demos/demo-zellij.png" width="150" alt="Aperçu de la démo Mini-Zellij"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/mini-zellij"><strong>Mini-Zellij</strong></a></sub><br><sub>≃ 944 lignes</sub></td>
    <td align="center" width="25%"><a href="https://rmux.io/#demo-playwright"><img src="https://rmux.io/demos/demo-playwright.png" width="150" alt="Aperçu de la démo automatisation terminal"></a><br><sub><a href="https://github.com/Helvesec/rmux-demos/tree/main/terminal-playwright-demo"><strong>Automatisation terminal</strong></a></sub><br><sub>≃ 1,495 lignes</sub></td>
  </tr>
</table>

</div>

<a id="installation"></a>

## 📦 Installation

| Plateforme / gestionnaire | Commande |
| :--- | :--- |
| <picture><source media="(prefers-color-scheme: dark)" srcset="../install/apple.svg"><img src="../install/apple-light.svg" width="28" alt="macOS"></picture> / Homebrew | `brew install rmux` |
| <picture><source media="(prefers-color-scheme: dark)" srcset="../install/windows.svg"><img src="../install/windows-light.svg" width="28" alt="Windows"></picture> / WinGet | `winget install rmux` |
| <picture><source media="(prefers-color-scheme: dark)" srcset="../install/windows.svg"><img src="../install/windows-light.svg" width="28" alt="Windows"></picture> / Scoop | `scoop bucket add rmux https://github.com/Helvesec/scoop-rmux && scoop install rmux` |
| <picture><source media="(prefers-color-scheme: dark)" srcset="../install/windows.svg"><img src="../install/windows-light.svg" width="28" alt="Windows"></picture> / Chocolatey | `choco install rmux` |
| <picture><source media="(prefers-color-scheme: dark)" srcset="../install/linux.svg"><img src="../install/linux-light.svg" width="28" alt="Linux"></picture> / APT | Voir le [guide APT](https://rmux.io/docs/get-started/) |
| <picture><source media="(prefers-color-scheme: dark)" srcset="../install/linux.svg"><img src="../install/linux-light.svg" width="28" alt="Linux"></picture> / DNF | Voir le [guide DNF](https://rmux.io/docs/get-started/) |
| <picture><source media="(prefers-color-scheme: dark)" srcset="../install/linux.svg"><img src="../install/linux-light.svg" width="28" alt="Linux"></picture> <picture><source media="(prefers-color-scheme: dark)" srcset="../install/apple.svg"><img src="../install/apple-light.svg" width="28" alt="macOS"></picture> / Nix | `nix profile install github:Helvesec/rmux` |
| <picture><source media="(prefers-color-scheme: dark)" srcset="../install/rust.svg"><img src="../install/rust-light.svg" width="28" alt="Rust"></picture> / Cargo | `cargo install rmux --locked` |

Les téléchargements directs (`.tar.gz`, `.deb`, `.rpm`, `.zip`) sont disponibles dans la [GitHub Release v0.9.1](https://github.com/Helvesec/rmux/releases/tag/v0.9.1).

Les gestionnaires de paquets peuvent avoir du retard pendant la revue des registres ; utilisez le téléchargement versionné de la GitHub Release tant qu'un gestionnaire n'est pas à jour.

Sous Windows, extrayez l'intégralité du `.zip` et ajoutez la racine du paquet
extrait au `PATH`. Conservez ensemble `rmux.exe`, `rmux-daemon.exe` et
`libexec/rmux/rmux.exe` ; copier uniquement l'exécutable public ne constitue
pas une installation valide.

Pour les téléchargements Unix `.tar.gz`, lancez `./install.sh --prefix ~/.local`
depuis l'archive extraite. L'installateur préserve le layout `bin/` et
`libexec/` requis pour que la petite CLI publique puisse toujours atteindre son
helper complet.

Les paquets de release peuvent utiliser une petite CLI publique pour les commandes détachées rapides et un helper CLI complet privé pour les formes de commandes tmux compatibles plus complexes. Les paquets Windows livrent `rmux.exe` comme dispatcher léger et placent la CLI complète sous `libexec/rmux/rmux.exe`. Définissez `RMUX_DISABLE_TINY_CLI=1` pour forcer le helper complet pendant le diagnostic de compatibilité CLI.

<a id="claude-teammate-mode"></a>

## 🤝 Mode Claude Teammate

Exécutez Claude Code dans un espace de travail RMUX local avec le
[mode teammate tmux](https://code.claude.com/docs/en/agent-teams) activé.

<p align="center">
  <img src="../teammate.jpg" alt="Mode Claude Teammate dans RMUX" width="900">
</p>

```bash
rmux claude [args]
# ex. rmux claude --dangerously-skip-permissions
```

RMUX ouvre une session attachée et passe automatiquement `--teammate-mode tmux`
à Claude, avec vos `[args]`.

Sous le capot : pour router correctement les commandes, RMUX ajoute un shim
`tmux` privé au début du `PATH` de Claude. Ce périmètre est strictement limité
au processus Claude et n'entre pas en conflit avec votre installation système
de `tmux`.

Installez la skill Claude Code utilisateur de RMUX pour que Claude conserve les
repères RMUX hors de ce dépôt : CLI, SDK, web-share et patterns
d'automatisation.

```bash
rmux claude install-skill
```

La skill est installée dans le profil Claude (`~/.claude/skills/rmux` sous
Linux/macOS, `%USERPROFILE%\.claude\skills\rmux` sous Windows). Le dépôt garde
la copie source dans `resources/claude/skills/rmux/SKILL.md`, afin que le
projet puisse la packager et l'installer sans dossier source caché `.claude`
dans l'arborescence.

Note : nécessite que `claude` soit installé sur votre machine.

<a id="configuration"></a>

## ⚙️ Configuration

Sur Linux et macOS, RMUX lit `.rmux.conf` depuis les emplacements système et utilisateur standards :

1. `/etc/rmux.conf`
2. `~/.rmux.conf`
3. `$XDG_CONFIG_HOME/rmux/rmux.conf`
4. `~/.config/rmux/rmux.conf`

Sur Windows, RMUX lit `.rmux.conf` depuis :

1. `%XDG_CONFIG_HOME%\rmux\rmux.conf`
2. `%USERPROFILE%\.rmux.conf`
3. `%APPDATA%\rmux\rmux.conf`
4. `%RMUX_CONFIG_FILE%`

### Compatibilité `tmux.conf`

Quand RMUX démarre avec la recherche de configuration par défaut et qu'aucun fichier RMUX n'est chargé, il vérifie aussi les emplacements standards de `tmux.conf`. Les fichiers de configuration explicites passés avec `-f` ne déclenchent pas ce fallback.

Les fichiers de fallback utilisent le parser de source compatible tmux et sont chargés au mieux. Les commandes supportées sont appliquées ; les lignes de plugins non supportées sont signalées sans interrompre le démarrage. Définissez `RMUX_DISABLE_TMUX_FALLBACK=1` pour désactiver l'autoload.

Sur Unix, RMUX fournit aussi un shim `tmux` privé par socket dans les environnements de commande afin que les scripts de plugins courants reviennent vers RMUX. Définissez `RMUX_DISABLE_TMUX_SHIM=1` pour le désactiver.

<a id="web-sharing"></a>

## 🌐 Web Multiplex (Web Share)

RMUX peut partager un pane ou une session dans un navigateur, créer des panes, redimensionner les splits et garder l'exécution terminale en local.

```sh
# Démarrer un Web Share local sur loopback
rmux web-share

# Partager une session nommée
rmux new-session -d -s work
rmux web-share -t work

# Partager au-delà de localhost
rmux web-share --tunnel-provider localhost-run
```

Utilisez un tunnel provider, apportez votre propre ingress, ou hébergez le frontend statique sur votre propre domaine.

Points d'entrée utiles :

- [Vue d'ensemble Web Share du dépôt](../web-share.md)
- [Documentation Web Share](https://rmux.io/docs/web-share/)
- [Modèle de sécurité](https://rmux.io/docs/web-share/#/security)
- [Tunnel providers](https://rmux.io/docs/web-share/#/tunnels)

<a id="scripting-api"></a>

## 🧰 Scripts & API

Les SDK se connectent au daemon RMUX local et exposent sessions, panes,
streams, waits et snapshots pour l'automatisation.

```sh
cargo add rmux-sdk
pip install librmux
npm install @rmux/sdk
```

- SDK Rust : [`rmux-sdk`](https://crates.io/crates/rmux-sdk)
- SDK Python : [`librmux`](https://pypi.org/project/librmux/)
- SDK TypeScript : [`@rmux/sdk`](https://www.npmjs.com/package/@rmux/sdk)

<a id="documentation"></a>

## 📚 Documentation

La documentation complète de RMUX est disponible sur [rmux.io/docs](https://rmux.io/docs/).

Elle inclut :

- [Guides d'installation](https://rmux.io/docs/get-started/)
- [Référence CLI](https://rmux.io/docs/cli/)
- [Exemples](https://rmux.io/docs/examples/)
- [Référence API](https://rmux.io/docs/api/)
- [Vue d'ensemble SDK du dépôt](../scripting-sdk.md)
- [Web Share](https://rmux.io/docs/web-share/)

Pour un profil ergonomique orienté humain qui conserve une sélection terminal native intuitive tout en ajoutant des raccourcis de splits et une intégration presse-papiers plus simples, voir [docs/human-friendly-config.md](../human-friendly-config.md).

## 🧩 Widget Ratatui

```rust
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};
use ratatui_rmux::{PaneState, PaneWidget};
use rmux_sdk::PaneSnapshot;

fn render(snapshot: PaneSnapshot, area: Rect, buffer: &mut Buffer) {
    let state = PaneState::from_snapshot(snapshot);
    PaneWidget::new(&state).render(area, buffer);
}
```

## 🏗️ Architecture

<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://rmux.io/rmux-architecture-dark.png?v=0.9.1-web-share">
  <source media="(prefers-color-scheme: light)" srcset="https://rmux.io/rmux-architecture-light.png?v=0.9.1-web-share">
  <img src="https://rmux.io/rmux-architecture-dark.png?v=0.9.1-web-share" alt="Architecture runtime RMUX" width="800">
</picture>

</div>

`rmux` garde les shells, sessions, windows, panes et processus PTY dans le daemon local. Les clients locaux utilisent l'IPC. Web Share est un accès navigateur explicite : le daemon expose un pane ou une session sélectionnée via un WebSocket chiffré de bout en bout, pendant que l'exécution reste sur votre machine.

## 🧱 Workspace

| Crate | Rôle | Publication |
| :--- | :--- | :--- |
| `rmux-types` | Types de valeurs partagés et neutres vis-à-vis des plateformes | publique |
| `rmux-proto` | DTO IPC détachés, framing, erreurs sûres sur le fil | publique |
| `rmux-os` | Petits helpers à la frontière OS | publique |
| `rmux-ipc` | Endpoints et transports IPC locaux | publique |
| `rmux-sdk` | SDK Rust adossé au daemon | publique |
| `ratatui-rmux` | Widget d'intégration Ratatui | publique |
| `rmux-web-crypto` | Coeur E2EE Web Share et frontière crypto WASM | publique |
| `rmux-pty` | Allocation PTY, resize et contrôle de processus enfant | crate de support |
| `rmux-core` | Sessions, panes, layouts, formats, hooks, buffers | crate de support |
| `rmux-server` | Daemon Tokio et dispatch des requêtes | crate de support |
| `rmux-client` | Client IPC local et plomberie du mode attach | crate de support |
| `rmux` | CLI et point d'entrée daemon masqué | binaire public |
| `rmux-render-core` | Coeur de rendu snapshot partagé | interne au workspace |

<a id="platform-support"></a>

## 🖥️ Plateformes

| Plateforme | Backend PTY | Backend IPC | Endpoint par défaut |
| :--- | :--- | :--- | :--- |
| Linux | PTY Unix | Socket Unix | `/tmp/rmux-{uid}/default` |
| macOS | PTY Unix | Socket Unix | `/tmp/rmux-{uid}/default` |
| Windows | ConPTY | Named pipe | named pipe par utilisateur |

## 🧾 Notes de compatibilité terminal

RMUX fonctionne avec les shells qui interrogent les capacités du terminal, notamment fish. Il répond aux requêtes d'attributs de terminal et gère le timing de la touche Escape afin que les prompts fish et les séquences de touches se comportent normalement dans les panes RMUX.

Le passthrough graphique est disponible pour les terminaux externes qui supportent Kitty graphics ou SIXEL. RMUX détecte Kitty graphics pour Kitty, Ghostty et WezTerm, et détecte SIXEL pour des terminaux comme foot, mintty, mlterm et WezTerm. Il est opt-in :

```tmux
set -g allow-passthrough on
```

La valeur tmux `all` est acceptée pour la compatibilité de configuration. RMUX rend le pane attaché ; `all` se comporte donc actuellement comme `on` plutôt que d'ajouter le passthrough pour les panes non attachés.

Si votre terminal supporte l'un de ces protocoles mais n'est pas détecté automatiquement, ajoutez une override de fonctionnalité terminal :

```tmux
set -as terminal-features 'xterm-kitty:kitty-graphics'
set -as terminal-features 'xterm*:sixel'
```

Le passthrough SIXEL est couvert par la suite de régression automatisée Unix PTY attach. Sur Windows, RMUX active le passthrough ConPTY moderne quand l'OS le supporte, mais l'affichage SIXEL dépend toujours du terminal externe. Définissez `RMUX_CONPTY_NO_PASSTHROUGH=1` pour désactiver ce mode backend lors d'un diagnostic.

<a id="verification"></a>

## 🧪 Vérification

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

`#![forbid(unsafe_code)]` est utilisé dans les crates de haut niveau. Le code lié à l'OS et au terminal est isolé dans les crates runtime de plus bas niveau.

## ⚖️ Licence

RMUX est distribué sous double licence, au choix :

- [Licence MIT](../../LICENSE-MIT)
- [Licence Apache 2.0](../../LICENSE-APACHE)
