# rmux-client

> **Private API.** Implementation detail of [`rmux`](https://github.com/Helvesec/rmux).
> Semver is not guaranteed inside `0.x` — versions may break at any point.
> If you want a stable Rust API, depend on [`rmux-sdk`](https://crates.io/crates/rmux-sdk) instead.

Blocking local client and attach-mode plumbing used by the
[RMUX](https://github.com/Helvesec/rmux) CLI to talk to a running daemon
and to drive the interactive attach loop. Published to crates.io for RMUX
workspace consumers and release builds; not intended as a stable consumer
surface.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
