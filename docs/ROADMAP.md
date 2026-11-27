# Roadmap

This roadmap describes planned RMUX work. It is not a promise that every
item will land in a specific release.

## Completed Foundations

- Native Linux, macOS, and Windows terminal backends.
- A tmux-compatible CLI surface for common session, window, pane, copy-mode, and
  configuration workflows.
- A daemon-backed Rust SDK and Ratatui widget crate.
- Web Share with local execution, a static browser frontend, and hybrid
  post-quantum end-to-end encryption.
- Release and package-manager support for GitHub Releases, APT, DNF, Homebrew,
  Scoop, WinGet, Chocolatey, and public SDK crates on crates.io.

## Current Priorities

1. **tmux compatibility**: continue closing behavior differences in command
   parsing, configuration loading, hooks, formats, copy mode, mouse handling,
   terminal features, and plugin-oriented workflows.
2. **Web Share hardening**: keep tightening origin checks, connection limits,
   role enforcement, error behavior, and browser/daemon protocol tests.
3. **Supply-chain assurance**: improve direct-download authenticity, release
   provenance, reproducibility checks, and package-manager validation.
4. **SDK stability**: grow typed automation APIs without breaking existing
   public Rust crates within their accepted support boundaries.
5. **Documentation**: keep the repository docs, rmux.io docs, examples, and
   package-manager instructions consistent with the current release.
6. **External review**: pursue independent review for the Web Share crypto and
   release process when funding and reviewer availability make it practical.

## Post-1.0 Python/TypeScript SDK qualification

This explicit milestone follows RMUX 1.0; no date or minor version is assigned.
Python `librmux` and TypeScript `@rmux/sdk` are excluded from new RMUX 1.0
artifacts, bundled implementations and positive SDK support/build/package
manifests. Historical 0.6 publications and aliases remain identifiable.

Before making a new support promise, assign package owners and approve the
supported surface mapped to the common schema. Require exact source/package/RMUX
bindings, installed consumers, API and migration baselines, lifecycle and
platform evidence, release ownership and independent review. See the
[scope and qualification obligations](scripting-sdk.md#additional-language-sdk-scope).

## Out of Scope

- Running shells in a hosted cloud terminal service.
- Requiring WSL for Windows support.
- Replacing the user's shell, editor, terminal emulator, or operating system
  package manager.
