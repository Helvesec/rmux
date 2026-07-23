# Human-friendly starter config

RMUX supports native local runtime backends on Linux, macOS, and Windows. It provides tmux-style commands and many tmux-compatible defaults, but it is not a byte-for-byte tmux clone.

This guide shows an ergonomic interactive profile for people who want normal terminal selection by default, easier cwd-preserving split bindings, and explicit clipboard hooks.

## Starter config

See [`docs/examples/human-friendly.conf`](examples/human-friendly.conf) for the full file.

```tmux
# Safer prefix for heavy shell work.
set -g prefix C-a
unbind C-b
bind C-a send-prefix

# Native selection works like a normal terminal until you opt into pane mouse mode.
set -g mouse off
bind T if-shell -F '#{mouse}' 'set -g mouse off ; display-message "mouse OFF: native terminal selection enabled"' 'set -g mouse on ; display-message "mouse ON: pane mouse mode enabled"'

# Quality-of-life.
set -g history-limit 100000
set -g renumber-windows on
set -g base-index 1
setw -g pane-base-index 1
setw -g mode-keys vi
set -g status-keys vi

# Easier split bindings for keyboard layouts where %, ", or | are awkward.
bind % split-window -h -c "#{pane_current_path}"
bind '"' split-window -v -c "#{pane_current_path}"
bind v split-window -h -c "#{pane_current_path}"
bind b split-window -v -c "#{pane_current_path}"
bind c new-window -c "#{pane_current_path}"

# Pick one clipboard command for your platform, then uncomment it.
# set -s copy-command 'clip.exe'
# set -s copy-command 'pbcopy'
# set -s copy-command 'wl-copy'
# set -s copy-command 'xclip -selection clipboard'
bind [ copy-mode
bind -T copy-mode-vi v send-keys -X begin-selection
bind -T copy-mode-vi y send-keys -X copy-pipe-and-cancel
bind -T copy-mode-vi C-c send-keys -X copy-pipe-and-cancel
```

## Why `mouse off` is the ergonomic default here

When RMUX mouse mode is on, the multiplexer receives mouse drag and double-click events. That enables pane-aware selection, resizing, and copy-mode interactions, but it also means native terminal selection no longer behaves like a plain shell.

A common compromise for interactive use is:

- keep `mouse off` by default so drag and double-click selection feel normal
- toggle it on only when you want pane mouse behavior with `prefix + T`

Many terminals also offer a modifier such as `Shift` plus drag to force native selection even when the multiplexer has mouse mode enabled.

## Cwd-preserving splits

`new-window -c "#{pane_current_path}"` and `split-window -c "#{pane_current_path}"` start new shells in the active pane's current directory. The starter config applies this to both the classic tmux-compatible split bindings and the keyboard-layout friendly alternatives.

## Keyboard-layout friendly splits

The classic tmux-compatible split bindings remain important:

- `prefix + %` for left/right panes
- `prefix + "` for top/bottom panes

For users on keyboard layouts where those keys are awkward, it is useful to add plain-letter alternatives:

- `prefix + v` for left/right panes
- `prefix + b` for top/bottom panes

Keeping both bindings means the session stays familiar to tmux users while also becoming easier to operate on non-US keyboards.

## Copying text

With `copy-command` configured, copy-mode actions can send text directly to your platform clipboard.

Common clipboard commands:

- Windows: `clip.exe`
- macOS: `pbcopy`
- Linux Wayland: `wl-copy`
- Linux X11: `xclip -selection clipboard`

`set-clipboard` defaults to `external`, matching tmux. That mode allows
RMUX-initiated copies, such as `set-buffer -w`, to reach the outer terminal,
but deliberately ignores OSC 52 clipboard writes emitted by programs running
inside panes. To let trusted pane applications update the clipboard, opt in
explicitly:

```tmux
set -s set-clipboard on
```

The attached terminal (including a terminal reached over SSH) must also support
OSC 52. Enabling `on` lets pane output replace the outer clipboard, so keep the
safer `external` default when that trust is not appropriate.

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

If you only see one shell after creating a new window, nothing is broken. You opened a new window, not a split pane.
