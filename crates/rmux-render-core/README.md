# rmux-render-core

> **Early API.** Workspace-internal building block of
> [RMUX](https://github.com/Helvesec/rmux); the surface is not yet stabilised
> and may change inside `0.x`. For a stable rendering path, depend on
> [`ratatui-rmux`](https://crates.io/crates/ratatui-rmux) instead.

Pure-data RMUX pane snapshot and ratatui rendering core. Holds the captured
pane snapshot types (`PaneSnapshot`, `PaneCell`, `PaneColor`, `PaneCursor`,
`PaneGlyph`, `PaneAttributes`) plus the deterministic ratatui projection
(`PaneWidget`, `PaneState`).

No daemon, no IPC, no process, no filesystem, no network, no Tokio. The
crate compiles for `wasm32-unknown-unknown`, which makes it usable in
browsers and other restricted hosts that just want to render captured
RMUX pane data.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
