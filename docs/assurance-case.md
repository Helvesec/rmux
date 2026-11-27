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

The detailed [threat model](#rmux-threat-model) covers named pipes, Unix sockets,
Web Share, configuration imports, diagnostics and build inputs, including the
controls implemented at each boundary and their remaining limits.

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

## Third-party notices

The Unix, Debian, RPM and Windows packagers generate a notice from the Cargo
builds that produce the full CLI, tiny CLI and daemon. A successful package build
also writes `<package-file>.NOTICE.txt` beside the archive and includes that
file in `SHA256SUMS.txt`. Distribute all three: the package, its notice and the
checksum manifest. The notice header identifies the package by name and SHA256.
The same generated text is carried inside archives at
`share/licenses/rmux/THIRD-PARTY-NOTICES.txt` and inside DEB/RPM packages at
`/usr/share/doc/rmux/THIRD-PARTY-NOTICES.txt`. Archive installers retain that
file; the Unix installer also retains the root project licenses under
`share/licenses/rmux/`. The descriptor keeps these additions optional for
local-only packages without a notice cache. These payloads use archive layout
v4 and DEB/RPM layout v3. Historical projections retain their original
identities and installed files; validators select the declared package version.
`scripts/packaging/layout --with-notices` selects the new projections explicitly,
and `--for-package <root>` selects from an extracted package. Fresh builds and
release reuse still require a matching build-derived cache.

The package set comes from `compiler-artifact` events in completed, successful
Cargo JSONL streams. Cargo metadata supplies names, versions and licenses only
for those observed packages. Entries that appear only in `Cargo.lock` or the
resolved metadata graph are not added. Host build dependencies and procedural
macros are included as build contributions, even when not shipped as runtime
code. This is not a claim of post-link reachability and is not an SPDX/CycloneDX
SBOM or an attestation of the build machine.

License expressions must have a valid choice under `[licenses].allow` in
`deny.toml`. `AND` requires both terms; `OR` permits an allowed branch. License
exceptions are checked as a complete `license WITH exception` choice, never
silently discarded. Missing or disallowed licenses, missing upstream license
text, incomplete build streams and unknown package identities fail generation.
The source license, copyright and NOTICE files are retained in the output.
Workspace crates may inherit the workspace's root license texts. Continue to
run `cargo deny check licenses` for the repository-wide policy check; this
generator additionally restricts the notice to actual observed builds.

Some published hax/libcrux crate archives omit their repository's license files.
For the pinned versions only, the generator embeds verbatim upstream texts from
`scripts/packaging/licenses/`. It checks the observed package's crates.io source,
version, repository, license and `.cargo_vcs_info.json` revision/subdirectory
before using a supplement. This lookup never adds packages to the build graph;
other missing license texts still fail. Generation remains offline.

The upstream sources and SHA256 digests of these verbatim files are:

| Source | Files and SHA256 |
| --- | --- |
| [hax d8b5b3d3b666](https://github.com/hacspec/hax/tree/d8b5b3d3b666fee8943a351445d2b680105e8ea3) | `LICENSE`: `9a50bad5a51e0ad726ea3a7f4b7b758e1b4d1784e6abefe1367f5bf01e972725` |
| [libcrux c5fb80f37530](https://github.com/cryspen/libcrux/tree/c5fb80f37530ee9b2df9501ae5ff8cb4a973a4bd), [platform 316fcd6c19c2](https://github.com/cryspen/libcrux/tree/316fcd6c19c2e33bd0429e1319b6557d91038512) | `LICENSE`: `c517c468fc7f8d83319dd8b3743923f6891e0dfbaf7c57a874758c8f39b98564`; `LICENSE-MIT`: `e40d41f94826514279cc7648088180e19621f7acd281dd394a0534d535da51fd` |

### Generate from existing build records

Generate the metadata and capture the successful builds in the same checkout,
with the same locked dependency sources available. For example:

```sh
cargo metadata --locked --offline --format-version=1 > metadata.json
cargo build --locked --offline --package rmux --bin rmux \
  --message-format=json-render-diagnostics > full.jsonl
cargo run --locked --offline --quiet --package xtask -- third-party-notice \
  --metadata metadata.json --build-log full.jsonl \
  --policy deny.toml --output NOTICE.txt
```

The generator accepts repeated `--build-log` arguments. Keep Cargo's child exit
status: a transport returning zero does not make a failed build successful.
Only use production `cargo build` streams, not test logs, metadata, help output
or manually assembled package lists. The metadata, logs and dependency sources
are trusted builder inputs; the generator cannot authenticate a fabricated log.

### Packaging and reuse

Fresh packager builds capture their three commands, preserve the full helper
before the tiny build overwrites the public binary, and create a notice cache
under `<binary-directory>/third-party-notices/`. The cache binds the notice to
the three original binary hashes, `Cargo.lock` and the license policy. A failed
build or notice generation cannot update a previous successful cache.
Changes to the generator or bundled upstream texts also invalidate that cache.

Canonical build records retain each sidecar as an `archive-notice`,
`debian-notice` or `rpm-notice` asset. When notices are present, every supported
package must have exactly one matching sidecar. The producer checks its package
name and SHA256 header; candidate manifests, staging, checksums, authorization
and publication retain those same bytes. Historical records without any notice
extension remain readable; new packagers require build-derived notices.
The release workflow captures the Windows package binaries before their tests
and reuse manifest, so reuse also has the matching notice cache. The legacy
release upload, attestation and asset lists include the sidecars.

`--reuse-release-binary` / `-ReuseReleaseBinaries` require that matching cache.
For an external build pipeline, `scripts/packaging/notices.py capture` records
the actual Cargo commands; after the full helper has been copied and the tiny
and daemon builds have completed, record their logs with:

```sh
python3 scripts/packaging/notices.py record \
  --binary-dir target/x86_64-unknown-linux-gnu/release \
  --log full.jsonl --log tiny.jsonl --log daemon.jsonl
```

Never synthesize these logs to repackage unknown binaries. A cache from changed
payloads or a changed policy is rejected. Local-only `--skip-build
--allow-stale-binary` packages can omit a notice when no build-derived cache
exists; that mode does not establish the dependency population for distribution.
If a cache exists, it must match even for a local-only package.

Upstream native/system libraries, externally supplied ConPTY files and separately
produced browser assets are not discovered through Cargo events. Keep their own
license/provenance material with those deliveries; do not describe the Cargo
notice as covering unrelated supplied binaries. See the
[threat model](#rmux-threat-model) for the trust boundaries of build inputs,
configuration, IPC, Web Share and diagnostics.

## RMUX threat model

RMUX runs terminal commands with the user's privileges. Its primary assets are
shell access, terminal input/output and scrollback, local files, configuration,
Web Share capabilities, and the integrity of the software delivered to users.
An attacker may be another local account, an untrusted configuration author, a
network relay, a remote share participant, or a supplier of build inputs.

The OS kernel, the user's account, the loaded browser frontend and the release
builder are trusted. A process already running as that user can generally read
their files and control their terminal. This model does not claim isolation
from a compromised account, kernel, browser extension or build machine.

### Named pipes

The Windows pipe namespace crosses process and potentially user boundaries.
Pipe squatting, impersonation and permissive ACLs could redirect clients or
grant terminal access to another account. Managed endpoints use owner-scoped
names and protected owner-only security descriptors. Clients validate the
server's account and integrity level; the pipe's owner and mandatory label
provide the fallback when querying the process token is unavailable. Failure
to establish that identity is an error, not a reason to grant broader access.

The implementation is in `crates/rmux-ipc/src/windows_endpoint_*`,
`listener.rs` and `server_identity_windows.rs`. The endpoint tests cover unsafe
inherited ACLs and state records. These controls do not distinguish a malicious
process with the same account and security identity from a trusted one. Keep
endpoint state private; do not make a pipe broadly accessible to repair a
connection failure. Platform-specific enforcement still needs native Windows
testing; a Unix test is not evidence of a Windows access check.

### Unix sockets

Another local account could replace a socket, traverse a shared directory, or
connect after permissions were broadened. Default managed directories are
owner-only (0700) and sockets use 0600. The managed-directory client guard
rejects symlinks and unexpected ownership/modes and rechecks directory identity
while connecting. Explicit server access grants deliberately cross the default
user boundary: their permissions must be reviewed and revoked when no longer
needed. A custom socket path also needs a trustworthy parent directory.

See `crates/rmux-ipc/src/managed_socket_unix.rs` and
`crates/rmux-server/src/unix_socket*.rs`. Filesystem permissions and peer access
checks reduce cross-user access; they cannot protect against the socket owner
or an administrator. Never expose a local socket through a network bridge as
though possession of its pathname were authentication.

### Web Share

Relays and tunnel providers can observe connection metadata, timing and packet
lengths, drop traffic and deny service. They are not trusted with terminal
plaintext. The hybrid handshake and authenticated channel bind the transcript
and reject unauthenticated, replayed or reordered records. The daemon enforces
operator/spectator permissions, frame limits and queue limits; browser UI alone
is not an authorization boundary.

Operator links are shell credentials. Spectator links disclose terminal output,
which may itself contain secrets. URL fragment tokens are not sent in the
frontend HTTP request, but scripts executing in that frontend can read them.
Use a trusted frontend, avoid sharing links through public logs, and stop a
share to revoke access. Encryption does not protect a compromised endpoint or
frontend and does not prevent traffic analysis or denial of service. See
`docs/web-share.md` and the tests in `crates/rmux-web-crypto` and the server's
Web Share handlers.

### Configuration imports

An imported configuration is executable input to RMUX's command engine, not
inert preferences. Commands, hooks and shell actions can change state or run
with the user's authority. Importing an untrusted file, including one reached
through an include, glob or symlink, grants that author those capabilities.
Review the entire import chain before loading it. Do not treat parse-only
validation as a sandbox or as proof that subsequent execution is safe.

The source-file reader bounds individual input to 16 MiB, a matched set to 256
files and aggregate content to 32 MiB. Parse/read errors are reported through
the configuration engine. These are resource and diagnostic controls, not an
authorization policy for the file's contents. See
`crates/rmux-server/src/handler_scripting/source_files.rs`, `config_engine/`
and the source-file tests. Files changing between review and execution remain
a risk; use a private, controlled configuration directory.

### Diagnostics

Diagnostic output crosses from a local, trusted machine to whoever receives a
support report. `rmux diagnose` summarizes selected environment values and
redacts home-directory prefixes from paths; it does not dump the entire
environment. Configuration diagnostic lines are filtered and bounded. These
transformations are not a universal secret detector: terminal names, custom
paths, configuration messages and user-controlled values can still identify
people or contain sensitive text. Inspect reports before sharing them.

The optional `RMUX_ATTACH_EXIT_LOG` appends lifecycle records to the path supplied
by the user. Its destination must be trusted and private; enabling a log is not
a promise of private permissions for every pre-existing file or parent path.
Do not use a shared or attacker-controlled destination. See
`src/cli/diagnose.rs`, its tests and
`crates/rmux-server/src/diagnostic_log.rs`.

### Build inputs and third-party notices

Cargo build scripts and procedural macros execute on the build host. Treat
dependency sources as executable input even when their code is not part of the
final runtime. Keep the lockfile fixed, enforce `deny.toml` with
`cargo deny check licenses`, and generate notices from the completed Cargo
build events, not from every entry in a lockfile or metadata resolution.
The notice includes observed build dependencies as well as target dependencies;
it does not claim that every compiled unit survives linking or LTO.

The builder, Cargo's metadata/events and the local dependency source files are
trusted inputs to notice generation. A notice is attribution, not an SBOM
attestation or a defense against a compromised builder. Ship it with its
package, retain upstream copyright/license/NOTICE text, and verify package
checksums before distribution. The project's allowlist is a release policy;
meeting it does not remove the obligations of the selected upstream licenses.
