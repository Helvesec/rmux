# rmux-pty

> **Private API.** Implementation detail of [`rmux`](https://github.com/Helvesec/rmux).
> Semver is not guaranteed inside `0.x` — versions may break at any point.
> If you want a stable Rust API, depend on [`rmux-sdk`](https://crates.io/crates/rmux-sdk) instead.

PTY allocation, resize, and child-process control used by the
[RMUX](https://github.com/Helvesec/rmux) daemon. Native Unix PTYs on Linux
and macOS, native ConPTY on Windows — together with named pipes for local
IPC (handled by [`rmux-ipc`](https://crates.io/crates/rmux-ipc)), this is
how RMUX runs first-class on Windows without WSL.

Published to crates.io for RMUX workspace consumers and release builds; not
intended as a stable consumer surface.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
