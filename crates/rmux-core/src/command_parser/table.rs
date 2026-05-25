/// Frozen tmux command table entry used for command lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandEntry {
    /// The canonical tmux command name.
    pub name: &'static str,
    /// The exact tmux alias, when the frozen entry declares one.
    pub alias: Option<&'static str>,
}

/// Frozen command inventory from `/opt/rmux/reference/tmux`
/// `31d77e29b6c9fbb07d032018da78db3a8a38d979` `cmd.c:121`.
pub const COMMAND_TABLE: &[CommandEntry] = &[
    CommandEntry {
        name: "attach-session",
        alias: Some("attach"),
    },
    CommandEntry {
        name: "bind-key",
        alias: Some("bind"),
    },
    CommandEntry {
        name: "break-pane",
        alias: Some("breakp"),
    },
    CommandEntry {
        name: "capture-pane",
        alias: Some("capturep"),
    },
    CommandEntry {
        name: "choose-buffer",
        alias: None,
    },
    CommandEntry {
        name: "choose-client",
        alias: None,
    },
    CommandEntry {
        name: "choose-tree",
        alias: None,
    },
    CommandEntry {
        name: "clear-history",
        alias: Some("clearhist"),
    },
    CommandEntry {
        name: "clear-prompt-history",
        alias: Some("clearphist"),
    },
    CommandEntry {
        name: "clock-mode",
        alias: None,
    },
    CommandEntry {
        name: "command-prompt",
        alias: None,
    },
    CommandEntry {
        name: "confirm-before",
        alias: Some("confirm"),
    },
    CommandEntry {
        name: "copy-mode",
        alias: None,
    },
    CommandEntry {
        name: "customize-mode",
        alias: None,
    },
    CommandEntry {
        name: "delete-buffer",
        alias: Some("deleteb"),
    },
    CommandEntry {
        name: "detach-client",
        alias: Some("detach"),
    },
    CommandEntry {
        name: "display-menu",
        alias: Some("menu"),
    },
    CommandEntry {
        name: "display-message",
        alias: Some("display"),
    },
    CommandEntry {
        name: "display-popup",
        alias: Some("popup"),
    },
    CommandEntry {
        name: "display-panes",
        alias: Some("displayp"),
    },
    CommandEntry {
        name: "find-window",
        alias: Some("findw"),
    },
    CommandEntry {
        name: "has-session",
        alias: Some("has"),
    },
    CommandEntry {
        name: "if-shell",
        alias: Some("if"),
    },
    CommandEntry {
        name: "join-pane",
        alias: Some("joinp"),
    },
    CommandEntry {
        name: "kill-pane",
        alias: Some("killp"),
    },
    CommandEntry {
        name: "kill-server",
        alias: None,
    },
    CommandEntry {
        name: "kill-session",
        alias: None,
    },
    CommandEntry {
        name: "kill-window",
        alias: Some("killw"),
    },
    CommandEntry {
        name: "last-pane",
        alias: Some("lastp"),
    },
    CommandEntry {
        name: "last-window",
        alias: Some("last"),
    },
    CommandEntry {
        name: "link-window",
        alias: Some("linkw"),
    },
    CommandEntry {
        name: "list-buffers",
        alias: Some("lsb"),
    },
    CommandEntry {
        name: "list-clients",
        alias: Some("lsc"),
    },
    CommandEntry {
        name: "list-commands",
        alias: Some("lscm"),
    },
    CommandEntry {
        name: "list-keys",
        alias: Some("lsk"),
    },
    CommandEntry {
        name: "list-panes",
        alias: Some("lsp"),
    },
    CommandEntry {
        name: "list-sessions",
        alias: Some("ls"),
    },
    CommandEntry {
        name: "list-windows",
        alias: Some("lsw"),
    },
    CommandEntry {
        name: "load-buffer",
        alias: Some("loadb"),
    },
    CommandEntry {
        name: "lock-client",
        alias: Some("lockc"),
    },
    CommandEntry {
        name: "lock-server",
        alias: Some("lock"),
    },
    CommandEntry {
        name: "lock-session",
        alias: Some("locks"),
    },
    CommandEntry {
        name: "move-pane",
        alias: Some("movep"),
    },
    CommandEntry {
        name: "move-window",
        alias: Some("movew"),
    },
    CommandEntry {
        name: "new-session",
        alias: Some("new"),
    },
    CommandEntry {
        name: "new-window",
        alias: Some("neww"),
    },
    CommandEntry {
        name: "next-layout",
        alias: Some("nextl"),
    },
    CommandEntry {
        name: "next-window",
        alias: Some("next"),
    },
    CommandEntry {
        name: "paste-buffer",
        alias: Some("pasteb"),
    },
    CommandEntry {
        name: "pipe-pane",
        alias: Some("pipep"),
    },
    CommandEntry {
        name: "previous-layout",
        alias: Some("prevl"),
    },
    CommandEntry {
        name: "previous-window",
        alias: Some("prev"),
    },
    CommandEntry {
        name: "refresh-client",
        alias: Some("refresh"),
    },
    CommandEntry {
        name: "rename-session",
        alias: Some("rename"),
    },
    CommandEntry {
        name: "rename-window",
        alias: Some("renamew"),
    },
    CommandEntry {
        name: "resize-pane",
        alias: Some("resizep"),
    },
    CommandEntry {
        name: "resize-window",
        alias: Some("resizew"),
    },
    CommandEntry {
        name: "respawn-pane",
        alias: Some("respawnp"),
    },
    CommandEntry {
        name: "respawn-window",
        alias: Some("respawnw"),
    },
    CommandEntry {
        name: "rotate-window",
        alias: Some("rotatew"),
    },
    CommandEntry {
        name: "run-shell",
        alias: Some("run"),
    },
    CommandEntry {
        name: "save-buffer",
        alias: Some("saveb"),
    },
    CommandEntry {
        name: "select-layout",
        alias: Some("selectl"),
    },
    CommandEntry {
        name: "select-pane",
        alias: Some("selectp"),
    },
    CommandEntry {
        name: "select-window",
        alias: Some("selectw"),
    },
    CommandEntry {
        name: "send-keys",
        alias: Some("send"),
    },
    CommandEntry {
        name: "send-prefix",
        alias: None,
    },
    CommandEntry {
        name: "server-access",
        alias: None,
    },
    CommandEntry {
        name: "set-buffer",
        alias: Some("setb"),
    },
    CommandEntry {
        name: "set-environment",
        alias: Some("setenv"),
    },
    CommandEntry {
        name: "set-hook",
        alias: None,
    },
    CommandEntry {
        name: "set-option",
        alias: Some("set"),
    },
    CommandEntry {
        name: "set-window-option",
        alias: Some("setw"),
    },
    CommandEntry {
        name: "show-buffer",
        alias: Some("showb"),
    },
    CommandEntry {
        name: "show-environment",
        alias: Some("showenv"),
    },
    CommandEntry {
        name: "show-hooks",
        alias: None,
    },
    CommandEntry {
        name: "show-messages",
        alias: Some("showmsgs"),
    },
    CommandEntry {
        name: "show-options",
        alias: Some("show"),
    },
    CommandEntry {
        name: "show-prompt-history",
        alias: Some("showphist"),
    },
    CommandEntry {
        name: "show-window-options",
        alias: Some("showw"),
    },
    CommandEntry {
        name: "source-file",
        alias: Some("source"),
    },
    CommandEntry {
        name: "split-window",
        alias: Some("splitw"),
    },
    CommandEntry {
        name: "start-server",
        alias: Some("start"),
    },
    CommandEntry {
        name: "suspend-client",
        alias: Some("suspendc"),
    },
    CommandEntry {
        name: "swap-pane",
        alias: Some("swapp"),
    },
    CommandEntry {
        name: "swap-window",
        alias: Some("swapw"),
    },
    CommandEntry {
        name: "switch-client",
        alias: Some("switchc"),
    },
    CommandEntry {
        name: "unbind-key",
        alias: Some("unbind"),
    },
    CommandEntry {
        name: "unlink-window",
        alias: Some("unlinkw"),
    },
    CommandEntry {
        name: "wait-for",
        alias: Some("wait"),
    },
    CommandEntry {
        name: "web-share",
        alias: None,
    },
];
