# Security Policy

## Reporting a vulnerability

Report security bugs through GitHub private vulnerability reporting:
https://github.com/Helvesec/rmux/security/advisories/new

You can also email security@rmux.io.

Do not open a public issue for a security bug. Public issues are for normal bugs and feature requests.

In your report, include what you found, the steps to reproduce it, and the version you tested. A proof of concept helps.

## In scope

- The rmux daemon and the CLI.
- The workspace crates (`rmux-core`, `rmux-server`, `rmux-client`, `rmux-pty`, `rmux-ipc`, `rmux-web-crypto`, and the rest).
- The web-share end-to-end encryption: the handshake, the key schedule, and the record layer.
- The local IPC surface (the Unix socket and the Windows named pipe).
- The install scripts at https://rmux.io/install.sh and https://rmux.io/install.ps1.
- The release artifacts and their signatures.

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
