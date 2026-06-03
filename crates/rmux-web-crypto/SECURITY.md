# Security model - rmux web-share E2EE v4

This document describes the threat model for web-share end-to-end encryption.

## Threat model

A web-share session is relayed through an **untrusted relay** (`share.rmux.io`
behind a Cloudflare tunnel). TLS protects the wire against network attackers.
The E2EE layer additionally hides terminal content from the **relay itself** and
provides forward secrecy beyond the relay TLS connection.

- **Hybrid key agreement.** Each connection combines an ephemeral X25519 shared
  secret with an ML-KEM-768 shared secret. The key schedule mixes both values
  with the high-entropy token secret:
  `ikm = x25519_dh || ml_kem_shared_secret || SHA256(token)`.
- **Forward secrecy.** A relay that records the TLS-decrypted ciphertext and
  later obtains the share token cannot decrypt past traffic because the
  per-connection X25519 and ML-KEM secrets have been discarded. In v3, keys were
  derived from the token alone, so a later token leak could retroactively decrypt
  recorded traffic.
- **Post-quantum defense.** The channel is hybrid by construction, not
  X25519-only. Recorded traffic remains protected against a future X25519 break
  as long as ML-KEM-768 remains secure and the token secret was not compromised
  during the session.
- **Authentication.** The 256-bit share token is mixed into the key schedule
  after hashing. A relay performing its own X25519 and ML-KEM exchanges with
  each side still cannot derive working keys without the token, so it cannot
  MITM or read content. The token is never sent on the wire; only `token_id`, a
  truncated hash, and proof-of-knowledge through the first encrypted frame are
  exposed.
- **PIN checks.** The PIN is a secondary factor checked after the
  token-authenticated handshake. It is never fed into the KDF; doing so would let
  a relay brute-force a low-entropy secret offline. Wrong PIN attempts trigger
  exponential backoff (`registry.rs`), so the PIN only gates a session whose
  256-bit token is already known.

## Hybrid construction

The construction is Noise-style PSK + ephemeral-ephemeral with an ML-KEM-768
hybrid secret mixed into the same HKDF input. The record layer never relies on
ML-KEM alone and never relies on X25519 alone; the channel remains protected as
long as either key-agreement primitive remains secure and the token secret is not
known to the attacker.

In browsers, X25519 is deliberately kept in WebCrypto instead of being moved
into WASM. WebCrypto can generate the ephemeral X25519 private key as a
**non-extractable** key, so an XSS payload cannot directly export that private
key. ML-KEM runs in WASM through `libcrux-ml-kem`, with entropy supplied by the
browser.

`deriveBits` still exposes the resulting *shared secret* to the JS heap before
it is passed to the WASM record layer; only the *private key* remains
non-extractable. This improves browser private-key handling compared with a full
Noise-in-WASM implementation, but it does not provide a complete memory
isolation boundary.

## Browser compromise scope

A non-extractable key is not an absolute barrier. An XSS on the share page can
still read the share token from the URL fragment (`#t=...`) or drive the page
directly. E2EE protects the **relay path**, not a fully compromised page. Treat
the share page's origin and CSP as part of the trust boundary.

## Close-code policy

Every pre-ready handshake rejection returns a single opaque wire close code,
`(4000, "handshake_rejected")`; the precise reason is logged server-side only.
This removes an oracle: distinguishable codes would otherwise reveal token
validity or, because capacity is checked only after a correct PIN, PIN
correctness. The UI therefore treats pre-ready failures as a generic connection
rejection. Post-ready operational codes (slow viewer, normal close, invalid
client command) are unaffected.

## Transcript binding

The client hello carries the ML-KEM encapsulation key; the server challenge
carries the ML-KEM ciphertext. The exact wire bytes of both messages are hashed
into the HKDF salt, so capability stripping, key substitution, ciphertext
substitution, and other transcript changes derive different traffic keys.

## Zeroization

Native ephemeral X25519 secrets are zeroized on drop (x25519-dalek `zeroize`
feature); derived key material is zeroized after ciphers are built. In the
browser, the X25519 private key is a non-extractable WebCrypto key and is not
stored in WASM memory. JS/WASM runtimes provide weaker memory-wiping guarantees
than native processes, so browser-side secrets should not be assumed to be fully
scrubbed.

## Status

The Rust core (`rmux-web-crypto`) compiles and tests natively and to `wasm32`.
Release validation covers the browser build pipeline (wasm-pack/wasm-bindgen),
bundle budget checks, Playwright e2e coverage, and native↔WASM interop in a full
build environment.
