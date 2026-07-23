# Security Assurance Case

This document summarizes the security properties RMUX is designed to provide and
the evidence used to support them.

## Security Requirements

RMUX is expected to:

- keep terminal execution, PTYs, panes, windows, sessions, and scrollback on the
  local machine;
- restrict local IPC to the owning user;
- keep Web Share tunnel providers and relays from reading or modifying terminal
  payloads silently;
- enforce Web Share operator and spectator roles at the daemon;
- reject malformed, replayed, reordered, oversized, or unauthenticated Web Share
  protocol frames;
- publish release artifacts with checksums and package-manager metadata created
  from the same release assets.

## Threat Model

In scope:

- passive network observers;
- tunnel providers and relays that can observe or forward WebSocket traffic;
- active payload injection, replay, reordering, and malformed Web Share frames;
- accidental cross-user access to local IPC endpoints;
- compromised package metadata or stale checksums detected before release.

Out of scope:

- a compromised local user account;
- malware, keyloggers, malicious browser extensions, or a compromised browser;
- a user loading a tampered Web Share frontend from an untrusted origin;
- denial of service against a daemon, tunnel, or network controlled by the user;
- theoretical breaks of X25519, ML-KEM-768, HKDF-SHA256, or ChaCha20-Poly1305
  without a practical attack.

## Trust Boundaries

- **Local machine boundary**: shells, PTYs, processes, sessions, and scrollback
  stay in the local daemon.
- **Local IPC boundary**: local clients reach the daemon through owner-scoped
  Unix sockets or per-user Windows named pipes.
- **Web Share transport boundary**: tunnels and relays carry WebSocket traffic
  but are not trusted with terminal plaintext.
- **Frontend boundary**: the loaded browser page is trusted. Users can self-host
  the static frontend if they need to control that boundary.
- **Release boundary**: GitHub Releases, package-manager metadata, and repository
  packages are created from tagged source and checked by CI.

## Claims and Evidence

### Local execution stays local

The daemon owns all terminal state and process lifecycle. Local clients and SDK
users send requests through local IPC; they do not move shells into a hosted
service. The platform table in the README documents the Unix socket and Windows
named-pipe backends.

### Web Share protects terminal payloads from relays

Web Share uses a hybrid handshake with ephemeral X25519, ML-KEM-768, transcript
binding, HKDF-SHA256, and ChaCha20-Poly1305 records. The relay or tunnel sees
ciphertext. AEAD authentication rejects tampering.

The `rmux-web-crypto` tests include primitive known-answer tests, fixed
handshake/channel tests, replay and ordering tests, and WebAssembly-oriented
coverage. Web Share server tests cover malformed handshakes, role enforcement,
capacity paths, and close-code behavior.

### Access roles are enforced at the daemon

Operator and spectator links are separate roles. Spectator writes and unsupported
role actions are rejected by the server, not only hidden in the browser UI.

### Common implementation weaknesses are reduced

RMUX uses Rust for memory safety. Upper-level crates forbid unsafe code, and the
remaining OS/terminal boundary code is isolated. CI runs formatting, clippy with
warnings denied, workspace tests, platform checks, dependency policy checks,
source boundary checks, WebAssembly provenance checks, package validation, and
platform smoke tests.

The Web Share protocol rejects malformed JSON, unknown fields, invalid sizes,
invalid origins, bad tokens, wrong roles, replayed records, reordered records,
oversized operator frames, and slow viewers that cannot keep up with output.

### Release artifacts are checked before publication

The release workflow creates release assets, package metadata, and Linux
repository metadata. Direct archives and packages are published with SHA256
checksums. APT and RPM repositories are signed. Homebrew, Scoop, WinGet, and
Chocolatey metadata pin the published release assets and their checksums.

For the RMUX 0.9 release line, release artifact smokes run against the final
packaged artifacts rather than the debug tree. Unix archives are verified by
`scripts/verify-package.sh` and `scripts/smoke-installed-rmux.sh`; Debian and
RPM packages are verified by `scripts/verify-debian-package.sh` and
`scripts/verify-rpm-package.sh`; Windows archives are verified by
`scripts/verify-package-windows.ps1` with daemon, SDK, mouse, and Ctrl-matrix
smoke switches in release CI. Package verification rejects hidden source-tree
tooling and temporary/socket leftovers inside direct archives.

The RMUX 0.9 release line does not publish a standalone SBOM. The release
record for this line is the tagged source tree, `Cargo.lock`, package-manager
metadata, SHA256SUMS,
Sigstore bundle, and GitHub build attestations. Revisit standalone SBOM
generation before distributing into environments that require SPDX or CycloneDX
artifacts.

## Residual Risks

- Timing, packet sizes, and connection metadata may be visible to tunnel
  providers.
- Web Share depends on the integrity of the browser page the user loads.
- A compromised endpoint can read its own terminal contents.
- Direct archive downloads currently rely on HTTPS and published checksums; APT
  and RPM provide stronger repository signing.
- Resource exhaustion remains possible if an attacker can reach a user-exposed
  share endpoint at sufficient scale.
