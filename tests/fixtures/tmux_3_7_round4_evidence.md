# tmux 3.7b round4 divergence evidence

Tracked fixture for ledger entries C-D37 through C-D48. The original probes were
recorded on July 7, 2026 against the pinned `tmux 3.7b` oracle and the matching
RMUX build under review.

## C-D37 float-flag expression comparisons

Commands:

- `tmux -L r4 -f /dev/null display-message -p '#{e|==|f:5,5}'`
- `tmux -L r4 -f /dev/null display-message -p '#{e|!=|f:5,6}'`
- `tmux -L r4 -f /dev/null display-message -p '#{e|<|f:5,6}'`
- `tmux -L r4 -f /dev/null display-message -p '#{e|<=|f:5,5}'`
- `tmux -L r4 -f /dev/null display-message -p '#{e|>|f:6,5}'`
- `tmux -L r4 -f /dev/null display-message -p '#{e|>=|f:5,5}'`

tmux exited 0 with stdout `1.00` for each command. RMUX exited 0 with stdout
`1` for each command.

## C-D38 expression operands with embedded spaces

`tmux -L r4 -f /dev/null display-message -p '#{e|+|: 5 , 3 }'` exited 0 with
empty stdout. RMUX exited 0 with stdout `8`.

## C-D39 split-window extension flags

The `split-window -k` behavior was re-probed on July 15, 2026 against tmux
3.7b. With global `remain-on-exit` reporting `off`, an `exit 7` split created
with `-k` remained listed with `pane_dead=1` and `pane_dead_status=7`;
`show-options -p` reported `remain-on-exit key`. An otherwise identical split
without `-k` disappeared. RMUX now pins that scoped behavior across direct CLI,
`source-file`, and server-queue entry paths.

For `split-window -m`, `-s`, `-S`, and `-R`, tmux parsed far enough to report
`expects an argument`. RMUX continues to report analogous unknown-flag errors
for those unimplemented surfaces.

## C-D40 list size ordering

In a three-pane session, tmux
`list-panes -O size -F '#{pane_index}:#{pane_width}x#{pane_height}'` printed
`1:59x5`, `2:20x24`, `0:59x18`. RMUX printed `2:20x24`, `1:59x5`, `0:59x18`.

## C-D41 refresh-client subscription flags

Detached tmux invocations for `refresh-client -A %0:foo`,
`refresh-client -B name:what:format`, and `refresh-client -r pane:fmt` exited 1
with `no current client`. RMUX exited 1 with explicit unsupported-feature
messages for `-A`, `-B`, and `-r`.

## C-D42 respawn-pane without a command

In a `remain-on-exit` session with a dead `true` pane, tmux
`respawn-pane -t w:1.0` exited 0 and
`display-message -p -t w:1.0 '#{pane_current_command}'` printed `true`. RMUX
exited 0 and printed `bash`.

## Resolved C-D43 control-mode attach cursor

After sending `printf old`, `tmux -C -L r4 -f /dev/null attach -t w` produced
the control-mode begin/end/session-changed/exit sequence and no `%output`.
RMUX now captures each existing pane's current output sequence at the attach
boundary and likewise emits no historical `%output`; output produced after the
attach remains live. A newly created session deliberately starts from its
oldest retained cursor so startup output cannot race its first subscription.

## C-D44 shutdown hook run-shell delivery

Reproduction, measured July 8, 2026 with the pinned tmux 3.7b oracle and
`target/debug/rmux`:

```sh
new-session -d -s victim 'sleep 30'
set-hook -g session-closed \
  "run-shell \"printf '%s\n' session-closed >> '$out'\""
kill-session -t victim
```

With that single-session shutdown, tmux wrote:

```text
session-closed
```

RMUX wrote no marker before the daemon exited. Round4 intentionally did not
change shutdown draining.

## C-D45 startup config messages

With a startup config containing `display-message hello`, tmux pty startup
showed `/tmp/r4-startup-...conf:1: hello`. RMUX attached to the created session
without rendering that early config status message.

## C-D46 mouse placeholder targets outside mouse events

tmux `select-window -t=` and `kill-window -t=` exited 1 with `no mouse target`.
RMUX exited 1 with `invalid session: ` for both commands.

## C-D47 kill-window last-window CLI fallback

Reproduction, measured July 8, 2026 with the pinned tmux 3.7b oracle and
`target/debug/rmux`:

```sh
new-session -d -s keep 'sleep 30'
new-session -d -s victim 'sleep 30'
set-hook -g window-unlinked \
  "run-shell \"printf '%s\n' window-unlinked >> '$out'\""
set-hook -g session-closed \
  "run-shell \"printf '%s\n' session-closed >> '$out'\""
kill-window -t victim:0
```

tmux wrote:

```text
window-unlinked
session-closed
```

RMUX wrote:

```text
session-closed
window-unlinked
```

## C-D48 queued attach terminal-exit banners

`attach -t w \; detach-client` exited 0 in both tools; tmux transcript length
was 539 and included `[detached (from session w)]`, while RMUX transcript length
was 0. `attach -t w \; kill-server` exited 1 and left no server in both tools;
tmux included `[server exited]`, while RMUX transcript length was 0.
