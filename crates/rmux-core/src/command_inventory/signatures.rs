//! Command signatures advertised by `list-commands`.

pub(super) const LIST_COMMAND_SIGNATURES: &[(&str, &str)] = &[
    (
        "attach-session",
        "(attach) [-dErx] [-c working-directory] [-f flags] [-t target-session]",
    ),
    (
        "bind-key",
        "(bind) [-nr] [-T key-table] [-N note] key [command [argument ...]]",
    ),
    (
        "break-pane",
        "(breakp) [-abdP] [-F format] [-n window-name] [-s src-pane] [-t dst-window]",
    ),
    (
        "capture-pane",
        "(capturep) [-aCeFHJLMNpPqT] [-b buffer-name] [-E end-line] [-S start-line] [-t target-pane]",
    ),
    (
        "choose-buffer",
        "[-NrZ] [-F format] [-f filter] [-K key-format] [-O sort-order] [-t target-pane] [template]",
    ),
    (
        "choose-client",
        "[-NrZ] [-F format] [-f filter] [-K key-format] [-O sort-order] [-t target-pane] [template]",
    ),
    (
        "choose-tree",
        "[-GNrswZ] [-F format] [-f filter] [-K key-format] [-O sort-order] [-t target-pane] [template]",
    ),
    ("clear-history", "(clearhist) [-H] [-t target-pane]"),
    ("clear-prompt-history", "(clearphist) [-T prompt-type]"),
    ("clock-mode", "[-t target-pane]"),
    (
        "command-prompt",
        "[-1CbeFiklN] [-I inputs] [-p prompts] [-t target-client] [-T prompt-type] [template]",
    ),
    (
        "confirm-before",
        "(confirm) [-by] [-c confirm-key] [-p prompt] [-t target-client] command",
    ),
    ("copy-mode", "[-deHMqSu] [-s src-pane] [-t target-pane]"),
    ("customize-mode", "[-NZ] [-F format] [-f filter] [-t target-pane]"),
    ("delete-buffer", "(deleteb) [-b buffer-name]"),
    (
        "detach-client",
        "(detach) [-aP] [-E shell-command] [-s target-session] [-t target-client]",
    ),
    (
        "display-menu",
        "(menu) [-MO] [-b border-lines] [-c target-client] [-C starting-choice] [-H selected-style] [-s style] [-S border-style] [-t target-pane] [-T title] [-x position] [-y position] name [key] [command] ...",
    ),
    (
        "display-message",
        "(display) [-aCIlNpv] [-c target-client] [-d delay] [-F format] [-t target-pane] [message]",
    ),
    (
        "display-popup",
        "(popup) [-BCEkN] [-b border-lines] [-c target-client] [-d start-directory] [-e environment] [-h height] [-s style] [-S border-style] [-t target-pane] [-T title] [-w width] [-x position] [-y position] [shell-command [argument ...]]",
    ),
    (
        "display-panes",
        "(displayp) [-bN] [-d duration] [-t target-client] [template]",
    ),
    (
        "find-window",
        "(findw) [-CiNrTZ] [-t target-pane] match-string",
    ),
    ("has-session", "(has) [-t target-session]"),
    (
        "if-shell",
        "(if) [-bF] [-t target-pane] shell-command command [command]",
    ),
    (
        "join-pane",
        "(joinp) [-bdfhv] [-l size] [-s src-pane] [-t dst-pane]",
    ),
    ("kill-pane", "(killp) [-a] [-t target-pane]"),
    ("kill-server", ""),
    ("kill-session", "[-aCg] [-t target-session]"),
    ("kill-window", "(killw) [-a] [-t target-window]"),
    ("last-pane", "(lastp) [-deZ] [-t target-window]"),
    ("last-window", "(last) [-t target-session]"),
    (
        "link-window",
        "(linkw) [-abdk] [-s src-window] [-t dst-window]",
    ),
    ("list-buffers", "(lsb) [-F format] [-f filter] [-O order]"),
    (
        "list-clients",
        "(lsc) [-F format] [-f filter] [-O order][-t target-session]",
    ),
    ("list-commands", "(lscm) [-F format] [command]"),
    (
        "list-keys",
        "(lsk) [-1aNr] [-F format] [-O order] [-P prefix-string][-T key-table] [key]",
    ),
    (
        "list-panes",
        "(lsp) [-asr] [-F format] [-f filter] [-O order][-t target-window]",
    ),
    ("list-sessions", "(ls) [-r] [-F format] [-f filter] [-O order]"),
    (
        "list-windows",
        "(lsw) [-ar] [-F format] [-f filter] [-O order][-t target-session]",
    ),
    (
        "load-buffer",
        "(loadb) [-b buffer-name] [-t target-client] path",
    ),
    ("lock-client", "(lockc) [-t target-client]"),
    ("lock-server", "(lock) "),
    ("lock-session", "(locks) [-t target-session]"),
    (
        "move-pane",
        "(movep) [-bdfhv] [-l size] [-s src-pane] [-t dst-pane]",
    ),
    (
        "move-window",
        "(movew) [-abdkr] [-s src-window] [-t dst-window]",
    ),
    (
        "new-session",
        "(new) [-AdDEPX] [-c start-directory] [-e environment] [-F format] [-f flags] [-n window-name] [-s session-name] [-t target-session] [-x width] [-y height] [shell-command [argument ...]]",
    ),
    (
        "new-window",
        "(neww) [-abdkPS] [-c start-directory] [-e environment] [-F format] [-n window-name] [-t target-window] [shell-command [argument ...]]",
    ),
    ("next-layout", "(nextl) [-t target-window]"),
    ("next-window", "(next) [-a] [-t target-session]"),
    (
        "paste-buffer",
        "(pasteb) [-dprS] [-s separator] [-b buffer-name] [-t target-pane]",
    ),
    ("pipe-pane", "(pipep) [-IOo] [-t target-pane] [shell-command]"),
    ("previous-layout", "(prevl) [-t target-window]"),
    ("previous-window", "(prev) [-a] [-t target-session]"),
    (
        "refresh-client",
        "(refresh) [-cDlLRSU] [-A pane:state] [-B name:what:format] [-C XxY] [-f flags] [-r pane:report] [-t target-client] [adjustment]",
    ),
    ("rename-session", "(rename) [-t target-session] new-name"),
    ("rename-window", "(renamew) [-t target-window] new-name"),
    (
        "resize-pane",
        "(resizep) [-DLMRTUZ] [-x width] [-y height] [-t target-pane] [adjustment]",
    ),
    (
        "resize-window",
        "(resizew) [-aADLRU] [-x width] [-y height] [-t target-window] [adjustment]",
    ),
    (
        "respawn-pane",
        "(respawnp) [-k] [-c start-directory] [-e environment] [-t target-pane] [shell-command [argument ...]]",
    ),
    (
        "respawn-window",
        "(respawnw) [-k] [-c start-directory] [-e environment] [-t target-window] [shell-command [argument ...]]",
    ),
    ("rotate-window", "(rotatew) [-DUZ] [-t target-window]"),
    (
        "run-shell",
        "(run) [-bCE] [-c start-directory] [-d delay] [-t target-pane] [shell-command [argument ...]]",
    ),
    ("save-buffer", "(saveb) [-a] [-b buffer-name] path"),
    ("select-layout", "(selectl) [-Enop] [-t target-pane] [layout-name]"),
    (
        "select-pane",
        "(selectp) [-DdeLlMmRUZ] [-T title] [-t target-pane]",
    ),
    ("select-window", "(selectw) [-lnpT] [-t target-window]"),
    (
        "send-keys",
        "(send) [-FHKlMRX] [-c target-client] [-N repeat-count] [-t target-pane] [key ...]",
    ),
    ("send-prefix", "[-2] [-t target-pane]"),
    ("server-access", "[-adlrw] [-t target-pane] [user]"),
    (
        "set-buffer",
        "(setb) [-aw] [-b buffer-name] [-n new-buffer-name] [-t target-client] [data]",
    ),
    (
        "set-environment",
        "(setenv) [-Fhgru] [-t target-session] variable [value]",
    ),
    ("set-hook", "[-agpRuw] [-t target-pane] hook [command]"),
    (
        "set-option",
        "(set) [-aFgopqsuUw] [-t target-pane] option [value]",
    ),
    (
        "set-window-option",
        "(setw) [-aFgoqu] [-t target-window] option [value]",
    ),
    ("show-buffer", "(showb) [-b buffer-name]"),
    (
        "show-environment",
        "(showenv) [-hgs] [-t target-session] [variable]",
    ),
    ("show-hooks", "[-gpw] [-t target-pane] [hook]"),
    ("show-messages", "(showmsgs) [-JT] [-t target-client]"),
    (
        "show-options",
        "(show) [-AgHpqsvw] [-t target-pane] [option]",
    ),
    ("show-prompt-history", "(showphist) [-T prompt-type]"),
    (
        "show-window-options",
        "(showw) [-gv] [-t target-window] [option]",
    ),
    ("source-file", "(source) [-Fnqv] [-t target-pane] path ..."),
    (
        "split-window",
        "(splitw) [-bdefhIPvZ] [-c start-directory] [-e environment] [-F format] [-l size] [-t target-pane][shell-command]",
    ),
    ("start-server", "(start) "),
    ("suspend-client", "(suspendc) [-t target-client]"),
    ("swap-pane", "(swapp) [-dDUZ] [-s src-pane] [-t dst-pane]"),
    ("swap-window", "(swapw) [-d] [-s src-window] [-t dst-window]"),
    (
        "switch-client",
        "(switchc) [-ElnprZ] [-c target-client] [-t target-session] [-T key-table] [-O order]",
    ),
    ("unbind-key", "(unbind) [-anq] [-T key-table] key"),
    ("unlink-window", "(unlinkw) [-k] [-t target-window]"),
    ("wait-for", "(wait) [-L|-S|-U] channel"),
    ("capabilities", "[--human|--json]"),
    ("claude", "[install-skill|claude-args...]"),
    ("doctor", "tmux-dropin"),
    ("setup", "tmux-shim"),
    (
        "wait-pane",
        "[-t target-pane] [--text text|--next-text text|--visible-text text|--quiet|--pane-exit|--get-by-text text] [--stable-for duration] [--timeout duration] [--json]",
    ),
    (
        "pane-snapshot",
        "[-t target-pane] [--json] [--style] [--region row,col,rows,cols]",
    ),
    ("stream-pane", "[-t target-pane] [--raw|--lines]"),
    (
        "collect-pane-output",
        "[-t target-pane] --until-pane-exit --max-bytes bytes [--json]",
    ),
    ("locator", "[-t target-pane] --get-by-text text [--json]"),
    (
        "expect-pane",
        "[-t target-pane] --get-by-text text [--visible|--hidden|--count count] [--json]",
    ),
    (
        "find-panes",
        "[--title title] [--title-prefix prefix] [--current-command command] [--cwd path] [--json]",
    ),
    (
        "find-sessions",
        "[--name name] [--name-prefix prefix] [--json]",
    ),
    ("broadcast-keys", "-t target-pane... [-l] -- key ..."),
    (
        "with-session",
        "session-name [--kill-on-owner-exit] [--ttl duration] -- command ...",
    ),
    (
        "web-share",
        "[-lX] [-K share-id] [disconnect share-id] [--config] [--lookup share-id] [--operator-only|--spectator-only] [--ttl seconds|--expires-at RFC3339] [--kill-session-on-expire] [--max-operators count] [--max-spectators count] [--frontend-url url] [--tunnel-url url|--tunnel-provider provider] [--no-navbar] [--no-disclaimer] [--hide-viewers] [--theme user|light|dark] [--no-pin] [-t pane|session]",
    ),
];
