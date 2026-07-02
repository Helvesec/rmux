# Claude Code integration

RMUX ships an official Claude Code skill at:

```text
resources/claude/skills/rmux/SKILL.md
```

RMUX keeps the source copy outside any hidden `.claude` source-tree directory.
Install it into your Claude Code profile when you want Claude to discover RMUX
guidance.

Install or update the user-level skill:

```sh
rmux claude install-skill
```

The command writes:

- Linux/macOS: `~/.claude/skills/rmux/SKILL.md`
- Windows: `%USERPROFILE%\.claude\skills\rmux\SKILL.md`

The installed copy is generated from the repository source skill:

```text
resources/claude/skills/rmux/SKILL.md
```

Use `rmux claude [args...]` to start Claude Code inside an RMUX workspace with
tmux teammate mode enabled. The installed skill teaches Claude when to use
`rmux claude`, `send-keys --wait`, `capture-pane`, `web-share`, and the typed
SDKs.
