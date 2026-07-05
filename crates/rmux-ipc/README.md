# rmux-ipc

Local IPC endpoint and transport primitives for the
[RMUX](https://github.com/Helvesec/rmux) terminal multiplexer.

Resolves where the RMUX daemon listens (Unix domain socket on Linux and
macOS, named pipe on Windows) and provides the listener / stream types the
daemon and clients use. Strictly local — no network listener is ever
opened.

## Surface

- `LocalEndpoint` — platform-neutral endpoint descriptor.
- `default_endpoint`, `endpoint_for_label`, `resolve_endpoint` — discover where to talk to the daemon.
- `LocalListener` — bind a daemon socket / named pipe.
- Stream types — async I/O bridges used by the daemon and the SDK.

Most callers reach `rmux-ipc` through [`rmux-sdk`](https://crates.io/crates/rmux-sdk)
rather than depending on it directly.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
