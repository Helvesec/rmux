# Roadmap

This roadmap covers the next year of RMUX work. It is not a promise that every
item will land in a specific release.

## Completed Foundations

- Native Linux, macOS, and Windows terminal backends.
- A tmux-compatible CLI surface for common session, window, pane, copy-mode, and
  configuration workflows.
- A daemon-backed Rust SDK and Ratatui widget crate.
- Web Share with local execution, a static browser frontend, and hybrid
  post-quantum end-to-end encryption.
- Release and package-manager support for crates.io, GitHub Releases, APT, DNF,
  Homebrew, Scoop, WinGet, and Chocolatey.

## Current Priorities

1. **tmux compatibility**: continue closing behavior differences in command
   parsing, configuration loading, hooks, formats, copy mode, mouse handling,
   terminal features, and plugin-oriented workflows.
2. **Web Share hardening**: keep tightening origin checks, connection limits,
   role enforcement, error behavior, and browser/daemon protocol tests.
3. **Supply-chain assurance**: improve direct-download authenticity, release
   provenance, reproducibility checks, and package-manager validation.
4. **SDK stability**: grow typed automation APIs without breaking existing
   public crates, and keep the Python SDK aligned with the Rust SDK.
5. **Documentation**: keep the repository docs, rmux.io docs, examples, and
   package-manager instructions consistent with the current release.
6. **External review**: pursue independent review for the Web Share crypto and
   release process when funding and reviewer availability make it practical.

## Out of Scope

- Running shells in a hosted cloud terminal service.
- Requiring WSL for Windows support.
- Replacing the user's shell, editor, terminal emulator, or operating system
  package manager.
