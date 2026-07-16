# rmux-types

Shared platform-neutral value types for the [RMUX](https://github.com/Helvesec/rmux) terminal multiplexer workspace.

This crate holds the small, dependency-free primitives used across the RMUX
workspace — terminal sizes and other low-level value types that need to be
present in both the public SDK surface and the internal runtime crates
without dragging extra dependencies through the dependency graph.

## Surface

- `TerminalSize { cols: u16, rows: u16 }` and associated constructors.

Most users of RMUX do not depend on `rmux-types` directly — these primitives
are re-exported from [`rmux-sdk`](https://crates.io/crates/rmux-sdk) and
[`rmux-proto`](https://crates.io/crates/rmux-proto). Pull the type from
whichever public surface you already use.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
