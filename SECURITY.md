# Security Policy

## Reporting a vulnerability

Report security bugs through GitHub private vulnerability reporting:
https://github.com/Helvesec/rmux/security/advisories/new

You can also email security@rmux.io.

Do not open a public issue for a security bug. Public issues are for normal bugs and feature requests.

In your report, include what you found, the steps to reproduce it, and the version you tested. A proof of concept helps.

## Verifying releases

Releases starting with `v0.6.5` include `SHA256SUMS`, `SHA256SUMS.sigstore.json`, and GitHub build provenance attestations. The provenance is SLSA Build Level 2. Earlier releases are checksums-only.

Verify an asset attestation with:

```sh
gh attestation verify <asset> --repo Helvesec/rmux
```

Verify the signed checksums with:

```sh
cosign verify-blob \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-identity-regexp 'https://github.com/Helvesec/rmux/.github/workflows/release.yml@refs/tags/v0\.(6|7)\.[0-9]+(-[0-9A-Za-z.-]+)?$' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  SHA256SUMS
```

## In scope

- The rmux daemon and the CLI.
- The workspace crates (`rmux-core`, `rmux-server`, `rmux-client`, `rmux-pty`, `rmux-ipc`, `rmux-web-crypto`, and the rest).
- The web-share end-to-end encryption: the handshake, the key schedule, and the record layer.
- The local IPC surface (the Unix socket and the Windows named pipe).
- The install scripts at https://rmux.io/install.sh and https://rmux.io/install.ps1.
- The release artifacts and their signatures.

## Web-share authentication errors

The normative web-share E2EE protocol specification lives in [docs/specs/web-share-e2ee-protocol-v1.md](docs/specs/web-share-e2ee-protocol-v1.md).

Pre-ready authentication failures use `(4000, "handshake_rejected")` after a uniform delay. A token-authenticated client that omits a required pairing PIN receives `(4008, "pin_required")`; wrong PINs remain on the generic `4000` path.

## Out of scope

- Bugs that need a compromised browser, a malicious extension, or a tampered page. The web-share encryption protects the path through the relay. It does not protect a device that is already compromised.
- Bugs that need a compromised local user account. The daemon trusts the local user by design.
- Theoretical attacks on X25519 or ML-KEM-768 without a working exploit.
- Denial of service against a server you run yourself.
- Issues in third-party tunnel providers.

## Response times

- We reply within 3 working days.
- We send a first assessment within 7 working days.
- We aim to ship a fix within 90 days. Critical issues are faster.

## Coordinated disclosure

Keep the report private until a fix is released. We will credit you in the advisory if you want.

## Supported versions

Security fixes go into the latest release. Update to the newest version before you report.
