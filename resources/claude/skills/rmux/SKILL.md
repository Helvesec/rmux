---
name: rmux
description: Guide for using RMUX with Claude Code, including the tmux-compatible CLI, agent automation waits, the typed SDK, browser web-share, and the rmux claude launcher.
disable-model-invocation: true
---

# RMUX

RMUX is a Rust terminal multiplexer with a tmux-compatible CLI, a typed SDK,
browser web-share, and a Claude Code launcher. Prefer checking the installed
binary instead of assuming a version:

```sh
rmux -V
rmux list-commands
rmux list-commands send-keys
```

If RMUX is missing, suggest installing the published package for the user's
platform or `cargo install rmux --locked`.

This repository ships this source copy at `resources/claude/skills/rmux/SKILL.md`
so the project can package and install it without keeping a hidden `.claude`
directory in the source tree. To use the guidance, run
`rmux claude install-skill`; it installs a user-level copy under
`~/.claude/skills/rmux` on Linux/macOS or `%USERPROFILE%\.claude\skills\rmux`
on Windows.

## Claude Code

Use `rmux claude [args...]` to run Claude Code inside an RMUX workspace with
tmux teammate mode enabled.

```sh
rmux claude
rmux claude --dangerously-skip-permissions
```

RMUX passes `--teammate-mode tmux` to Claude and scopes a private `tmux` shim to
the Claude process. The shim lets Claude's tmux-compatible teammate commands
drive RMUX panes without replacing the user's system tmux. Set
`RMUX_DISABLE_TMUX_SHIM=1` only when the user explicitly wants to disable that
shim.

For reliable Claude Code workflows, prefer `rmux claude` over starting a plain
daemon from inside an arbitrary Claude shell. On Unix, if the user is not using
`rmux claude` and their daemon is killed when Claude recycles a shell, starting
the daemon with `setsid rmux new-session -d -s NAME` can detach it from that
shell. `setsid` is Unix-only; on Windows, use `rmux claude`.

## Agent Automation

Avoid blind sleeps. Send input with a bounded wait and inspect output when
needed:

```sh
rmux new-session -d -s work
rmux send-keys -t work --wait quiet --stable-for 500ms --timeout 2m -- 'cargo test' Enter
rmux capture-pane -t work -p
```

Recommended waits:

- `--wait quiet` waits for output to settle and is the default choice for
  builds, tests, and shell commands with unknown final text.
- `--wait-next-text TEXT` waits for new output only and avoids matching old
  scrollback.
- `--wait-visible-text TEXT` waits for rendered visible text.
- `--wait-pane-exit` is for one-shot pane processes that are expected to exit.
- Always pair waits with `--timeout`.

When using `send-keys` with any `--wait*` flag, put `--` before the payload
keys. `--wait-text` and `--wait-next-text` observe raw PTY output, including
shell echo. If the marker appears in the command itself, prefer `--wait quiet`
or disable shell echo first.

Useful automation commands:

```sh
rmux wait-pane -t work --quiet --timeout 30s
rmux stream-pane -t work --lines
rmux collect-pane-output -t work --until-pane-exit --max-bytes 1048576
```

## CLI Basics

RMUX mirrors tmux syntax for common session, window, and pane operations:

```sh
rmux new-session -d -s NAME
rmux attach-session -t NAME
rmux list-sessions
rmux split-window -h -t NAME
rmux split-window -v -t NAME
rmux list-panes -t NAME
rmux kill-session -t NAME
rmux kill-server
```

Use `rmux list-commands COMMAND` to confirm the exact tmux-compatible flags for
the installed version.

## Web Share

Use `rmux web-share` to open an existing pane or session in a browser. Operator
links can send input; spectator links are read-only.

```sh
rmux web-share
rmux web-share -t work
rmux web-share --spectator-only
rmux web-share --operator-only
rmux web-share --tunnel-provider localhost-run
rmux web-share --no-pin
rmux web-share --ttl 3600
rmux web-share list
rmux web-share stop SHARE_ID
```

For public read-only demos, prefer `--spectator-only` and put external
per-client throttling at the tunnel, CDN, or reverse proxy layer.

## SDK

RMUX exposes typed SDKs. The Rust crate is `rmux-sdk`; Python is `librmux`;
TypeScript is `@rmux/sdk`. Prefer the SDK for structured automation instead of
parsing terminal text when the task is programmatic.

Rust shape:

```rust
use rmux_sdk::{EnsureSession, EnsureSessionPolicy, Rmux, SessionName};

let rmux = Rmux::builder().connect_or_start().await?;
let session = rmux
    .ensure_session(
        EnsureSession::named(SessionName::new("work")?)
            .policy(EnsureSessionPolicy::CreateOrReuse)
            .detached(true),
    )
    .await?;

let pane = session.pane(0, 0);
pane.send_text("printf 'ready\\n'\n").await?;
pane.wait_for_text("ready").await?;
let snapshot = pane.snapshot().await?;
println!("{}", snapshot.visible_text());
```

Key SDK reminders:

- `send_text` sends text; include a trailing newline when the shell should run
  the command.
- `send_key` sends a named key such as `Enter` or `C-c`.
- Prefer daemon-side waits such as `wait_for_text_next` when available.
- Killing the last session can shut down the daemon; reconnect before more
  operations.
