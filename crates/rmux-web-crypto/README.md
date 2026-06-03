# rmux-web-crypto

Single-crate crypto core for rmux web-share E2EE.

It provides:

- ephemeral **X25519** key generation and Diffie-Hellman;
- **ML-KEM-768** wrappers for the post-quantum hybrid shared secret;
- deriving a session from a PSK, an X25519 DH shared secret, an ML-KEM shared
  secret, and the exact handshake transcript bytes;
- authenticated ChaCha20-Poly1305 records with monotonic sequence numbers;
- web-share **text/binary "kind byte"** framing on top of opaque records;
- browser WASM bindings behind `--features wasm`.

It has no knowledge of WebSockets, TCP, JSON, or HTTP — those live in the
rmux-server web module — and therefore does not depend on `rmux-server` (no
circular dependency).

Forward secrecy comes from per-connection X25519 and ML-KEM secrets.
Authentication comes from the PSK mixed into the key schedule. The PSK must be
high-entropy: rmux uses `SHA-256(256-bit token)`.

Native daemon builds use the default `x25519` feature and link the `rlib`.
Browser builds use:

```sh
wasm-pack build crates/rmux-web-crypto --release --target web \
  --no-default-features --features wasm \
  --out-name rmux_web_crypto_wasm
```
