use std::cmp::Reverse;
use std::collections::BTreeSet;

use rmux_proto::{
    KillSessionRequest, KillWindowRequest, PaneKillRequest, PaneTargetRef, Request, Response,
    RmuxError, SessionId, SessionName, WindowId, WindowTarget,
};

use super::super::{
    dispatch_with_expected_session_identity, dispatch_with_expected_window_occurrence_identity,
    ExpectedWindowOccurrenceIdentity, RequestHandler,
};
use super::mode_tree_model::ModeTreeAction;
use crate::pane_terminals::WindowLinkOccurrenceId;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TreeKillSortKey {
    rank: u8,
    session_name: String,
    session_id: u32,
    window_index: Reverse<u32>,
    window_id: u32,
    window_occurrence_id: u64,
    pane_id: u32,
}

impl RequestHandler {
    pub(super) async fn perform_tree_kill_current(&self, attach_pid: u32) -> Result<(), RmuxError> {
        let mut mode = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            active
                .mode_tree
                .clone()
                .ok_or_else(|| RmuxError::Server("mode-tree is not active".to_owned()))?
        };
        let selected_id = mode.selected_id.clone();
        let build = self.build_mode_tree(&mut mode, attach_pid).await?;
        let Some(item) = selected_id
            .as_ref()
            .and_then(|selected_id| build.items.get(selected_id))
        else {
            return Ok(());
        };
        self.perform_tree_kill_actions(attach_pid, vec![item.action.clone()])
            .await
    }

    pub(super) async fn perform_tree_kill_tagged(&self, attach_pid: u32) -> Result<(), RmuxError> {
        let mut mode = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            active
                .mode_tree
                .clone()
                .ok_or_else(|| RmuxError::Server("mode-tree is not active".to_owned()))?
        };
        let build = self.build_mode_tree(&mut mode, attach_pid).await?;
        let actions = build
            .items
            .values()
            .filter(|item| mode.tagged.contains(&item.id))
            .map(|item| item.action.clone())
            .collect::<Vec<_>>();
        self.perform_tree_kill_actions(attach_pid, actions).await
    }

    pub(super) async fn perform_tree_kill_actions(
        &self,
        attach_pid: u32,
        mut actions: Vec<ModeTreeAction>,
    ) -> Result<(), RmuxError> {
        actions.sort_by_key(tree_kill_sort_key);
        let mut stable_targets = BTreeSet::new();
        actions.retain(|action| {
            tree_kill_stable_identity(action).is_none_or(|identity| stable_targets.insert(identity))
        });
        for action in actions {
            let response = match action {
                ModeTreeAction::TreeTarget {
                    session_name,
                    session_id,
                    window_index: None,
                    ..
                } => {
                    let request = Request::KillSession(KillSessionRequest {
                        target: session_name.clone(),
                        kill_all_except_target: false,
                        clear_alerts: false,
                        kill_group: false,
                    });
                    self.dispatch_mode_tree_session_request(
                        attach_pid,
                        session_name,
                        session_id,
                        request,
                    )
                    .await
                }
                ModeTreeAction::TreeTarget {
                    session_name,
                    session_id,
                    window_index: Some(window_index),
                    window_id: Some(window_id),
                    window_occurrence_id: Some(window_occurrence_id),
                    pane_index: None,
                    ..
                } => {
                    let request = Request::KillWindow(KillWindowRequest {
                        target: WindowTarget::with_window(session_name.clone(), window_index),
                        kill_all_others: false,
                    });
                    self.dispatch_mode_tree_window_request(
                        attach_pid,
                        ExpectedWindowOccurrenceIdentity::new(
                            session_name,
                            session_id,
                            window_index,
                            window_id,
                            window_occurrence_id,
                        ),
                        request,
                    )
                    .await
                }
                ModeTreeAction::TreeTarget {
                    session_name,
                    session_id,
                    window_index: Some(window_index),
                    window_id: Some(window_id),
                    window_occurrence_id: Some(window_occurrence_id),
                    pane_index: Some(_),
                    pane_id: Some(pane_id),
                } => {
                    let request = Request::PaneKill(PaneKillRequest {
                        target: PaneTargetRef::by_id(session_name.clone(), pane_id),
                        kill_all_except: false,
                    });
                    self.dispatch_mode_tree_window_request(
                        attach_pid,
                        ExpectedWindowOccurrenceIdentity::new(
                            session_name,
                            session_id,
                            window_index,
                            window_id,
                            window_occurrence_id,
                        ),
                        request,
                    )
                    .await
                }
                ModeTreeAction::TreeTarget { .. } => {
                    return Err(RmuxError::Server(
                        "mode-tree target lost its stable identity".to_owned(),
                    ));
                }
                ModeTreeAction::None
                | ModeTreeAction::Buffer { .. }
                | ModeTreeAction::Client { .. }
                | ModeTreeAction::CustomizeOption { .. }
                | ModeTreeAction::CustomizeKey { .. } => continue,
            };
            if let Response::Error(error) = response {
                return Err(error.error);
            }
        }
        self.refresh_hook_identity_aliases().await;
        if self.mode_tree_active(attach_pid).await {
            self.refresh_mode_tree_overlay_if_active(attach_pid).await?;
        }
        Ok(())
    }

    async fn dispatch_mode_tree_session_request(
        &self,
        attach_pid: u32,
        session_name: SessionName,
        session_id: SessionId,
        request: Request,
    ) -> Response {
        dispatch_with_expected_session_identity(self, attach_pid, session_name, session_id, request)
            .await
    }

    async fn dispatch_mode_tree_window_request(
        &self,
        attach_pid: u32,
        identity: ExpectedWindowOccurrenceIdentity,
        request: Request,
    ) -> Response {
        dispatch_with_expected_window_occurrence_identity(self, attach_pid, identity, request).await
    }
}

fn tree_kill_stable_identity(action: &ModeTreeAction) -> Option<(u8, u32)> {
    match action {
        ModeTreeAction::TreeTarget {
            pane_id: Some(pane_id),
            ..
        } => Some((0, pane_id.as_u32())),
        ModeTreeAction::TreeTarget {
            window_id: Some(window_id),
            ..
        } => Some((1, window_id.as_u32())),
        ModeTreeAction::TreeTarget { session_id, .. } => Some((2, session_id.as_u32())),
        ModeTreeAction::None
        | ModeTreeAction::Buffer { .. }
        | ModeTreeAction::Client { .. }
        | ModeTreeAction::CustomizeOption { .. }
        | ModeTreeAction::CustomizeKey { .. } => None,
    }
}

fn tree_kill_sort_key(action: &ModeTreeAction) -> TreeKillSortKey {
    match action {
        ModeTreeAction::TreeTarget {
            session_name,
            session_id,
            window_index: Some(window_index),
            window_id,
            window_occurrence_id,
            pane_index: Some(_),
            pane_id,
        } => TreeKillSortKey {
            rank: 0,
            session_name: session_name.to_string(),
            session_id: session_id.as_u32(),
            window_index: Reverse(*window_index),
            window_id: window_id.map_or(u32::MAX, WindowId::as_u32),
            window_occurrence_id: window_occurrence_id
                .map_or(u64::MAX, WindowLinkOccurrenceId::as_u64),
            pane_id: pane_id.map_or(u32::MAX, rmux_proto::PaneId::as_u32),
        },
        ModeTreeAction::TreeTarget {
            session_name,
            session_id,
            window_index: Some(window_index),
            window_id,
            window_occurrence_id,
            pane_index: None,
            ..
        } => TreeKillSortKey {
            rank: 1,
            session_name: session_name.to_string(),
            session_id: session_id.as_u32(),
            window_index: Reverse(*window_index),
            window_id: window_id.map_or(u32::MAX, WindowId::as_u32),
            window_occurrence_id: window_occurrence_id
                .map_or(u64::MAX, WindowLinkOccurrenceId::as_u64),
            pane_id: u32::MAX,
        },
        ModeTreeAction::TreeTarget {
            session_name,
            session_id,
            window_index: None,
            ..
        } => TreeKillSortKey {
            rank: 2,
            session_name: session_name.to_string(),
            session_id: session_id.as_u32(),
            window_index: Reverse(0),
            window_id: u32::MAX,
            window_occurrence_id: u64::MAX,
            pane_id: u32::MAX,
        },
        _ => TreeKillSortKey {
            rank: 3,
            session_name: String::new(),
            session_id: u32::MAX,
            window_index: Reverse(0),
            window_id: u32::MAX,
            window_occurrence_id: u64::MAX,
            pane_id: u32::MAX,
        },
    }
}
