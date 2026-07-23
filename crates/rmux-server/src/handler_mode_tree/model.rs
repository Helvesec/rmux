use std::collections::{BTreeMap, BTreeSet};

use rmux_core::{KeyCode, Session};
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{PaneId, PaneTarget, SessionId, SessionName, Target, WindowId, WindowTarget};

use super::super::scripting_support::rename_pane_target_session;
use super::super::RequesterOrigin;
use crate::pane_terminals::WindowLinkOccurrenceId;
use crate::pane_transcript::SharedPaneTranscript;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct ModeTreePaneIdentity {
    target: PaneTarget,
    session_id: SessionId,
    window_id: WindowId,
    window_occurrence_id: WindowLinkOccurrenceId,
    pane_id: PaneId,
    output_generation: u64,
}

impl ModeTreePaneIdentity {
    pub(super) fn capture(
        state: &mut crate::pane_terminals::HandlerState,
        target: &PaneTarget,
    ) -> Result<Self, rmux_proto::RmuxError> {
        state.ensure_live_window_link_occurrences();
        let session = state
            .sessions
            .session(target.session_name())
            .ok_or_else(|| crate::pane_terminals::session_not_found(target.session_name()))?;
        let window = session.window_at(target.window_index()).ok_or_else(|| {
            rmux_proto::RmuxError::Server("mode-tree host window disappeared".to_owned())
        })?;
        let pane = window.pane(target.pane_index()).ok_or_else(|| {
            rmux_proto::RmuxError::Server("mode-tree host pane disappeared".to_owned())
        })?;
        let window_id = window.id();
        let pane_id = pane.id();
        let window_occurrence_id = state
            .window_link_occurrence_id(target.session_name(), target.window_index())
            .ok_or_else(|| {
                rmux_proto::RmuxError::Server(
                    "mode-tree host window occurrence disappeared".to_owned(),
                )
            })?;
        Ok(Self {
            target: target.clone(),
            session_id: session.id(),
            window_id,
            window_occurrence_id,
            pane_id,
            output_generation: state.pane_output_generation_for_target(target, pane_id),
        })
    }

    pub(super) fn target(&self) -> &PaneTarget {
        &self.target
    }

    pub(super) const fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub(super) fn matches(&self, state: &crate::pane_terminals::HandlerState) -> bool {
        state
            .sessions
            .session(self.target.session_name())
            .filter(|session| session.id() == self.session_id)
            .and_then(|session| session.window_at(self.target.window_index()))
            .filter(|window| window.id() == self.window_id)
            .and_then(|window| window.pane(self.target.pane_index()))
            .is_some_and(|pane| pane.id() == self.pane_id)
            && state
                .window_link_occurrence_id(self.target.session_name(), self.target.window_index())
                == Some(self.window_occurrence_id)
            && state.pane_output_generation_for_target(&self.target, self.pane_id)
                == self.output_generation
    }

    pub(super) fn current_target(
        &self,
        state: &crate::pane_terminals::HandlerState,
    ) -> Option<PaneTarget> {
        let original_session_target = state
            .sessions
            .iter()
            .find(|(_, session)| session.id() == self.session_id)
            .and_then(|(session_name, session)| {
                self.exact_target_in_session(session_name, session)
            });
        original_session_target.or_else(|| {
            state
                .sessions
                .iter()
                .filter_map(|(session_name, session)| {
                    self.exact_target_in_session(session_name, session)
                })
                .min_by(|left, right| {
                    left.session_name()
                        .as_str()
                        .cmp(right.session_name().as_str())
                        .then_with(|| left.window_index().cmp(&right.window_index()))
                        .then_with(|| left.pane_index().cmp(&right.pane_index()))
                })
        })
    }

    fn exact_target_in_session(
        &self,
        session_name: &SessionName,
        session: &Session,
    ) -> Option<PaneTarget> {
        let preferred = session
            .window_at(self.target.window_index())
            .filter(|window| window.id() == self.window_id)
            .and_then(|window| {
                window
                    .panes()
                    .iter()
                    .find(|pane| pane.id() == self.pane_id)
                    .map(|pane| (self.target.window_index(), pane.index()))
            });
        let (window_index, pane_index) = preferred.or_else(|| {
            session
                .windows()
                .iter()
                .find_map(|(&window_index, window)| {
                    (window.id() == self.window_id).then(|| {
                        window
                            .panes()
                            .iter()
                            .find(|pane| pane.id() == self.pane_id)
                            .map(|pane| (window_index, pane.index()))
                    })?
                })
        })?;
        Some(PaneTarget::with_window(
            session_name.clone(),
            window_index,
            pane_index,
        ))
    }

    pub(super) fn output_generation_matches(
        &self,
        state: &crate::pane_terminals::HandlerState,
        target: &PaneTarget,
    ) -> bool {
        state.pane_output_generation_for_target(target, self.pane_id) == self.output_generation
    }

    fn rename_session(&mut self, old_name: &SessionName, new_name: &SessionName) {
        rename_pane_target_session(&mut self.target, old_name, new_name);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) struct ModeTreeActionIdentity {
    attach_pid: u32,
    attach_id: u64,
    state_id: u64,
}

impl ModeTreeActionIdentity {
    pub(in crate::handler) const fn new(attach_pid: u32, attach_id: u64, state_id: u64) -> Self {
        Self {
            attach_pid,
            attach_id,
            state_id,
        }
    }

    pub(in crate::handler) const fn attach_pid(self) -> u32 {
        self.attach_pid
    }

    pub(in crate::handler) const fn attach_id(self) -> u64 {
        self.attach_id
    }

    pub(in crate::handler) const fn state_id(self) -> u64 {
        self.state_id
    }

    pub(in crate::handler) fn matches_active(
        self,
        state: &crate::pane_terminals::HandlerState,
        active_attach: &crate::handler::attach_support::ActiveAttachState,
    ) -> bool {
        active_attach
            .by_pid
            .get(&self.attach_pid)
            .is_some_and(|active| {
                active.id == self.attach_id
                    && active.mode_tree_state_id == self.state_id
                    && active.mode_tree.as_ref().is_some_and(|mode| {
                        state
                            .sessions
                            .session(&mode.session_name)
                            .is_some_and(|session| session.id() == mode.session_id)
                            && mode
                                .host_identity
                                .as_ref()
                                .is_none_or(|identity| identity.matches(state))
                    })
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
    }
}

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
    pub(super) origin: RequesterOrigin,
    pub(super) kind: ModeTreeKind,
    pub(super) session_name: SessionName,
    pub(super) session_id: SessionId,
    pub(super) host_pane: Option<PaneTarget>,
    pub(super) host_identity: Option<ModeTreePaneIdentity>,
    pub(super) host_transcript: Option<SharedPaneTranscript>,
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
    pub(in crate::handler) zoom_restore: Option<ModeTreePaneIdentity>,
    pub(super) last_list_rows: usize,
}

impl ModeTreeClientState {
    pub(in crate::handler) fn rename_session(
        &mut self,
        old_name: &SessionName,
        new_name: &SessionName,
    ) {
        if &self.session_name == old_name {
            self.session_name = new_name.clone();
        }
        if let Some(target) = self.host_pane.as_mut() {
            rename_pane_target_session(target, old_name, new_name);
        }
        if let Some(identity) = self.host_identity.as_mut() {
            identity.rename_session(old_name, new_name);
        }
        if let Some(identity) = self.zoom_restore.as_mut() {
            identity.rename_session(old_name, new_name);
        }
    }
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
        session_id: SessionId,
        window_index: Option<u32>,
        window_id: Option<WindowId>,
        window_occurrence_id: Option<WindowLinkOccurrenceId>,
        pane_index: Option<u32>,
        pane_id: Option<PaneId>,
        pane_output_generation: Option<u64>,
    },
    Buffer {
        name: String,
        order: u64,
    },
    Client {
        pid: u32,
        attach_id: u64,
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

pub(super) struct ChooseTreeTarget {
    pub(super) session_name: SessionName,
    pub(super) session_id: SessionId,
    pub(super) window_index: Option<u32>,
    pub(super) window_id: Option<WindowId>,
    pub(super) window_occurrence_id: Option<WindowLinkOccurrenceId>,
    pub(super) pane_index: Option<u32>,
    pub(super) pane_id: Option<PaneId>,
    pub(super) pane_output_generation: Option<u64>,
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
    pub(super) fn session_tree_target(session_name: SessionName, session_id: SessionId) -> Self {
        Self::TreeTarget {
            session_name,
            session_id,
            window_index: None,
            window_id: None,
            window_occurrence_id: None,
            pane_index: None,
            pane_id: None,
            pane_output_generation: None,
        }
    }

    pub(super) fn window_tree_target(
        session_name: SessionName,
        session_id: SessionId,
        window_index: u32,
        window_id: WindowId,
        window_occurrence_id: WindowLinkOccurrenceId,
    ) -> Self {
        Self::TreeTarget {
            session_name,
            session_id,
            window_index: Some(window_index),
            window_id: Some(window_id),
            window_occurrence_id: Some(window_occurrence_id),
            pane_index: None,
            pane_id: None,
            pane_output_generation: None,
        }
    }

    pub(super) fn pane_tree_target(
        target: PaneTarget,
        session_id: SessionId,
        window_id: WindowId,
        window_occurrence_id: WindowLinkOccurrenceId,
        pane_id: PaneId,
        pane_output_generation: u64,
    ) -> Self {
        Self::TreeTarget {
            session_name: target.session_name().clone(),
            session_id,
            window_index: Some(target.window_index()),
            window_id: Some(window_id),
            window_occurrence_id: Some(window_occurrence_id),
            pane_index: Some(target.pane_index()),
            pane_id: Some(pane_id),
            pane_output_generation: Some(pane_output_generation),
        }
    }

    pub(super) fn target_string(&self) -> Option<String> {
        match self {
            Self::None => None,
            Self::TreeTarget {
                session_name,
                window_index,
                pane_index,
                pane_id,
                ..
            } => match (window_index, pane_index) {
                (None, _) => Some(format!("={session_name}:")),
                (Some(window_index), None) => Some(format!("={session_name}:{window_index}.")),
                (Some(window_index), Some(_)) => pane_id
                    .map(PaneId::as_u32)
                    .map(|pane_id| format!("={session_name}:{window_index}.%{pane_id}")),
            },
            Self::Buffer { name, .. } => Some(name.clone()),
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
    pub(super) attach_id: u64,
    pub(super) session_name: Option<SessionName>,
    pub(super) label: String,
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
    DeleteBuffers { targets: Vec<ModeTreeAction> },
    DetachClients { targets: Vec<ModeTreeAction> },
    KillCurrentTreeSelection { targets: Vec<ModeTreeAction> },
    KillTaggedTreeSelections { targets: Vec<ModeTreeAction> },
}
