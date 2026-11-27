use super::*;

pub(in crate::tiny_main) struct TinyDisplayMessage {
    pub(in crate::tiny_main) target: Option<Target>,
    pub(in crate::tiny_main) message: Option<String>,
}

pub(in crate::tiny_main) struct TinySendKeys {
    pub(in crate::tiny_main) raw_target: String,
    pub(in crate::tiny_main) keys: Vec<String>,
}

pub(in crate::tiny_main) struct TinySourceFile {
    pub(in crate::tiny_main) paths: Vec<String>,
}

pub(in crate::tiny_main) struct TinyHasSession {
    pub(in crate::tiny_main) target: SessionName,
}

pub(in crate::tiny_main) struct TinyListWindows {
    pub(in crate::tiny_main) target: SessionName,
}

pub(in crate::tiny_main) enum TinyListPanes {
    AllSessions,
    Target {
        target: SessionName,
        target_window_index: Option<u32>,
    },
}

pub(in crate::tiny_main) struct TinyNewWindow {
    pub(in crate::tiny_main) target: SessionName,
    pub(in crate::tiny_main) name: Option<String>,
    pub(in crate::tiny_main) detached: bool,
    pub(in crate::tiny_main) command: Option<Vec<String>>,
    pub(in crate::tiny_main) start_directory: Option<PathBuf>,
}

pub(in crate::tiny_main) struct TinyKillSession {
    pub(in crate::tiny_main) target: SessionName,
}

pub(in crate::tiny_main) struct TinyShowOptions {
    pub(in crate::tiny_main) scope: OptionScopeSelector,
}

pub(in crate::tiny_main) struct TinyRenameWindow {
    pub(in crate::tiny_main) target: WindowTarget,
    pub(in crate::tiny_main) name: String,
}

pub(in crate::tiny_main) struct TinySelectWindow {
    pub(in crate::tiny_main) target: WindowTarget,
}

pub(in crate::tiny_main) struct TinyKillPane {
    pub(in crate::tiny_main) target: PaneTarget,
}

pub(in crate::tiny_main) struct TinyJoinPane {
    pub(in crate::tiny_main) source: PaneTarget,
    pub(in crate::tiny_main) target: PaneTarget,
    pub(in crate::tiny_main) detached: bool,
}

pub(in crate::tiny_main) struct TinySetOption {
    pub(in crate::tiny_main) scope: OptionScopeSelector,
    pub(in crate::tiny_main) option: String,
    pub(in crate::tiny_main) value: String,
}
