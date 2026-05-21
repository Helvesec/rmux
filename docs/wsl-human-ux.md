# Human-friendly WSL starter config

RMUX is tmux-compatible by design, which is great for existing tmux users and for automation. On WSL, however, a few defaults can feel surprising for people who are treating RMUX like an everyday interactive terminal:

- enabling `mouse` lets RMUX capture drag and double-click events instead of the terminal doing native text selection
- `%`, `"`, and `|` can be awkward on PT-BR and other keyboard layouts
- many users expect new panes and windows to start in the current working directory
- clipboard integration is much nicer when WSL explicitly forwards copies to `clip.exe`

This guide keeps RMUX defaults intact and shows one ergonomic user config you can drop into `~/.config/rmux/rmux.conf` or `~/.rmux.conf`.

## Starter config

See [`docs/examples/wsl-human.conf`](examples/wsl-human.conf) for the full file.

```tmux
# Safer prefix for heavy shell work.
set -g prefix C-a
unbind C-b
bind C-a send-prefix

# Native selection works like a normal terminal until you opt into pane mouse mode.
set -g mouse off
bind T if-shell -F '#{mouse}' 'set -g mouse off \; display-message "mouse OFF: native terminal selection enabled"' 'set -g mouse on \; display-message "mouse ON: pane mouse mode enabled"'

# Keep cwd when opening new panes or windows.
bind % split-window -h -c "#{pane_current_path}"
bind '"' split-window -v -c "#{pane_current_path}"
bind c new-window -c "#{pane_current_path}"

# Easier split bindings for keyboard layouts where %, ", or | are awkward.
bind v split-window -h -c "#{pane_current_path}"
bind b split-window -v -c "#{pane_current_path}"

# WSL clipboard integration.
set -s copy-command '/mnt/c/WINDOWS/System32/clip.exe'
bind [ copy-mode
bind -T copy-mode-vi v send-keys -X begin-selection
bind -T copy-mode-vi y send-keys -X copy-pipe-and-cancel
bind -T copy-mode-vi C-c send-keys -X copy-pipe-and-cancel
```

## Why `mouse off` is the ergonomic default here

When RMUX mouse mode is on, the multiplexer receives mouse drag and double-click events. That is what enables pane-aware selection, resizing, and copy-mode interactions, but it also means native terminal selection no longer behaves like a plain shell.

A common compromise for interactive WSL usage is:

- keep `mouse off` by default so drag and double-click selection feel normal
- toggle it on only when you want pane mouse behavior with `prefix + T`

Many terminals also offer a modifier such as `Shift` plus drag to force native selection even when the multiplexer has mouse mode enabled.

## Keyboard-layout friendly splits

The classic tmux-compatible split bindings remain important:

- `prefix + %` for left/right panes
- `prefix + "` for top/bottom panes

For users on PT-BR and similar layouts, it is useful to add plain-letter alternatives that do the same thing:

- `prefix + v` for left/right panes
- `prefix + b` for top/bottom panes

Keeping both bindings means the session stays familiar to tmux users while also becoming easier to operate on non-US keyboards.

## Copying text on WSL

With `copy-command` pointed at `clip.exe`, copy-mode actions can send text directly to the Windows clipboard.

One practical flow is:

1. `prefix + [` to enter copy-mode
2. move with arrows or `h/j/k/l`
3. press `v` to begin selection
4. press `y`, `Enter`, or a custom `C-c` binding to copy and exit

This keeps shell `Ctrl+C` behavior unchanged outside copy-mode while still giving a fast copy shortcut inside copy-mode.

## Window vs pane mental model

If you come from a regular terminal, the biggest usability confusion is usually this:

- `new-window` creates a new tab-like screen
- `split-window` creates another visible pane in the current screen

If you only see one shell after creating a new window, nothing is broken — you opened a new window, not a split pane.
