<div align="center">

<a href="https://rmux.io">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="share-header-dark.svg">
    <img src="share-header.svg" alt="SHARE" width="500">
  </picture>
</a>

**Web sharing for RMUX terminal panes and sessions.**

[![rmux 0.4.0](https://img.shields.io/badge/rmux-0.4.0-informational.svg)](https://github.com/Helvesec/rmux/releases/tag/v0.4.0)
[![E2EE](https://img.shields.io/badge/E2EE-ChaCha20--Poly1305-success.svg)](#cryptography)
[![Post-quantum](https://img.shields.io/badge/PQ-X25519%20%2B%20ML--KEM--768-blue.svg)](#cryptography)
[![Frontend](https://img.shields.io/badge/frontend-static-lightgrey.svg)](#key-features)

</div>

## Web Multiplex

`rmux web-share` shares any active terminal pane or session directly to a browser client.

Execution stays on your machine. The browser only shows the terminal screen and sends keystrokes back to your local daemon. The web app is static and serverless; terminal traffic stays end-to-end encrypted, including when you use a tunnel.

```sh
# Share current pane over loopback
rmux web-share

# Share a named session
rmux web-share -t work

# Share globally using a public tunnel provider
rmux web-share --tunnel-provider localhost-run
```

## Key Features

- **PTY execution remains local**: The local daemon manages all processes; nothing runs in the cloud.
- **E2E Encrypted**: A hybrid X25519 + ML-KEM-768 key exchange encrypts traffic directly between browser and daemon.
- **Static Frontend**: The web client is pure HTML/JS/WASM, with no backend server. You can self-host it on any CDN or serve it directly from the daemon.
- **Access Control**: Scoped links separate read-only Spectators from read-write Operators, gated by 6-digit pairing PINs.
- **Tunnel-safe traffic**: Tunnel providers forward encrypted frames and cannot read terminal traffic.

## Cryptography

<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="rmux-web-share-crypto-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="rmux-web-share-crypto-light.png">
  <img src="rmux-web-share-crypto-dark.png" alt="RMUX Web Share encryption key schedule" width="850">
</picture>

</div>

Each session negotiates fresh symmetric keys using a hybrid key exchange:
- Ephemeral **X25519** and **ML-KEM-768** (post-quantum) key agreement.
- Handshake binding to the URL token and exact session transcript.

All terminal frames are encrypted using **ChaCha20-Poly1305** directly between the browser and the local daemon.

## Access Control

| Role | Keyboard Input | Default Use Case |
| :--- | :---: | :--- |
| **Operator** | ✅ Enabled | Full interactive shell control |
| **Spectator** | ❌ Disabled | Read-only stream view |

```sh
# Restrict sharing to read-only spectators
rmux web-share --spectator-only

# Configure custom pairing PINs for each role
rmux web-share --pin-operator 123456 --pin-spectator 789012

# Set limits and expiration
rmux web-share --max-spectators 10 --ttl 3600
```

## Tunnels & Custom Domains

You can share over local loopback, a private network, or the public internet:

```sh
# Private sharing over your VPN (Tailscale Serve)
rmux web-share --tunnel-provider tailscale-serve

# Public sharing using built-in SSH tunnel presets
rmux web-share --tunnel-provider localhost-run

# Custom external tunnel address
rmux web-share --tunnel-url https://my-tunnel.example.com

# Custom web client domain (for self-hosting static assets)
rmux web-share --frontend-url https://share.example.com
```

## Comparison

| Tool | Browser | Static frontend | E2EE | PQ | Multiplexer | Self-host | No account | Mobile |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| **RMUX Web Share** | ✅ | ✅ | ✅ ChaCha20 | ✅ X25519 + ML-KEM | ✅ | ✅ | ✅ | ✅ |
| sshx | ✅ | ❌ | ✅ AES | ❌ | ❌ | △ | ✅ | △ |
| tmate | △ | ❌ | ❌ | ❌ | ✅ | △ | ✅ | ❌ |
| ttyd / GoTTY | ✅ | ❌ | ❌ | ❌ | ❌ | ✅ | ✅ | △ |
| Upterm | ❌ | ❌ | ✅ SSH | ❌ | ❌ | ✅ | ✅ | ❌ |
| VS Code Tunnels | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | △ |
| Warp sharing | △ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ |
| Pocketmux / pmux | ✅ | ❌ | ✅ DTLS | ❌ | ✅ | △ | ✅ | ✅ |

`△` means partial, indirect, or not the default path.

## CLI Commands

```sh
# List active shares
rmux web-share list

# Inspect details of a share
rmux web-share lookup <share-id>

# Stop a specific share
rmux web-share stop <share-id>

# Terminate all active shares
rmux web-share off

# Show web-share config
rmux web-share config
```

## Security Model & Threat Boundaries

- **End-to-End Encryption**: Hosts and tunnel providers cannot read terminal payloads.
- **Credential Protection**: Access tokens are stored in the URL fragment (`#t=...`), which browsers do not transmit to the hosting server in HTTP requests.
- **Auditability**: Builds are public and reproducible. If you require absolute supply-chain sovereignty, host the static frontend assets on your own infrastructure and pass `--frontend-url`.
- **Operator Security**: Treat operator URLs as highly sensitive credentials; they grant active input control over your local shell.
