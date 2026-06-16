use std::collections::{BTreeMap, BTreeSet};

use rmux_core::KeyCode;
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{PaneTarget, SessionName, Target, WindowTarget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ModeTreeKind {
    Tree,
    Buffer,
    Client,
    Customize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreviewMode {
    Off,
    Big,
    Normal,
}

impl PreviewMode {
    pub(super) fn cycle(self) -> Self {
        match self {
            Self::Off => Self::Big,
            Self::Big => Self::Normal,
            Self::Normal => Self::Off,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TreeDepth {
    Session,
    Window,
    Pane,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SortOrder {
    Index,
    Name,
    Activity,
    Creation,
    Size,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SearchState {
    pub(super) value: String,
    pub(super) direction: SearchDirection,
}

#[derive(Debug, Clone)]
pub(in crate::handler) struct ModeTreeClientState {
    pub(super) kind: ModeTreeKind,
    pub(super) session_name: SessionName,
    pub(super) host_pane: Option<PaneTarget>,
    pub(super) preview_mode: PreviewMode,
    pub(super) row_format: Option<String>,
    pub(super) filter_format: Option<String>,
    pub(super) filter_text: Option<String>,
    pub(super) key_format: String,
    pub(super) template: Option<String>,
    pub(super) search: Option<SearchState>,
    pub(super) tagged: BTreeSet<String>,
    pub(super) expanded: BTreeSet<String>,
    pub(super) selected_id: Option<String>,
    pub(super) scroll: usize,
    pub(super) preview_scroll: usize,
    pub(super) sort_order: Option<SortOrder>,
    pub(super) order_seq: Vec<SortOrder>,
    pub(super) reversed: bool,
    pub(super) tree_depth: TreeDepth,
    pub(super) show_all_group_members: bool,
    pub(super) auto_accept: bool,
    pub(in crate::handler) zoom_restore: Option<PaneTarget>,
    pub(super) last_list_rows: usize,
}

#[derive(Debug, Clone)]
pub(in crate::handler) struct ParsedModeTreeCommand {
    pub(super) kind: ModeTreeKind,
    pub(super) target: Option<String>,
    pub(super) preview_mode: PreviewMode,
    pub(super) row_format: Option<String>,
    pub(super) filter_format: Option<String>,
    pub(super) key_format: Option<String>,
    pub(super) template: Option<String>,
    pub(super) sort_order: Option<SortOrder>,
    pub(super) reversed: bool,
    pub(super) tree_depth: TreeDepth,
    pub(super) show_all_group_members: bool,
    pub(super) auto_accept: bool,
    pub(super) zoom: bool,
}

#[derive(Debug, Clone)]
pub(super) struct ModeTreeBuild {
    pub(super) items: BTreeMap<String, ModeTreeItem>,
    pub(super) roots: Vec<String>,
    pub(super) order: Vec<String>,
    pub(super) visible: Vec<String>,
    pub(super) no_matches: bool,
}

#[derive(Debug, Clone)]
pub(super) struct ModeTreeItem {
    pub(super) id: String,
    pub(super) parent: Option<String>,
    pub(super) children: Vec<String>,
    pub(super) depth: usize,
    pub(super) line: String,
    pub(super) search_text: String,
    pub(super) preview: Vec<String>,
    pub(super) no_tag: bool,
    pub(super) action: ModeTreeAction,
}

#[derive(Debug, Clone)]
pub(super) enum ModeTreeAction {
    None,
    TreeTarget {
        session_name: SessionName,
        window_index: Option<u32>,
        pane_index: Option<u32>,
        pane_id: Option<u32>,
    },
    Buffer {
        name: String,
    },
    Client {
        pid: u32,
        control: bool,
    },
    CustomizeOption {
        scope: OptionScopeSelector,
        name: String,
    },
    CustomizeKey {
        table_name: String,
        key: KeyCode,
        key_string: String,
    },
}

impl ModeTreeKind {
    pub(super) fn command_name(self) -> &'static str {
        match self {
            Self::Tree => "choose-tree",
            Self::Buffer => "choose-buffer",
            Self::Client => "choose-client",
            Self::Customize => "customize-mode",
        }
    }

    pub(super) fn pane_mode_name(self) -> &'static str {
        match self {
            Self::Tree => "tree-mode",
            Self::Buffer => "buffer-mode",
            Self::Client => "client-mode",
            Self::Customize => "options-mode",
        }
    }
}

impl ModeTreeAction {
    pub(super) fn target_string(&self) -> Option<String> {
        match self {
            Self::None => None,
            Self::TreeTarget {
                session_name,
                window_index,
                pane_index,
                pane_id,
            } => Some(match (window_index, pane_index) {
                (None, _) => format!("={session_name}:"),
                (Some(window_index), None) => format!("={session_name}:{window_index}."),
                (Some(window_index), Some(_)) => {
                    let pane_id = pane_id.unwrap_or_default();
                    format!("={session_name}:{window_index}.%{pane_id}")
                }
            }),
            Self::Buffer { name } => Some(name.clone()),
            Self::Client { pid, .. } => Some(pid.to_string()),
            Self::CustomizeOption { name, .. } => Some(name.clone()),
            Self::CustomizeKey {
                table_name,
                key_string,
                ..
            } => Some(format!("{table_name}:{key_string}")),
        }
    }

    pub(super) fn current_target(&self) -> Option<Target> {
        match self {
            Self::TreeTarget {
                session_name,
                window_index,
                pane_index,
                ..
            } => match (window_index, pane_index) {
                (None, _) => Some(Target::Session(session_name.clone())),
                (Some(window_index), None) => Some(Target::Window(WindowTarget::with_window(
                    session_name.clone(),
                    *window_index,
                ))),
                (Some(window_index), Some(pane_index)) => Some(Target::Pane(
                    PaneTarget::with_window(session_name.clone(), *window_index, *pane_index),
                )),
            },
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ClientSnapshot {
    pub(super) pid: u32,
    pub(super) session_name: Option<SessionName>,
    pub(super) label: String,
    pub(super) order: u64,
    pub(super) activity: i64,
    pub(super) width: u16,
    pub(super) height: u16,
}

#[derive(Debug, Clone)]
pub(super) enum ModeTreePromptCallback {
    Filter,
    Search(SearchDirection),
    Command,
    CustomizeSetOption {
        scope: OptionScopeSelector,
        name: String,
    },
    CustomizeSetKey {
        table_name: String,
        key: KeyCode,
    },
}

#[derive(Debug, Clone)]
pub(super) enum ModeTreeDeferredAction {
    DeleteBuffers,
    DetachClients,
    KillCurrentTreeSelection,
    KillTaggedTreeSelections,
}
