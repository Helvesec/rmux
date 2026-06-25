# rmux web-share E2EE — Protocol v1

**Status:** Normative specification for cryptographic audit (OSTIF / OTF).
**Source:** `release/0.6.5` implementation tree at commit `2ecd58a3b261977de315abd732b3ff8e58befc83`. File and line references are audit breadcrumbs for that tree; if code moves later, resolve citations by symbol and commit history.
**Wire protocol version:** `1` (`WEB_SHARE_PROTOCOL_VERSION: u16 = 1`, `crates/rmux-server/src/web/protocol/mod.rs:45`).

---

## 1. Scope & Status

### 1.1 Purpose

This document specifies the rmux **web-share end-to-end-encrypted browser-terminal protocol, version 1** ("the protocol"). It defines the exact wire formats, cryptographic key schedule, transcript binding, record layer, authentication model, connection lifecycle, and error/close-code behavior, so that an independent auditor can check the implementation against the specification byte-for-byte.

### 1.2 What the protocol provides

End-to-end confidentiality and integrity of terminal payloads between a browser client and the local rmux daemon ("host"), over a WebSocket carried through an **untrusted relay** (a Cloudflare tunnel terminating TLS at `share.rmux.io`). The relay observes only ciphertext. Confidentiality is **hybrid** (post-quantum-resistant): it holds as long as the 256-bit share token remains secret AND at least one of X25519 / ML-KEM-768 remains unbroken.

### 1.3 Implementation surface this spec covers

- Crypto core crate `rmux-web-crypto` (native + WASM): `schedule.rs`, `record.rs`, `transcript.rs`, `ml_kem.rs`, `x25519.rs`, `framing.rs`, `session.rs`, `wasm.rs`, `error.rs`, plus `tests/kat.rs`.
- Daemon web protocol half `crates/rmux-server/src/web/*`: `crypto.rs`, `secrets.rs`, `pairing.rs`, `registry.rs`, `backoff.rs`, `record.rs`, `server.rs`, `origin.rs`, `protocol/{mod,handshake}.rs`, `server/{streams,http,pre_auth,rate_limit}.rs`, `websocket.rs`, `connection_limit.rs`.

### 1.4 Version authority

The authoritative wire value is the integer `protocol_version = 1` (`protocol/mod.rs:45`), confirmed by the byte-stable serialization test `challenge_serialization_is_wire_stable` (`handshake.rs:158-166`) which emits `protocol_version:1`. The module-level protocol docs, HKDF/transcript labels, and JSON protocol version are aligned on v1 in the cited tree. The string "v1" in the HKDF/transcript labels is part of the wire contract and MUST NOT change without a version bump.

---

## 2. Conventions & Terminology

### 2.1 Requirement keywords

The keywords **MUST**, **MUST NOT**, **SHOULD**, **SHOULD NOT**, and **MAY** are to be interpreted as described in RFC 2119.

### 2.2 Notation

- `||` denotes byte concatenation.
- `le_u64(x)` is `x` encoded as an unsigned 64-bit little-endian integer (`x.to_le_bytes()`).
- `be_u64(x)` is `x` encoded as an unsigned 64-bit big-endian integer (`x.to_be_bytes()`).
- `base64url(b)` is RFC 4648 §5 base64url **without padding** (Rust `base64::engine::general_purpose::URL_SAFE_NO_PAD`).
- `SHA256(x)` is the 32-byte SHA-256 digest of `x`.
- Byte literals are hexadecimal (e.g. `0xE0`). ASCII string literals are shown in double quotes; their byte length is given explicitly where load-bearing.

### 2.3 Terminology

| Term | Meaning |
|---|---|
| **client** | The browser side (JS + WASM `rmux-web-crypto`). |
| **server / host** | The local rmux daemon. |
| **relay** | The untrusted transport (tunnel / reverse proxy). |
| **token** | A 256-bit share secret, base64url-no-pad of 32 random bytes; never on the wire. |
| **PSK** | `SHA256(token_string_bytes)`, 32 bytes; mixed into the key schedule. |
| **token_id** | A 128-bit non-secret lookup handle derived from the PSK; sent in the hello. |
| **PIN / pairing code** | An optional 6-digit secondary human factor; never in the KDF. |
| **c2s / s2c** | Direction labels: client-to-server / server-to-client. |
| **record** | One encrypted frame on the wire (one WebSocket binary message). |

### 2.4 Transport framing

All handshake messages are WebSocket **Text** frames carrying UTF-8 JSON. After the handshake all frames are WebSocket **Binary** frames each carrying exactly one encrypted record (§9). A plaintext Text frame received after the handshake is a hard error (`crypto.rs:172-176`).

---

## 3. Cryptographic Primitives

| Primitive | Algorithm | Parameters / sizes |
|---|---|---|
| Key agreement (classical) | X25519 (RFC 7748) | 32-byte public key, 32-byte DH shared secret (`x25519.rs:26-41`) |
| Key agreement (PQ) | ML-KEM-768 (FIPS 203) via `libcrux-ml-kem` | enc. key 1184 B, ciphertext 1088 B, shared secret 32 B; keygen randomness 64 B; encaps randomness 32 B (`ml_kem.rs:16-24`) |
| AEAD | ChaCha20-Poly1305 (IETF, RFC 8439) | 256-bit key, 96-bit (12-byte) nonce, 128-bit (16-byte) tag (`record.rs:21-32`) |
| KDF | HKDF-SHA256 (RFC 5869) | Extract+Expand (`schedule.rs:87,96-103`) |
| Hash | SHA-256 | 32-byte output (`transcript.rs:8`, `secrets.rs:52-57`) |

### 3.1 X25519

The client uses a fresh per-connection ephemeral X25519 keypair; the server uses a fresh per-connection ephemeral X25519 keypair. The DH shared secret is the raw 32-byte RFC 7748 little-endian u-coordinate output (`Ephemeral::into_shared_secret`). The X25519 agreement step itself does **not** reject low-order points; the all-zero DH result is rejected later in the key schedule (§7.3). Native ephemeral secrets are zeroized on drop (x25519-dalek `zeroize`).

### 3.2 ML-KEM-768 (FIPS 203)

Keypair generation MUST be seeded with exactly 64 bytes of caller-supplied randomness (`KEYGEN_RANDOMNESS_LEN = 64`); no internal RNG / `getrandom` is compiled into the WASM bundle. Encapsulation MUST validate the encapsulation key per FIPS 203 §7.2 (modulus check); an invalid key MUST cause `encapsulate` to return `None`, and the caller MUST fail closed (`ml_kem.rs:60-78`, `crypto.rs:141-158`). The ML-KEM shared secret MUST NOT be value-checked: ML-KEM-768 uses the Fujisaki-Okamoto implicit-rejection transform, so an invalid ciphertext yields a pseudorandom (deterministic, secret-key-bound) shared secret rather than a distinguished failure.

### 3.3 ChaCha20-Poly1305

The IETF construction with a 96-bit nonce and 128-bit tag (§9.2). Keys and nonce prefixes are per-direction (§7.5).

### 3.4 HKDF-SHA256

Standard RFC 5869 Extract-then-Expand. HKDF-Extract internals are delegated to the RustCrypto `hkdf` crate. Verification coverage is strong and independent: `tests/kat.rs` contains a passing RFC 5869 HKDF-SHA256 known-answer test (`rfc5869_hkdf_sha256_test_case_1`), and the in-tree test `key_schedule_matches_independent_hkdf_spec` (`schedule.rs:199-232`) reconstructs the full Extract+Expand flow independently and asserts equality with `derive`. Both pass.

### 3.5 SHA-256

Used for: the transcript hash (§8), the PSK derivation (`SHA256(token_string)`), and the `token_id` derivation (§10.3).

---

## 4. Roles & Trust Model

### 4.1 In-scope adversaries

An implementation MUST preserve confidentiality and integrity of terminal payloads against all of the following (`docs/assurance-case.md:21-39`, `crates/rmux-web-crypto/SECURITY.md:7-30`):

1. **Passive network observer.**
2. **Tunnel provider / relay.** Terminates TLS; sees and forwards all WebSocket traffic; MUST NOT be able to read or silently modify terminal plaintext. This is the primary threat the E2EE layer mitigates.
3. **Active network attacker.** Injects, modifies, drops, replays, reorders, truncates, or oversizes frames; MUST be rejected by the record/handshake layer.
4. **MitM, including a relay running its own X25519/ML-KEM exchanges.** Without the share token (PSK) it derives different traffic keys and the first encrypted frame fails to open.
5. **Downgrade / transcript manipulation.** Capability stripping, version downgrade, public-key or ML-KEM ciphertext/encapsulation-key substitution are detected via transcript binding (§8).
6. **Post-quantum "harvest now, decrypt later".** Recorded ciphertext stays protected against a future X25519 break because the channel is hybrid (§7).

### 4.2 Out-of-scope (explicit non-goals)

An auditor MUST NOT treat these as protocol defects (`docs/assurance-case.md:21-39`, `SECURITY.md:57-62`):

1. A compromised local user account / the user's own machine (it can read its own terminal).
2. A compromised host OS, malware, keyloggers, malicious browser extensions, or a compromised browser.
3. A tampered / malicious frontend loaded from an untrusted origin; the loaded page is **trusted** (part of the TCB). Self-hosting the static frontend is the remedy for owning that boundary.
4. Denial of service / resource exhaustion against a user-exposed endpoint.
5. Theoretical breaks of X25519, ML-KEM-768, HKDF-SHA256, or ChaCha20-Poly1305 without a practical attack.

### 4.3 Trust boundaries

1. **Local-execution boundary.** All terminal state (PTYs, panes, sessions, scrollback) stays in the local daemon.
2. **Local IPC boundary.** Local clients reach the daemon via owner-scoped Unix sockets / per-user Windows named pipes only.
3. **Web-share transport boundary (untrusted).** Tunnels/relays carry WebSocket traffic but are not trusted with plaintext.
4. **Frontend boundary (trusted; part of TCB).** The share page's origin and CSP are part of the trust boundary, not protected against.
5. **Release / delivery boundary.** Built from tagged source; archives publish SHA-256 checksums; APT/RPM repos are signed.

### 4.4 JS↔WASM cryptographic boundary contract (browser side)

X25519 is kept in **WebCrypto** (browser-owned); ML-KEM runs in **WASM** (`libcrux-ml-kem`). The boundary contract (`wasm.rs:14-50,82-103,193-236`, `SECURITY.md:45-55`):

**X25519 (WebCrypto):**
- The ephemeral X25519 private key MUST be generated as a **non-extractable** WebCrypto key.
- Only the **32-byte DH shared secret** crosses into WASM, as the `dh` argument to `ClientSession::new`; it MUST be exactly 32 bytes else rejected (`"invalid X25519 shared secret length"`). The private key never enters WASM. The 32-byte DH value is wrapped in `Zeroizing` inside WASM.

**ML-KEM-768 (WASM):**
- The **decapsulation (secret) key never leaves WASM** (held inside `MlKemKeyPair`, never serialized to JS).
- `MlKemKeyPair::new` MUST be seeded with exactly 64 bytes from `crypto.getRandomValues`; non-64-byte input rejected (`"ml-kem keygen randomness must be 64 bytes"`).
- `encapsulationKey()` returns the 1184-byte public key for the hello.
- `decapsulate(ciphertext)` requires exactly 1088 bytes (else `"ml-kem ciphertext must be 1088 bytes"`) and returns the 32-byte shared secret.
- The server-side `mlKemEncapsulate` exists only under the `wasm-test` feature (browser test mocks); production browsers use `ClientSession` only.

**PSK (token):**
- The `psk` into `ClientSession::new` MUST be `SHA256(token)`, 32 bytes, computed in the browser. The token lives only in the URL fragment (`https://share.rmux.io/#t=...`) and MUST NOT be transmitted.

**Session entry point:** `ClientSession::new(psk, dh, ml_kem_secret, client_hello, server_challenge)` (§7). The WASM record API exposes only `sealText` / `sealBinary` / `open`; `Opened` carries exactly one of text/binary (via `isText`); all WASM crypto errors collapse to the single opaque JS string `"crypto operation failed"` (no error oracle to JS).

> **Auditor note (browser frontend absent from this checkout).** The JS frontend (WebCrypto non-extractable X25519, `deriveBits`, `SHA256(token_string)` PSK, `getRandomValues(64)`) is not in this repository. The contract above is confirmed from the WASM signatures and doc-comments only. An auditor MUST independently confirm, against the deployed frontend, that the browser: (a) hashes the token **string** (ASCII base64url characters), not the decoded bytes, supplying exactly 32 bytes; (b) emits base64url-no-pad for all generated fields; (c) stores and passes the literal sent-hello and received-challenge byte buffers (no re-serialization). See Appendix B.

---

## 5. Protocol Overview

```
  browser client                relay (untrusted)              rmux daemon (host)
  ───────────────                ─────────────────              ──────────────────
        │                                │                                │
        │ WS Text: client_hello (JSON) ─►│ ─────────────────────────────► │  parse, lookup token_id,
        │                                │                                │  ML-KEM encapsulate, X25519 DH,
        │ ◄──────── WS Text: server_challenge (JSON) ◄────────────────────│  derive session keys
        │                                │                                │
        │ WS Binary: encrypted auth ───►│ ─────────────────────────────► │  open auth frame (proves token),
        │   frame {type:"auth",...}      │                                │  PIN+backoff+capacity checks
        │                                │                                │
        │ ◄──────── WS Binary: encrypted "ready" ◄────────────────────────│  session established
        │                                │                                │
        │ ◄═══════ encrypted records (terminal I/O, both directions) ═════►│
```

The relay sees only the two JSON handshake messages (which carry public ephemeral material) and opaque ciphertext records. It cannot derive the session keys without the token.

---

## 6. Handshake Messages

### 6.0 Common encoding rules

- Handshake messages MUST be WebSocket **Text** frames containing UTF-8 JSON. The server rejects a non-Text hello (`hello_must_be_text`). The challenge is sent via `socket.write_text(...)` (`server.rs:375`).
- All binary fields inside the JSON (X25519 public key, ML-KEM encapsulation key, ML-KEM ciphertext, nonces, token_id) MUST be `base64url`-no-pad. Server encoders use `URL_SAFE_NO_PAD` (`crypto.rs:52,60`); server decoders use `URL_SAFE_NO_PAD` (decode sites `crypto.rs:119,236,246`; `secrets.rs:73,89`) and MUST reject standard padded base64.
- `protocol_version` MUST be the integer `1`.
- Capability label strings: E2EE = `"e2ee-token-auth"` (`crypto.rs:10`); pane-frame = `"pane-frame-v1"` (`protocol/mod.rs:55`); palette = `"terminal-palette-v1"` (`protocol/mod.rs:58-60`).

### 6.1 client_hello (client → server)

Parsed by `serde_json::from_str::<ClientHelloWire>` (`crypto.rs:88`). `ClientHelloWire` is `#[serde(deny_unknown_fields)]` (`crypto.rs:256-267`): any unknown JSON field MUST cause a parse failure (collapsed to `HANDSHAKE_REJECTED`).

| JSON field | Type | Constraint | Citation |
|---|---|---|---|
| `type` | string | MUST equal `"hello"` (serde renames to field `kind`) | `crypto.rs:89,259-260` |
| `protocol_version` | integer (`u16`) | MUST equal `1` | `crypto.rs:89` |
| `capabilities` | array of string | MUST contain `"e2ee-token-auth"` (any order; extra entries allowed) | `crypto.rs:92-98` |
| `token_id` | string | base64url-no-pad decoding to exactly 16 bytes | `crypto.rs:99`, `secrets.rs:72-76` |
| `client_nonce` | string | base64url-no-pad decoding to exactly 16 bytes | `crypto.rs:100,235-243` |
| `client_public` | string | base64url-no-pad decoding to exactly 32 bytes (raw X25519 public key) | `crypto.rs:104,245-250` |
| `client_ml_kem_ek` | string | base64url-no-pad decoding to exactly 1184 bytes (ML-KEM-768 encapsulation key) | `crypto.rs:105,118-126`, `ml_kem.rs:20` |

All fields are REQUIRED (no defaults).

**Validation order** (`parse_client_hello`, `crypto.rs:87-113`):
1. JSON deserialize with `deny_unknown_fields`; failure → reject.
2. `kind != "hello"` OR `protocol_version != 1` → reject.
3. `capabilities` lacks `"e2ee-token-auth"` → reject.
4. `token_id` not base64url-no-pad of 16 bytes → reject.
5. `client_nonce` not base64url-no-pad of exactly 16 bytes → reject.
6. `client_public` not base64url-no-pad of exactly 32 bytes → reject.
7. `client_ml_kem_ek` not base64url-no-pad of exactly 1184 bytes → reject. ML-KEM structural/modulus validity (FIPS 203 §7.2) is **not** checked here; it is checked at encapsulation time (§6.2 step 2).

**Notes:**
- The server preserves the **exact raw hello text** (untouched UTF-8 bytes, not a re-serialization) in `ClientHello.raw` (`crypto.rs:26,111`); these exact bytes are bound into the key schedule transcript (`server.rs:362`).
- `client_nonce` is parsed and length-checked (`decode_nonce`, `crypto.rs:235-243`) and stored (`crypto.rs:107`), but is **not** fed into HKDF directly: only `hello.raw` is passed to `derive_server_crypto` (`server.rs:362`). The `client_nonce` is therefore bound only transitively, via the raw-hello transcript bytes.
- JSON field **order** in the hello is not enforced (serde reads fields by name). The transcript binds whatever raw bytes the client sent (`crypto.rs:111`, `server.rs:362`), so the browser's actual field order is bound as-is. An auditor confirming the deployed frontend should note this is a non-issue server-side.

### 6.2 server_challenge (server → client)

Built by `build_challenge` → `ServerHandshakeMessage::Challenge` serialized with `serde_json::to_string` (`handshake.rs:18-28,50-63`). The enum uses `#[serde(tag = "type", rename_all = "snake_case")]`, so `type = "challenge"`.

| JSON field | Type | Value / encoding | Citation |
|---|---|---|---|
| `type` | string | exactly `"challenge"` | `handshake.rs:19,21` |
| `protocol_version` | integer (`u16`) | exactly `1` | `handshake.rs:56` |
| `capabilities` | array of string | exactly `["e2ee-token-auth","terminal-palette-v1","pane-frame-v1"]`, in this order | `protocol/mod.rs:56-60`, `handshake.rs:57` |
| `server_nonce` | string | base64url-no-pad of 16 random bytes | `crypto.rs:55-61`, `server.rs:345` |
| `server_public` | string | base64url-no-pad of the 32-byte raw X25519 public key | `server.rs:336,350`, `x25519.rs:26-28` |
| `server_ml_kem_ct` | string | base64url-no-pad of exactly 1088 bytes (ML-KEM-768 ciphertext) | `server.rs:340,351`, `ml_kem.rs:22` |

Field order is exactly as serialized (struct declaration order). The exact serialized form is asserted byte-for-byte by the passing test `challenge_serialization_is_wire_stable` (`handshake.rs:164-172`):

```
{"type":"challenge","protocol_version":1,"capabilities":["e2ee-token-auth","terminal-palette-v1","pane-frame-v1"],"server_nonce":"…","server_public":"…","server_ml_kem_ct":"…"}
```

**Server challenge construction sequence** (`server.rs:335-375`):
1. Generate ephemeral server X25519 keypair; `server_public = public_bytes()` (32 B).
2. ML-KEM encapsulate to `client_ml_kem_ek` (`try_from`'d to `&[u8;1184]`). FIPS 203 §7.2 validity is checked inside `ml_kem::encapsulate`; invalid → `None` → uniform rejection (`invalid_ml_kem_key`). Encaps randomness = 32 B. Output: ciphertext 1088 B + shared secret 32 B.
3. Generate `server_nonce` = base64url-no-pad of 16 random bytes.
4. Serialize the challenge text **once**; the same bytes are both bound into the key schedule (`server.rs:362-363`) and transmitted (`server.rs:375`).

### 6.3 Key-material sizes

| Quantity | Bytes | Citation |
|---|---|---|
| X25519 public key | 32 | `x25519.rs:26-28` |
| X25519 DH shared secret | 32 | `x25519.rs:38-41` |
| ML-KEM-768 encapsulation key (`client_ml_kem_ek`) | 1184 | `ml_kem.rs:20` |
| ML-KEM-768 ciphertext (`server_ml_kem_ct`) | 1088 | `ml_kem.rs:22` |
| ML-KEM-768 shared secret | 32 | `ml_kem.rs:24` |
| ML-KEM keygen randomness | 64 | `ml_kem.rs:16` |
| ML-KEM encaps randomness | 32 | `ml_kem.rs:18` |
| Handshake nonce (client/server) | 16 | `crypto.rs:55-61,239` |
| `token_id` (decoded) | 16 | `secrets.rs:68,75` |

### 6.4 Handshake-message failure handling

Every pre-ready failure (bad hello JSON, version/capability/`token_id`/nonce/key-length mismatch, unknown token, disallowed origin, invalid ML-KEM key, weak DH) collapses on the wire to the single close pair `HANDSHAKE_REJECTED = (4000, "handshake_rejected")` after a uniform delay (§11). The precise internal reason is logged server-side only, never sent (`server.rs:427`). Hello read timeout = `PRE_AUTH_TIMEOUT = 5s` (`protocol/mod.rs:41`). (The one intentional non-4000 pre-ready close, missing-PIN → `(4008, "pin_required")`, is specified in §11.2.)

---

## 7. Key Schedule

All derivation is performed by `derive(psk, dh_shared_secret, ml_kem_shared_secret, client_hello, server_challenge)` (`schedule.rs:63-109`), with the directional split in `SessionKeys::into_client` / `SessionKeys::into_server` (`schedule.rs:116-140`). Every label and the transcript domain tag are `v1`-versioned and MUST NOT change without a protocol-version bump.

### 7.1 Inputs

| Input | Type / length | Source |
|---|---|---|
| `dh_shared_secret` | exactly 32 bytes | ephemeral X25519 ECDH output (RFC 7748 raw little-endian u-coordinate) |
| `ml_kem_shared_secret` | exactly 32 bytes | ML-KEM-768 shared secret |
| `psk` | `&[u8]`, 32 bytes in production | `SecretHash::as_bytes()` = `SHA256(token_string_bytes)` (`secrets.rs:52-57`) |
| `client_hello` | exact wire bytes | `hello.raw.as_bytes()` (`server.rs:362`) |
| `server_challenge` | exact wire bytes | `challenge_text.as_bytes()` (`server.rs:363`) |

The PSK MUST be high-entropy. The PIN MUST NOT be mixed into the schedule (§10.6, §12.1).

**PSK length guarantee.** On the daemon path the 32-byte PSK length is a **type-level** guarantee, not merely a doc-comment assertion: `SecretHash` is `[u8;32]`, `as_bytes()` returns `&[u8;32]` (`secrets.rs:59-61`), and `server.rs:357` wraps `secret.as_bytes()`. The core `derive` accepts `psk: &[u8]` (variable) only so the same function serves the WASM path; the daemon never supplies a non-32-byte PSK. The **browser-supplied** PSK length is unverifiable in this checkout and remains an auditor item (§4.4, Appendix B).

### 7.2 Order of operations (`derive`)

1. Constant-time all-zero DH rejection (§7.3).
2. `transcript = transcript_hash(client_hello, server_challenge)` (§8).
3. Build IKM (§7.4).
4. `hk = HKDF-SHA256(salt = transcript, ikm)` (`schedule.rs:87`).
5. Four HKDF-Expand calls (§7.4).
6. `ikm.zeroize()` (`schedule.rs:106`).

### 7.3 Constant-time weak-secret rejection (X25519 only)

Before any derivation, `derive` MUST reject an all-zero X25519 shared secret in constant time (`schedule.rs:70-74`):

```
let zero = [0u8; 32];
if bool::from(dh_shared_secret.ct_eq(&zero)) { return Err(WeakSharedSecret); }
```

- The comparison MUST be constant-time (`subtle::ConstantTimeEq`).
- This rejects low-order-point X25519 results (RFC 7748) yielding an all-zero/predictable DH value.
- The X25519 agreement step itself does NOT reject; rejection happens only here.
- The ML-KEM shared secret is **not** zero-checked, by design (FO implicit rejection; §3.2). ML-KEM key validity is enforced earlier at encapsulate time (`None` → fail closed).

### 7.4 HKDF Extract and Expand

```
ikm  = dh_shared_secret (32)  ||  ml_kem_shared_secret (32)  ||  psk (32 in production)
salt = transcript (32, public)
PRK  = HKDF-Extract(salt, ikm)                          // HKDF-SHA256
```

- The IKM concatenation is unambiguous without length tags **only because** `dh` and `ml_kem` are fixed 32-byte prefixes and `psk` is the variable-length trailing field. Implementations MUST use exactly 32-byte DH and 32-byte ML-KEM values so the boundary `[0..32] | [32..64] | [64..]` is well-defined (`schedule.rs:79-84`).
- The IKM buffer MUST be zeroized after extract (`schedule.rs:106`).

Four independent `HKDF-Expand(PRK, info, L)` calls; each `info` is the entire label (ASCII, no surrounding structure, no NUL terminator) (`schedule.rs:16-23,96-103`):

| Output | HKDF `info` label (exact bytes) | Label len | Output len |
|---|---|---|---|
| `c2s_key` (client→server AEAD key) | `rmux web-share v1 key c2s` | 25 | 32 |
| `s2c_key` (server→client AEAD key) | `rmux web-share v1 key s2c` | 25 | 32 |
| `c2s_nonce_prefix` | `rmux web-share v1 nonce c2s` | 27 | 4 |
| `s2c_nonce_prefix` | `rmux web-share v1 nonce s2c` | 27 | 4 |

HKDF-Expand outputs are independent of call order; the labels and lengths are normative. Any expand failure MUST fail closed (`Error::KeyDerivation`). All outputs and intermediate raw key arrays MUST be zeroized once consumed (`SessionKeys` is `Zeroize + ZeroizeOnDrop`; `schedule.rs:30`).

### 7.5 Directional assignment

`SessionKeys` holds `c2s_key[32]`, `s2c_key[32]`, `c2s_nonce_prefix[4]`, `s2c_nonce_prefix[4]` (`schedule.rs:31-40`). Each side picks its sealing vs opening direction (`schedule.rs:116-140`, `session.rs:10-48`):

- **Client** (`into_client`): seals with `c2s_key` + `c2s_nonce_prefix`; opens with `s2c_key` + `s2c_nonce_prefix`.
- **Server** (`into_server`): seals with `s2c_key` + `s2c_nonce_prefix`; opens with `c2s_key` + `c2s_nonce_prefix`.

Each direction has a distinct key and a distinct 4-byte nonce prefix, so the two directions never share a (key, nonce) space.

### 7.6 Known-answer references

- Transcript hash KAT (§8.6): `c269da65fcd3ded338735b48f95fc833b8bff39146417043ae6a0aff8c90212c` (test `wire_stable_transcript_hash_vector`, passing).
- PSK KAT: token string `"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"` → `SHA256` = `ea866a757e4c38babfa8127cbe9a409d3e1f93a00ff1488ff735fcf917afffd0` (`secrets.rs:102-128`, passing).
- Independent HKDF-SHA256 reconstruction KAT: `key_schedule_matches_independent_hkdf_spec` (`schedule.rs:199-232`, passing).

---

## 8. Transcript Binding

The transcript hash cryptographically binds the **exact wire bytes** of `client_hello` then `server_challenge` into the key schedule, where it is consumed as the HKDF **salt** (`schedule.rs:77,87`). Any relay modification — version downgrade, capability stripping, nonce/public-key/ML-KEM-ciphertext substitution — changes the derived keys and is detected (fail closed).

### 8.1 Function

```rust
pub fn transcript_hash(client_hello: &[u8], server_challenge: &[u8]) -> [u8; 32]
```
(`transcript.rs:31`)

### 8.2 Hash and domain tag

SHA-256, 32-byte output (`transcript.rs:8,38`). A fixed domain-separation tag MUST be prepended:

```
DOMAIN = b"rmux web-share v1 transcript"     (28 ASCII bytes, no NUL terminator, no length prefix on the tag itself)
```
(`transcript.rs:11,33`). The 28-byte length is by character count of the literal; there is no separate length constant in code (it is the slice length of the byte-string literal).

### 8.3 Byte layout hashed

```
transcript = SHA256(
      DOMAIN                                 // 28 bytes
   || le_u64(len(client_hello))             //  8 bytes, little-endian
   || client_hello                          //  exact received wire bytes
   || le_u64(len(server_challenge))         //  8 bytes, little-endian
   || server_challenge                      //  exact serialized/sent wire bytes
)
```
(`transcript.rs:32-38`)

| Order | Field | Length | Encoding |
|---|---|---|---|
| 1 | domain tag | 28 | ASCII literal `rmux web-share v1 transcript` |
| 2 | `len(client_hello)` | 8 | `u64` little-endian |
| 3 | `client_hello` | variable | raw UTF-8 JSON wire bytes |
| 4 | `len(server_challenge)` | 8 | `u64` little-endian |
| 5 | `server_challenge` | variable | raw UTF-8 JSON wire bytes |

Lengths MUST be little-endian `u64` (not big-endian, not varint). Length-prefixing makes the concatenation unambiguous: `("ab","c")` and `("a","bc")` MUST differ (`transcript.rs:54-59`).

### 8.4 Exact-bytes requirement (normative)

Callers MUST pass the precise bytes that appeared on the wire and MUST NOT re-serialize the message structs (`transcript.rs:28-30`). Server side: the parser stores `raw = text.to_owned()` (`crypto.rs:14-16,26,111`) and derivation passes `hello.raw.as_bytes()`. The `server_challenge` is serialized once and the same buffer is both bound and transmitted (`server.rs:348-352,363,375`). The client side forwards both buffers as "exact wire bytes" to `derive_client_session` (`wasm.rs:88,94-100`); confirming the browser stores and passes the literal sent-hello and received-challenge buffers (no re-serialization) is an auditor item against the deployed frontend (Appendix B).

### 8.5 What is bound

Because both full messages are hashed, every field is bound automatically: protocol version, capabilities, both X25519 public keys, the ML-KEM encapsulation key (in hello), the ML-KEM ciphertext (in challenge), and both nonces — giving downgrade resistance and hybrid ciphertext binding.

### 8.6 Known-answer test vector (normative)

```
client_hello     = {"type":"hello","protocol_version":1,"capabilities":["e2ee-token-auth"],"token_id":"tok","client_nonce":"nonce","client_public":"pub","client_ml_kem_ek":"ek"}
server_challenge = {"type":"challenge","protocol_version":1,"capabilities":["e2ee-token-auth"],"server_nonce":"nonce","server_public":"pub","server_ml_kem_ct":"ct"}
transcript_hash  = c269da65fcd3ded338735b48f95fc833b8bff39146417043ae6a0aff8c90212c
```
(`transcript.rs:69-77`, test `wire_stable_transcript_hash_vector`, confirmed passing.)

---

## 9. Record Layer

After the handshake, every application message is one WebSocket **Binary** frame carrying exactly one encrypted record. A WebSocket **Text** frame received post-handshake MUST be rejected (`InvalidData "plaintext websocket text after e2ee handshake"`, `crypto.rs:172-175`). There are two independently-keyed directions (c2s, s2c), each with its own AEAD key, 4-byte nonce prefix, and monotonic 64-bit sequence counter, processed by a `RecordSealer`/`RecordOpener` pair (`session.rs:10-48`).

### 9.1 Encrypted record frame

| Offset | Length | Field | Value / encoding |
|---|---|---|---|
| 0 | 1 | `magic` | constant `0xE0` (`ENCRYPTED_FRAME`, `record.rs:27`) |
| 1 | 8 | `seq` | 64-bit sequence number, **big-endian** (`record.rs:48`) |
| 9 | N | `ciphertext_and_tag` | ChaCha20-Poly1305 ciphertext followed by the 16-byte Poly1305 tag (`record.rs:93-112`) |

- The first 9 bytes (`magic || seq`) are the cleartext authenticated header (`HEADER_LEN = 9`, `record.rs:30`).
- Tag length is 16 bytes (`TAG_LEN = 16`, `record.rs:32`).
- Minimum frame length MUST be `MIN_FRAME_LEN = 25` (9-byte header + 16-byte tag over empty plaintext, `record.rs:33-34`). The kind-byte layer (§9.8) never produces empty inner plaintext, so the smallest frame `Sealer` actually emits is 26 bytes (1 kind byte + 0 body + 16 tag).
- The wire frame produced by `seal` is `header(9) || ciphertext_and_tag` = `0xE0 || be_u64(seq) || ChaCha20Poly1305(…)` (`record.rs:109-112`).

### 9.2 AEAD

ChaCha20-Poly1305 (IETF, RFC 8439; `record.rs:21-22`). Key = 32-byte per-direction key from §7. Invoked with `Payload { msg = inner_plaintext, aad = header }` (`record.rs:96-101,170-178`). A passing RFC 8439 ChaCha20-Poly1305 KAT is present in `tests/kat.rs`.

### 9.3 Nonce construction (12 bytes)

```
nonce = nonce_prefix (4)  ||  be_u64(seq) (8)
```
`make_nonce` writes `nonce[0..4] = prefix`, `nonce[4..12] = seq.to_be_bytes()` (`record.rs:37-42`). The prefix is the fixed per-direction value from §7.5 — a 4-byte HKDF-Expand output, derived per session (`schedule.rs:100-103`), **not** a wire constant. Because the prefix is fixed per direction and `seq` is unique and strictly monotonic within that direction, every nonce is unique for the key lifetime. The same `seq` occurs once in each direction, but the two directions have distinct keys and distinct prefixes, so there is no cross-direction collision.

### 9.4 AAD

The AAD MUST be exactly the 9-byte cleartext header:

```
aad = 0xE0 || be_u64(seq)     (9 bytes)
```
(`make_header`, `record.rs:45-50`; passed on both seal and open, `record.rs:99,176`). On open the AAD is the received `frame[..9]`; any tampering with `magic` or `seq` causes AEAD authentication failure (`Error::Decrypt`).

### 9.5 Sequence numbering

- Both sealer and opener start at `next_seq = 0` (`record.rs:65,129`); the first frame in each direction carries `seq = 0`.
- **Sealer:** uses `seq = next_seq`, then increments `next_seq += 1` **only after** AEAD encryption succeeds (`record.rs:89-107`).
- **Opener:** accepts a frame only if `seq == next_seq` (strict in-order, `record.rs:160-162`); increments **only after** successful authentication/decryption (`record.rs:181-182`).
- Sequence numbers are 64-bit unsigned, big-endian on the wire; they MUST NOT wrap.

### 9.6 Opener processing order (replay & reorder rejection)

The opener MUST process frames in this order (`record.rs:149-185`):

1. **Bounds/magic check (first):** if `frame.len() < 25` OR `frame[0] != 0xE0` → `Error::MalformedFrame` (`record.rs:150-152`). All attacker-controlled slicing is bounds-checked so opening never panics.
2. Parse `seq` from `frame[1..9]` big-endian (`record.rs:155-158`).
3. **Sequence check:** if `seq != next_seq` → `Error::OutOfOrder` (`record.rs:160-162`). This single exact-match check rejects both replays (`seq < next_seq`) and reordering/gaps (`seq > next_seq`). There is no replay window and no out-of-order buffering.
4. **Exhaustion check:** if `next_seq == u64::MAX` → `Error::SequenceExhausted` **before** decryption (`record.rs:163-165`).
5. AEAD decrypt with computed nonce and header AAD; failure → `Error::Decrypt` (`record.rs:167-179`).
6. On success, increment `next_seq`, return inner plaintext.

The counter advances only after authenticated success. On AEAD authentication failure the opener does **not** advance `next_seq` (`record.rs:170-182`: the decrypt error returns before line 182). In principle a correctly-authenticated retransmission at the same `seq` could later be accepted; this is unreachable over the in-order WebSocket/TCP transport, and no test pins it. An auditor should note the property as a transport-dependent assumption rather than a record-layer guarantee.

### 9.7 Fail-closed sequence exhaustion at `u64::MAX`

- **Sealer:** before sealing, if `next_seq == u64::MAX` → `Error::SequenceExhausted`, sealing nothing (`record.rs:86-88`). The terminal value `u64::MAX` is deliberately left unused, so the terminal nonce is never produced. Maximum records per direction = `u64::MAX` (sequences `0 .. u64::MAX-1`). Tests: sealing at `u64::MAX-1` succeeds then the next fails closed (`record.rs:238-248`); sealing at `u64::MAX` fails closed (`record.rs:197-203`).
- **Opener:** if `next_seq == u64::MAX` → `Error::SequenceExhausted` (after the `seq == next_seq` match, before decrypt; `record.rs:163-165`, test `record.rs:205-235`).

### 9.8 Inner kind-byte framing

The inner plaintext is framed with a single leading kind byte (`framing.rs`); the record layer treats it as opaque (`record.rs:16-18`).

```
plaintext = kind (1) || body (N)
```
(`framing.rs:45-50`)

| Value | Kind | Body semantics |
|---|---|---|
| `0x00` | `KIND_TEXT` | UTF-8 text (`framing.rs:12,36-38`) |
| `0x01` | `KIND_BINARY` | arbitrary binary (`framing.rs:13,41-43`) |

`seal_text(text)` → `0x00 || text.as_bytes()`; `seal_binary(body)` → `0x01 || body`.

**Opener decode (fail-closed, never panics; `framing.rs:64-80`):**
1. Open the underlying record (§9.6/§9.7 errors propagate unchanged).
2. Split off the first byte; if the plaintext is empty → `Error::EmptyPlaintext` (`framing.rs:71`).
3. `kind == 0x00`: validate body as UTF-8; failure → `Error::InvalidUtf8`, else `Message::Text` (`framing.rs:73-76`).
4. `kind == 0x01`: `Message::Binary(body)` (`framing.rs:77`).
5. Any other kind byte → `Error::UnknownKind(other)` carrying the offending byte (`framing.rs:78`).

(Tests: empty → `EmptyPlaintext`; `0x7f` → `UnknownKind(0x7f)`; `0x00` + `0xff 0xfe` → `InvalidUtf8`; `framing.rs:104-123`.)

### 9.9 Error variants (record + framing)

All distinct internally (`error.rs:11-37`; `Error` is `#[non_exhaustive]`, derives `PartialEq, Eq`):

| Variant | Meaning |
|---|---|
| `MalformedFrame` | frame `< 25` bytes or wrong magic |
| `OutOfOrder` | `seq != next_seq` (replay or reorder) |
| `SequenceExhausted` | counter at `u64::MAX`; fail closed, never wraps |
| `Decrypt` | AEAD authentication/decryption failure (also internal seal-encrypt failure) |
| `EmptyPlaintext` | decrypted record had no kind byte |
| `UnknownKind(u8)` | unrecognised kind byte |
| `InvalidUtf8` | text-record body not valid UTF-8 |
| `WeakSharedSecret` | all-zero X25519 DH (key schedule) |
| `KeyDerivation` | HKDF-Expand failure |

### 9.10 Opaque collapse on the wire (no crypto oracle)

Fine-grained record errors are never distinguished to a remote peer. `FrameOpener::open_message` maps every `rmux_web_crypto::Error` from `Opener::open` to one opaque `io::Error{ kind = InvalidData, msg = "e2ee open failed" }` (`crypto.rs:208-218`); seal failures collapse to `io::Error::other("e2ee seal failed")` (`crypto.rs:221-232`). During a live session this `InvalidData` propagates out of `read_message()` (`server/streams.rs:94-95`) and terminates the connection, logged only at `debug` (`server.rs:103-104`); **no distinguishing WebSocket close-code frame** is emitted for a record-layer crypto failure (the TCP connection simply ends). The numeric close codes in the web module (§11.4) are handshake/protocol-layer concerns and are not triggered by `record.rs`/`framing.rs` errors.

---

## 10. Token & PIN Authentication

### 10.1 Token (PSK source)

A share MAY expose an **operator** token and/or a **spectator** token. Each is a high-entropy 256-bit secret: `base64url`-no-pad of 32 random bytes (`secrets.rs:33-37`). The operator token is from 32 bytes of `getrandom`; the spectator token, when an operator token also exists, is **derived** (§10.4). The token string MUST NOT be transmitted on the wire; it travels only in the share URL fragment (`#t=...`).

### 10.2 PSK derivation (SHA-256 over the token STRING)

```
psk = SHA256( ASCII bytes of the base64url token STRING )      // exactly 32 bytes
```
This is `SHA256` of the token **string characters**, NOT of the base64url-decoded 32-byte value. `SecretHash::from_secret` hashes `secret.as_bytes()` (the token string) (`secrets.rs:52-57`); this is a test-pinned invariant (`secrets.rs:102-128`). The PSK is mixed into HKDF IKM as raw bytes (§7.4). The browser MUST compute the PSK identically (auditor item; §4.4).

KAT (passing): token `"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"` → `psk = ea866a757e4c38babfa8127cbe9a409d3e1f93a00ff1488ff735fcf917afffd0` (`secrets.rs:103-111`).

### 10.3 token_id (non-enumerable lookup handle)

```
token_id = base64url( SHA256( b"rmux-token-id-v1" || psk_bytes )[0..16] )
```
where `psk_bytes` = the 32-byte `SecretHash` (`secrets.rs:63-69`). It is a 128-bit (16-byte) handle. A valid `token_id` MUST base64url-decode to exactly 16 bytes (`secrets.rs:72-76`). It is one-way: it cannot be reversed to the token and does not yield the PSK. It is sent in the hello and used only to look up the share record and the stored `SecretHash`.

KATs (passing): operator token above → `token_id = "VANRFV6FYQX1QTOi-BMVrQ"` (`secrets.rs:113-115`); derived spectator token → `token_id = "PhCBAZMZcF4zzFOwp0GBjA"` (`secrets.rs:124-127`).

### 10.4 Spectator-token derivation

When a share has both roles, the spectator token MUST be derived from the operator token (`secrets.rs:39-46`):

```
secret           = base64url_decode(operator_token)          // exactly 32 bytes, else error
spectator_token  = base64url( HKDF-SHA256(salt = none, ikm = secret).expand(info = b"rmux read token v1", 32) )
```
(`SPECTATOR_TOKEN_INFO = b"rmux read token v1"`, 18 bytes, `secrets.rs:7`.) KAT (passing): operator token above → spectator token `"f-dj7QKyPUJhAZabQ7IkQCRR1DoYQvIGf-OkgSGMuo4"` (`secrets.rs:116-119`). A spectator-only share uses an independent random token (§10.1).

### 10.5 share_id and pairing code (PIN) generation

- `share_id`: 8 characters from the base32 alphabet `b"abcdefghijklmnopqrstuvwxyz234567"`, encoding 5 random bytes (40 bits) (`secrets.rs:9-20`). Used as the human-facing handle and per-role backoff key prefix.
- Pairing code (PIN): exactly **6 ASCII digits** — `format!("{:06}", v % 1_000_000)` where `v` is a uniform value `< 16_000_000`, regenerated until `< 16_000_000` to keep the distribution uniform (`secrets.rs:22-31`).

### 10.6 PIN policy

- PINs exist only when `require_pin == true`. With `require_pin == false`, supplying `operator_pin`/`spectator_pin` MUST be rejected; no PINs are stored (`pairing.rs:13-43`).
- A forced PIN (`--pin-operator` / `--pin-spectator`) MUST be exactly 6 ASCII digits or creation fails; a forced PIN for a disabled role MUST be rejected (`pairing.rs:101-111`).
- If both roles are enabled and both PINs are present, the operator and spectator PINs MUST differ (auto-generated PINs regenerated until distinct) (`pairing.rs:33-37`).
- **The PIN MUST NOT be fed into the key schedule / HKDF.** The channel is authenticated solely by the 256-bit token (PSK). The PIN is a secondary factor checked **after** the token-authenticated channel exists. Mixing a low-entropy PIN into the KDF would create an offline brute-force handle once the token is known (`SECURITY.md:31-35`; §12.1).

### 10.7 PIN comparison (constant time, length-guarded)

The PIN MUST be compared in constant time: equal length required first (`left.len() == right.len()`), then `subtle::ConstantTimeEq::ct_eq` over the bytes (`secrets.rs:78-82`). If the role has no configured PIN (`expected == None`), the check is a no-op success. Outcomes (`pairing.rs:53-74`):
- PIN provided but wrong → `"invalid web-share pairing code"`.
- PIN absent but required → `"missing web-share pairing code"`.

The inbound `pin` field is `Option<String>` with `#[serde(default)]` (`protocol/mod.rs:107-108`) and is **not** length/charset-validated in `read_auth_message` (`handshake.rs:74-113`). This is safe: a non-6-digit pin can never match a 6-digit configured PIN because the constant-time compare requires equal length first (`secrets.rs:81`).

### 10.8 Authorization ordering

The full authorization ordering (across `connect_token_id` and `record.connect`) MUST be:

0. **Process-wide connection permit** (`connection_limit.try_acquire`) acquired in `registry.rs:466-467`, **before** `record.connect` runs (hence before the role/PIN checks). On exhaustion it yields `"web-share connection limit reached"` (`registry.rs:468-469`), which collapses to `(4000,…)`. This gate is token-independent and pre-PIN, so it does not weaken PIN-oracle suppression; it is recorded as backoff `Other` (no escalation), because `is_auth_failure_for_backoff` does not match it (§10.9).

Then, inside `record.connect(pin, role, permit)` (`record.rs:89-128`):

1. **Role-exists check** (token grants this role) — else `"web-share has no {operator,spectator} access"`.
2. **PIN check** (§10.7, `pairing_codes.check`).
3. **Per-role lease / capacity acquisition** (`try_operator` / `try_spectator`, `record.rs:103-106,121-124`) — else `OPERATOR_LIMIT_ERROR = "web-share operator limit reached"` / `SPECTATOR_LIMIT_ERROR = "web-share spectator limit reached"` (`record.rs:19-20`).

So the precise full ordering is: **connection-permit (pre-PIN) → role-exists → PIN → per-role lease.** Consequence (intentional): a correct PIN is required to even reach the per-role lease check, which motivates the capacity-after-PIN wire collapse (§11.3).

### 10.9 Authentication backoff / lockout (`AuthBackoff`)

The backoff key is built in `connect_token_id` (`registry.rs:427-436`) as `format!("{}:{}", capability.share_id, capability.role.backoff_label())`, i.e. `"{share_id}:{role}"`, where the role labels `"operator"`/`"spectator"` come from `backoff_label()` (`record.rs:215-220`). An unresolved token uses the fallback `UNKNOWN_TOKEN_BACKOFF_KEY = "token_id:<unknown>"` (`registry.rs:42`).

Constants (`backoff.rs:5-21`):

| Constant | Value |
|---|---|
| `INITIAL_DELAY` | 100 ms |
| `MAX_DELAY` | 10 s |
| `RESET_AFTER` | 5 min (idle resets `fails`/`in_flight`) |
| `GC_AFTER` | 10 min (idle GC; `lifetime_fails` survives) |
| `INITIAL_LOCK_DURATION` | 1 h |
| `MAX_LOCK_DURATION` | 24 h |
| `MAX_BACKOFF_ENTRIES` | 4096 (oldest evicted at capacity) |
| `MAX_SHIFT` | 7 |
| `MAX_LOCK_SHIFT` | 8 |
| `LOCK_FAIL_CAP` | 50 (failures per lock window) |
| `LIFETIME_FAIL_CAP` | 500 (permanent fail-closed budget) |

Rules (`backoff.rs:126-247`):
- Each attempt MUST `reserve_attempt` (cancellation-safe RAII) before contacting the registry. Reservation position = `fails + in_flight` (concurrent in-flight guesses also escalate). Delay = `INITIAL_DELAY * 2^min(position-1, MAX_SHIFT)` capped at `MAX_DELAY`; position 0 → zero delay. The caller MUST `sleep(delay)` before proceeding.
- Outcomes settle once: `Success` (clears/removes entry); `AuthFailure` (increments `fails`, `lock_window_fails`, `lifetime_fails`); `Other` (capacity / missing PIN / connection-limit / dropped guard — releases in-flight, does NOT escalate).
- `AuthFailure` is recorded only for registry errors whose message contains `"invalid web-share pairing code"` or `"does not exist or has expired"` (`is_auth_failure_for_backoff`, `registry.rs:660-664`; matches via `.contains()`). The process-wide `"web-share connection limit reached"` is **not** matched, so it settles as `Other` (no escalation).
- **Temporary lock:** when `lock_window_fails >= 50`, the key is locked for `INITIAL_LOCK_DURATION * 2^min(completed_windows, MAX_LOCK_SHIFT)` capped at 24 h (1h, 2h, 4h … 24h). Attempts during a lock do NOT extend it; lock expiry resets `fails`/`in_flight`/`lock_window_fails` (not `lifetime_fails`).
- **Permanent fail-closed:** when `lifetime_fails >= 500`, the key is permanently locked (`permanently_locked()` true).
- A `Locked` decision MUST reserve no in-flight slot and MUST return the SAME uniform error as a missing share (`"web-share does not exist or has expired"`), exposing no new oracle. On the wire this collapses to `(4000, …)`.

---

## 11. Connection Lifecycle & Uniform Error/Close Behavior

### 11.0 Timeouts and process caps

| Constant | Value | Citation |
|---|---|---|
| `PRE_AUTH_TIMEOUT` | 5 s (HTTP read; hello read) | `protocol/mod.rs:41` |
| `AUTH_FRAME_TIMEOUT` | 2 s (auth frame) | `protocol/mod.rs:42` |
| `PRE_READY_TIMEOUT` | 8 s (whole pre-ready wrapper) | `server.rs:27-30` |
| `WEB_WRITE_TIMEOUT` | 2 s (each client write) | `server.rs:27-30` |
| `UNIFORM_AUTH_DELAY` | 50 ms | `protocol/mod.rs:43` |
| `PRE_AUTH_SLOTS` | 64 (pre-auth queue) | `server.rs:27`, `server/pre_auth.rs` |
| `HTTP_READ_LIMIT` | 8 KiB | `server/http.rs:7-32` |
| `DEFAULT_AUTHENTICATED_CONNECTION_LIMIT` | 256 (process-wide, atomic, RAII) | `connection_limit.rs:9,25-62` |
| `DEFAULT_MAX_OPERATORS` | 1 | `registry.rs:40` |
| `DEFAULT_MAX_SPECTATORS` | 12 | `registry.rs:41` |
| `MAX_TTL_SECONDS` | 7 days | `registry.rs:39` |
| `OPERATOR_RATE_LIMIT` | 200 frames / rolling 1 s window / connection | `server/rate_limit.rs:3` |

Pre-auth queue: a `PreAuthGuard` is acquired on TCP accept; if full, the connection is dropped immediately with no response. The guard is released the moment the auth frame is read.

HTTP: only `GET`/`HEAD` accepted (`405` otherwise); only `GET /share` with a valid WebSocket upgrade served (`404` otherwise); oversize/invalid headers → `431`; responses carry `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`, `Connection: close`.

WebSocket upgrade validation order (`serve_websocket`, `server.rs:156-187`), all failures → `400`:
1. `Sec-WebSocket-Key` present (else `400`).
2. `Sec-WebSocket-Version` trimmed `== "13"` (else `400`).
3. `valid_client_key`: STANDARD-base64 decode of `Sec-WebSocket-Key` to exactly 16 bytes (`websocket.rs:290-294`; else `400`).

`is_websocket_upgrade` (`http.rs:72-81`) additionally requires `Upgrade == "websocket"` (case-insensitive) AND a `Connection` header containing an `upgrade` token; this, together with `method == GET` and `path == "/share"`, is checked at `server.rs:138`.

`Origin` MUST be present (else pre-ready `origin_required`, §11.6). The post-ready operator command rate limit drops over-budget frames (logged); it does not close.

### 11.1 Pre-ready sequence (server view)

MUST be exactly this order; any failure before "ready" collapses per §11.2 (`server.rs:301-394`, `handshake.rs:35-113`):

1. TCP accept → acquire pre-auth slot (else drop).
2. Read+parse HTTP `GET /share` upgrade; validate method/key/version/upgrade (§11.0) and `Origin` presence (`server.rs:309-312`).
3. WebSocket `accept` (server handshake response).
4. **Read client hello** (Text, within `PRE_AUTH_TIMEOUT`); validate per §6.1. Retain the exact raw hello text for transcript binding.
5. **Pre-auth token lookup** `web_share_pre_auth_token(token_id, origin)` → `SecretHash` (PSK) + `origin_allowed` bool (`server.rs:329-332`); also runs `prune_expired`. Unknown `token_id` → reject (`unknown_token`); disallowed origin → reject (`origin_not_allowed`) (`registry.rs:503-519`).
6. Generate ephemeral server X25519 keypair (forward secrecy).
7. **ML-KEM-768 encapsulate** to `client_ml_kem_ek`; a malformed or wrong-length key yields `Ok(None)` (never propagated via `?`): `<&[u8;1184]>::try_from` failing (defense-in-depth; `parse_client_hello` already rejected non-1184-byte keys at `crypto.rs:122-124`) and the FIPS 203 §7.2 modulus check both map to `Ok(None)` → uniform reject `invalid_ml_kem_key` (`crypto.rs:144-148`). An RNG failure propagates as an error.
8. Generate random 16-byte `server_nonce` → base64url string.
9. **Build challenge text** (the exact bytes both bound and sent; §6.2).
10. Complete X25519 DH (consumes the ephemeral secret).
11. **Derive session** `derive_server_crypto(psk = SecretHash bytes (32), dh, ml_kem_ss, hello.raw bytes, challenge bytes)`. All-zero DH (low-order client public key) → uniform reject `weak_shared_secret` (never via `?`).
12. **Send challenge** (the same bound bytes), within `WEB_WRITE_TIMEOUT`.
13. **Read+decrypt auth frame** (Binary, within `AUTH_FRAME_TIMEOUT`). It MUST decrypt under the derived `Opener`; an undecryptable frame (wrong token / wrong transcript) → uniform reject `invalid_encrypted_auth` (end-to-end: `handshake.rs:85-87` → `reject_handshake(…)` `server.rs:380` → `(4000)` `server.rs:417,425-428`). Decrypted plaintext MUST be a TEXT message containing JSON (`deny_unknown_fields`, `protocol/mod.rs:100-109`): `{type:"auth", protocol_version:1, capabilities:[…], pin?:string}`. Validate `type=="auth"`, `protocol_version==1`, `capabilities` contains `"e2ee-token-auth"`; presence of `"pane-frame-v1"` sets `supports_session_pane_frame`; `pin` is OPTIONAL. **At this point the channel is token-authenticated** (AEAD open succeeding implies the peer holds the PSK). Drop the pre-auth guard.

**Registry/authorization phase** (runs OUTSIDE `PRE_READY_TIMEOUT`, because backoff may sleep longer; `server.rs:227-299`):

14. `open_web_share_token_id(token_id, pin)` → `connect_token_id` (`registry.rs:411-501`): resolve the share/capability **twice** — first to compute the backoff key (`registry.rs:419-426`), then, **after** the backoff sleep (§10.9), re-resolve (`registry.rs:458-480`) to guard against TOCTOU / expiry during the sleep. The second resolution is authoritative and is what is passed to `record.connect`. Acquire the process connection permit (`registry.rs:466-467`), then `record.connect(pin, role, permit)` (connection-permit → role-exists → PIN → per-role lease, §10.8).
15. On success: re-verify `origin_allowed` against the resolved share (`server.rs:278-281`; else uniform reject `origin_not_allowed`); `sleep(UNIFORM_AUTH_DELAY)`; split the socket into reader + outbound; send the encrypted `ready` message. **Connection established.**
16. Missing-PIN sub-case: see §11.2.

After "ready", all frames are encrypted records; any plaintext Text frame is a hard error (§9).

### 11.2 Uniform pre-ready rejection (anti-oracle)

Every pre-ready/auth failure (steps 2–14, except the missing-PIN case below) MUST collapse on the wire to the single close pair:

```
HANDSHAKE_REJECTED = (4000, "handshake_rejected")
```
(`protocol/mod.rs:47-54`). Before sending it, the server MUST `sleep(UNIFORM_AUTH_DELAY = 50ms)`, log the precise internal reason server-side (event `web_share_handshake_rejected`, with structured fields `reason` AND `close_code`, `server.rs:427`), and send the close (`server.rs:425-428`). The precise reason MUST NEVER appear on the wire. Internal reasons all mapping to `(4000, …)` include (server-log only, non-exhaustive): `origin_required`, `origin_not_allowed`, `hello_timeout`, `invalid_hello_frame`, `hello_must_be_text`, `invalid_hello`, `unknown_token`, `invalid_ml_kem_key`, `weak_shared_secret`, `auth_timeout`, `invalid_auth_frame`, `auth_must_be_encrypted`, `invalid_encrypted_auth`, `auth_must_be_text`, `invalid_auth_json`, `first_frame_must_auth`, `protocol_version_mismatch`, `missing_e2ee_capability`, the registry mappings `spectator_cap_reached`, `operator_cap_reached`, `operator_not_available`, and the catch-all `invalid_auth` (wrong PIN, unknown/expired share, **process-wide connection-limit `"web-share connection limit reached"`**, locked).

**The one intentional non-4000 pre-ready code — `pin_required`:** a token-authenticated client that omitted a required PIN (registry message contains `"missing web-share pairing code"`) MUST be closed with `(4008, "pin_required")` after `sleep(UNIFORM_AUTH_DELAY)`. `establish_web_share` (`server.rs:267-271`) intercepts that message **before** calling `close_for_auth_error`, emits `write_close_code(4008, "pin_required")` (`server.rs:269`), and returns `Ok(None)`. This is the ONLY distinguishable pre-ready code besides 4000. It is safe and not a PIN oracle because (a) it is reachable only after the token-authenticated handshake; (b) it fires only on an **absent** PIN, never a **wrong** PIN — a wrong PIN yields `"invalid web-share pairing code"` which collapses to `(4000,…)` via `invalid_auth`. Thus PIN correctness is never disclosed; only "you forgot to send one" is.

The generic auth-error mapper deliberately does not return `pin_required`; if reached, missing PIN remains collapsed into the generic `(4000, "handshake_rejected")` path (`handshake.rs:126-148,181-193`). The live server path intercepts missing PIN before that fallback, so the only wire-level missing-PIN behavior is the token-authenticated 4008 close above.

### 11.3 Capacity-after-PIN collapse (PIN-oracle suppression)

Because `record.connect` checks PIN before the per-role lease (§10.8), a "capacity reached" outcome implies the PIN was correct. To prevent capacity codes acting as a PIN oracle, `OPERATOR_LIMIT_ERROR` and `SPECTATOR_LIMIT_ERROR` MUST both collapse to `(4000, "handshake_rejected")` (logged precisely as `operator_cap_reached` / `spectator_cap_reached`) (`handshake.rs:126-159`). The token-independent process-wide connection limit (§10.8 step 0) similarly collapses to `(4000,…)`.

### 11.4 Post-ready (established) close codes

These are operational and intentionally distinguishable (the channel is already authenticated):

| Code | Reason | Meaning | Citation |
|---|---|---|---|
| `1000` | revoke string | graceful revoke/expiry; strings: `"pane_gone"`, `"session_gone"`, `"stopped_by_owner"`, `"ttl_expired"` (`WebShareRevokeReason::as_str`) | `record.rs:223-229,231-239`, `streams.rs:681` |
| `4001` | `"viewer_backpressure"` | viewer output queue overflow / slow consumer (`SLOW_VIEWER_CLOSE_CODE`) | `streams.rs:27` |
| `4006` | `"spectator_no_binary"` | a spectator (non-operator) sent a binary (input) frame | `streams.rs:105,329` |
| (none) | plain Close | sent in response to a client Close | — |

### 11.5 Close-frame encoding

A coded close payload is `code.to_be_bytes()` (2 bytes, big-endian) followed by the reason UTF-8 bytes truncated to at most 123 bytes, written as a WebSocket CLOSE frame (`websocket.rs:85-92,130-137`).

### 11.6 Origin enforcement

Origin is checked at three points:
- **Presence:** `Origin` MUST be present, else pre-ready reject `origin_required` (`server.rs:309-312`).
- **Pre-ready allow-check:** against the token's record via the `origin_allowed` bool returned by `web_share_pre_auth_token` (`server.rs:329-332`).
- **Post-authorization re-check:** against the resolved share (`share.origin_allowed`, `server.rs:278-281`).

A disallowed origin MUST collapse to `(4000,…)` (`origin_not_allowed`). Loopback/development origins (`127.0.0.1`/`localhost`) are allowed only when no public base URL was configured (`allow_loopback_development_origins`; `origin.rs:20-26,199-200`).

---

## 12. Security Invariants

### 12.1 Dual-PRF / transcript-as-salt rationale

The HKDF salt is the **public** 32-byte transcript hash; all secret material lives in the IKM. This is a deliberate dual-PRF / Noise-style PSK + ephemeral-ephemeral arrangement: the public transcript salts the extract while the three secrets (X25519 DH, ML-KEM-768, PSK) supply entropy. Because the transcript is the salt, any transcript tampering (capability stripping, version downgrade, public-key/ciphertext substitution) changes PRK and therefore every traffic key — fail closed. The PIN is deliberately excluded from the KDF (§10.6) to avoid creating an offline brute-force handle on a low-entropy secret once the token is known.

### 12.2 Hybrid security

The channel is hybrid by construction: it stays secure as long as the PSK is secret AND at least one of {X25519, ML-KEM-768} remains unbroken. This provides "harvest now, decrypt later" resistance: recorded ciphertext stays protected against a future X25519 break (§4.1.6).

### 12.3 X25519 vs ML-KEM zero-check asymmetry

X25519 is value-checked (constant-time all-zero rejection, §7.3) because a low-order point yields a predictable/all-zero DH value (RFC 7748). ML-KEM-768 is **not** value-checked, by design: the FO transform uses implicit rejection — an invalid ciphertext yields a pseudorandom, secret-key-bound shared secret with no all-zero sentinel, so a value check would be meaningless. ML-KEM validity is enforced earlier, at encapsulation time (FIPS 203 §7.2; `None` → fail closed). There is no in-tree code-level assertion that `libcrux-ml-kem` can never return an attacker-forceable constant shared secret; the in-tree NIST ACVP / CCTV ML-KEM-768 KATs (`tests/kat.rs`) check correctness against published vectors, not adversarial constancy. This is an external-dependency trust assumption on the (formally verified) `libcrux-ml-kem` crate and is in scope for the auditor.

### 12.4 Fail-closed properties

- Invalid/wrong-length ML-KEM key → `Ok(None)` → uniform rejection (never `?`).
- All-zero X25519 DH → `WeakSharedSecret` → uniform rejection.
- HKDF-Expand failure → `KeyDerivation`.
- Sequence exhaustion at `u64::MAX` → `SequenceExhausted`; the counter never wraps and never reuses a nonce.
- Malformed/short/wrong-magic frame → `MalformedFrame`; out-of-order/replay → `OutOfOrder`; AEAD failure → `Decrypt`.
- Every pre-ready failure → `(4000, "handshake_rejected")` after a 50 ms uniform delay (the sole exception being the post-token-auth missing-PIN case → `(4008, "pin_required")`, §11.2); every live-session record failure → connection termination with no distinguishing close frame.
- A `Locked` backoff decision → the same uniform error as a missing share.

### 12.5 Forward secrecy

Each connection uses fresh ephemeral keys: a per-connection ephemeral X25519 keypair (single-use; the secret is consumed when the DH is computed) and a per-connection ML-KEM-768 keypair. A relay that records TLS-decrypted ciphertext and later obtains the share token cannot decrypt past traffic, because the per-connection X25519 and ML-KEM secrets are discarded after derivation. Native ephemeral X25519 secrets are zeroized on drop; the HKDF IKM and raw key arrays are zeroized after the ciphers are built (`schedule.rs:106`, `SessionKeys` `ZeroizeOnDrop`).

### 12.6 Authentication binding

The first encrypted (auth) frame is proof-of-knowledge of the token: only a peer holding the PSK derives the matching keys, so an AEAD open success at step 13 establishes that the channel is token-authenticated. The end-to-end mapping is confirmed: an undecryptable auth frame (wrong token / wrong transcript) maps to the uniform `(4000)` path (`handshake.rs:85-87` → `server.rs:380,417,425-428`).

---

## 13. Residual Risks & Out-of-Scope

An implementation and auditor MUST acknowledge these residual risks (`SECURITY.md:45-88`, `docs/assurance-case.md:97-106`):

1. **`deriveBits` exposes the X25519 DH shared secret to the JS heap** before it crosses into WASM. Only the X25519 private key remains non-extractable. This is not a complete memory-isolation boundary and is weaker than a full Noise-in-WASM design.
2. **Browser-side secret scrubbing is weak.** JS/WASM runtimes give weaker memory-wiping guarantees than native; browser-side secrets MUST NOT be assumed fully scrubbed (the 32-byte DH/ML-KEM inputs are `Zeroizing` inside WASM, but the surrounding JS heap is not).
3. **The share page origin and CSP are part of the TCB.** An XSS on the page can read the token from `#t=...` or drive the page; E2EE does not protect a compromised page.
4. **Metadata leakage.** Timing, packet sizes, and connection metadata may be visible to relays / passive observers even though payloads are encrypted.
5. **A compromised endpoint can read its own terminal contents** (out of scope, but a real residual exposure).
6. **Resource exhaustion / DoS** remains possible against a user-exposed share endpoint.

Out-of-scope adversaries and trust boundaries are normatively defined in §4.2–§4.3.

---

## 14. Versioning & Negotiation

- The single wire version field `protocol_version` MUST be the integer `1` in both `client_hello` and `server_challenge` (§6). A mismatch → uniform rejection (`protocol_version_mismatch`).
- There is no in-band version range negotiation: both peers assert `1`. Capability negotiation is by list membership: the client MUST advertise `"e2ee-token-auth"`; the server advertises exactly `["e2ee-token-auth","terminal-palette-v1","pane-frame-v1"]`. Optional capabilities (`"pane-frame-v1"`, `"terminal-palette-v1"`) are detected by presence and gate optional features (e.g. `supports_session_pane_frame`).
- All cryptographic labels are versioned and frozen for v1: transcript domain `b"rmux web-share v1 transcript"` (28 B); HKDF info labels `rmux web-share v1 key c2s` / `… key s2c` (25 B) / `… nonce c2s` / `… nonce s2c` (27 B); `token_id` domain `b"rmux-token-id-v1"` (16 B); spectator-token info `b"rmux read token v1"` (18 B). Any change to these labels, the IKM layout, the transcript byte layout, the record frame layout, the magic byte `0xE0`, the nonce/AAD construction, or the close-code policy is a breaking change and MUST be accompanied by a `protocol_version` bump.
- The module-level protocol docs and the wire constants are aligned on v1 in the cited implementation tree (§1.4).

---

### Appendix A — Consolidated constant & label reference

| Item | Value | Citation |
|---|---|---|
| Encrypted-frame magic | `0xE0` | `record.rs:27` |
| Header length (magic‖seq) | 9 | `record.rs:30` |
| AEAD tag length | 16 | `record.rs:32` |
| Minimum record frame | 25 | `record.rs:33-34` |
| Nonce length | 12 (`prefix[4] ‖ be_u64(seq)[8]`) | `record.rs:37-42` |
| Kind byte: text / binary | `0x00` / `0x01` | `framing.rs:12-13` |
| Transcript domain | `rmux web-share v1 transcript` (28 B) | `transcript.rs:11` |
| Transcript length prefix | `le_u64` | `transcript.rs:34,36` |
| HKDF key labels | `rmux web-share v1 key c2s` / `… s2c` (25 B) → 32 B | `schedule.rs:17-19` |
| HKDF nonce labels | `rmux web-share v1 nonce c2s` / `… s2c` (27 B) → 4 B | `schedule.rs:21-23` |
| IKM layout | `dh[32] ‖ ml_kem[32] ‖ psk` | `schedule.rs:79-84` |
| HKDF salt | transcript hash (32 B) | `schedule.rs:77,87` |
| PSK | `SHA256(token_string)` (32 B) | `secrets.rs:52-57` |
| token_id | `base64url(SHA256("rmux-token-id-v1" ‖ psk)[0..16])` (16 B) | `secrets.rs:63-69` |
| Spectator-token info | `rmux read token v1` (18 B) | `secrets.rs:7` |
| Wire protocol version | `1` | `protocol/mod.rs:45` |
| Server capabilities | `["e2ee-token-auth","terminal-palette-v1","pane-frame-v1"]` | `protocol/mod.rs:56-60` |
| Pre-ready reject close | `(4000, "handshake_rejected")` | `protocol/mod.rs:47-54` |
| Missing-PIN close | `(4008, "pin_required")` | `server.rs:267-271` |
| Slow-viewer close | `(4001, "viewer_backpressure")` | `streams.rs:27` |
| Spectator-binary close | `(4006, "spectator_no_binary")` | `streams.rs:105,329` |
| Graceful revoke close | `(1000, <reason>)` | `record.rs:231-239`, `streams.rs:681` |
| Uniform auth delay | 50 ms | `protocol/mod.rs:43` |
| Process connection limit | 256 | `connection_limit.rs:9` |
| Default max operators / spectators | 1 / 12 | `registry.rs:40-41` |
| Max share TTL | 7 days | `registry.rs:39` |
| Backoff key | `"{share_id}:{role}"` / `"token_id:<unknown>"` | `registry.rs:427-436,42` |

### Appendix B — Open items for the auditor (browser frontend not in this checkout)

The cryptographic core and the daemon protocol half were verified against the source at HEAD `2ecd58a3…`, and the in-tree KATs and web-module tests cover RFC 5869 HKDF-SHA256, RFC 8439 ChaCha20-Poly1305, NIST ACVP / CCTV ML-KEM-768 keygen/encaps/decaps, the transcript-hash vector `c269da65…`, the PSK/token_id/spectator KATs, and the independent HKDF reconstruction. The following items cannot be confirmed from this repository because the JS/WASM-driving frontend is not present, and remain genuinely open for the auditor to confirm against the deployed frontend:

1. **Browser PSK computation** — confirm the frontend computes `SHA256` over the token **string** (ASCII base64url characters), not the decoded bytes, and supplies exactly 32 bytes. (Daemon side proven: `SecretHash::from_secret` hashes the token string; PSK KAT passes.)
2. **Browser base64url variant** — confirm the browser emits base64url-no-pad for `client_public`, `client_ml_kem_ek`, `client_nonce`, `token_id`. (Daemon decoders/encoders use `URL_SAFE_NO_PAD`.)
3. **Client exact-bytes transcript** — confirm the browser stores and passes the literal sent-hello and received-challenge byte buffers (no re-serialization) into `derive_client_session`.
4. **ML-KEM constant-output assumption** — no in-tree assertion that `libcrux-ml-kem` cannot return an attacker-forceable constant shared secret; this is an external-dependency trust assumption on the formally verified crate (§12.3).
