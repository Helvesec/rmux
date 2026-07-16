# rmux-os

Small OS abstraction layer used by [RMUX](https://github.com/Helvesec/rmux) IPC and terminal integrations.

A thin, dependency-light wrapper around the OS-specific bits the RMUX
runtime needs at its boundary: host introspection, process identity, user
identity, and terminal queries. Linux, macOS, and Windows are all
first-class — Windows uses native Win32 APIs rather than WSL.

## Surface

- `host` — OS host information.
- `identity` — process and user identity helpers.
- `process` — process control primitives.
- `terminal` — terminal queries.

Pull it in if you need the same OS-boundary primitives the RMUX runtime
uses; most users get them through the higher-level crates and never need
to depend on `rmux-os` directly.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
