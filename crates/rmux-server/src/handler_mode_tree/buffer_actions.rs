use rmux_core::LifecycleEvent;
use rmux_proto::{PasteBufferRequest, Response, RmuxError};

#[cfg(test)]
use std::sync::{Arc, Mutex};

use super::super::buffer_support::OrderedPasteBufferResult;
use super::super::RequestHandler;
use super::mode_tree_model::{ModeTreeAction, ModeTreeActionIdentity};
use super::mode_tree_selection::selected_items;
use crate::handler_support::attached_client_required;

#[cfg(test)]
#[derive(Debug, Default)]
pub(in crate::handler) struct ModeTreeBufferPastePause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

#[cfg(test)]
type ModeTreeBufferPauseSlots = Vec<(u32, Arc<ModeTreeBufferPastePause>)>;

#[cfg(test)]
static MODE_TREE_BUFFER_PASTE_PAUSES: Mutex<ModeTreeBufferPauseSlots> = Mutex::new(Vec::new());

#[cfg(test)]
static MODE_TREE_BUFFER_DELETE_PAUSES: Mutex<ModeTreeBufferPauseSlots> = Mutex::new(Vec::new());

#[cfg(test)]
fn install_mode_tree_buffer_pause(
    pauses: &Mutex<ModeTreeBufferPauseSlots>,
    attach_pid: u32,
) -> Arc<ModeTreeBufferPastePause> {
    let pause = Arc::new(ModeTreeBufferPastePause::default());
    let mut installed = pauses.lock().expect("mode-tree buffer pause lock");
    installed.retain(|(installed_pid, _)| *installed_pid != attach_pid);
    installed.push((attach_pid, Arc::clone(&pause)));
    pause
}

#[cfg(test)]
fn take_mode_tree_buffer_pause(
    pauses: &Mutex<ModeTreeBufferPauseSlots>,
    attach_pid: u32,
) -> Option<Arc<ModeTreeBufferPastePause>> {
    let mut installed = pauses.lock().expect("mode-tree buffer pause lock");
    let position = installed
        .iter()
        .position(|(installed_pid, _)| *installed_pid == attach_pid)?;
    Some(installed.swap_remove(position).1)
}

#[cfg(test)]
pub(in crate::handler) fn install_mode_tree_buffer_paste_pause(
    attach_pid: u32,
) -> Arc<ModeTreeBufferPastePause> {
    install_mode_tree_buffer_pause(&MODE_TREE_BUFFER_PASTE_PAUSES, attach_pid)
}

#[cfg(test)]
pub(in crate::handler) fn install_mode_tree_buffer_delete_pause(
    attach_pid: u32,
) -> Arc<ModeTreeBufferPastePause> {
    install_mode_tree_buffer_pause(&MODE_TREE_BUFFER_DELETE_PAUSES, attach_pid)
}

#[cfg(test)]
async fn pause_before_mode_tree_buffer_paste(identity: ModeTreeActionIdentity) {
    let pause = take_mode_tree_buffer_pause(&MODE_TREE_BUFFER_PASTE_PAUSES, identity.attach_pid());
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
}

#[cfg(test)]
async fn pause_before_mode_tree_buffer_delete(identity: ModeTreeActionIdentity) {
    let pause = take_mode_tree_buffer_pause(&MODE_TREE_BUFFER_DELETE_PAUSES, identity.attach_pid());
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
}

impl RequestHandler {
    #[cfg(test)]
    pub(super) async fn perform_buffer_paste(
        &self,
        attach_pid: u32,
        delete_after: bool,
    ) -> Result<(), RmuxError> {
        let identity = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| active.mode_tree.is_some())
                .ok_or_else(|| attached_client_required("choose-buffer"))?;
            ModeTreeActionIdentity::new(attach_pid, active.id, active.mode_tree_state_id)
        };
        self.perform_buffer_paste_for_identity(identity, delete_after)
            .await
    }

    pub(super) async fn perform_buffer_paste_for_identity(
        &self,
        identity: ModeTreeActionIdentity,
        delete_after: bool,
    ) -> Result<(), RmuxError> {
        let attach_pid = identity.attach_pid();
        let mut mode = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    active.id == identity.attach_id()
                        && active.mode_tree_state_id == identity.state_id()
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .ok_or_else(|| attached_client_required("choose-buffer"))?;
            active
                .mode_tree
                .clone()
                .ok_or_else(|| RmuxError::Server("mode-tree is not active".to_owned()))?
        };
        let had_tagged_items_before_rebuild = !mode.tagged.is_empty();
        let selected_id_before_rebuild = mode.selected_id.clone();
        let build = self.build_mode_tree(&mut mode, attach_pid).await?;
        if had_tagged_items_before_rebuild && mode.tagged.is_empty() {
            return self
                .refresh_mode_tree_overlay_for_action_identity(identity)
                .await;
        }
        if mode.tagged.is_empty() {
            match selected_id_before_rebuild {
                Some(selected_id) if build.items.contains_key(&selected_id) => {
                    mode.selected_id = Some(selected_id);
                }
                Some(_) => {
                    return self
                        .refresh_mode_tree_overlay_for_action_identity(identity)
                        .await
                }
                None => {}
            }
        }
        let target = self.mode_tree_active_pane(&mode.session_name).await?;
        #[cfg(test)]
        pause_before_mode_tree_buffer_paste(identity).await;
        for item in selected_items(&mode, &build) {
            let ModeTreeAction::Buffer { name, order } = &item.action else {
                continue;
            };
            let response = self
                .handle_paste_buffer_for_order_and_requester(
                    PasteBufferRequest {
                        name: Some(name.clone()),
                        target: target.clone(),
                        delete_after,
                        separator: None,
                        linefeed: false,
                        raw: false,
                        bracketed: false,
                    },
                    *order,
                    identity,
                )
                .await;
            match response {
                OrderedPasteBufferResult::StaleIdentity => {
                    // Keep the tree open so the replacement instance can be
                    // reviewed explicitly; no bytes or deletion occurred.
                    return self
                        .refresh_mode_tree_overlay_for_action_identity(identity)
                        .await;
                }
                OrderedPasteBufferResult::StaleRequesterIdentity => {
                    return Err(attached_client_required("choose-buffer"));
                }
                OrderedPasteBufferResult::Completed(Response::Error(error)) => {
                    return Err(error.error);
                }
                OrderedPasteBufferResult::Completed(_) => {}
            }
        }
        self.dismiss_mode_tree_with_refresh_for_action_identity(identity)
            .await
    }

    #[cfg(test)]
    pub(super) async fn perform_buffer_delete(&self, attach_pid: u32) -> Result<(), RmuxError> {
        let identity = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    active.mode_tree.is_some()
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .ok_or_else(|| attached_client_required("choose-buffer"))?;
            ModeTreeActionIdentity::new(attach_pid, active.id, active.mode_tree_state_id)
        };
        self.perform_buffer_delete_for_identity(identity).await
    }

    pub(super) async fn perform_buffer_delete_for_identity(
        &self,
        identity: ModeTreeActionIdentity,
    ) -> Result<(), RmuxError> {
        let attach_pid = identity.attach_pid();
        let mut mode = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    active.id == identity.attach_id()
                        && active.mode_tree_state_id == identity.state_id()
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .ok_or_else(|| attached_client_required("choose-buffer"))?;
            active
                .mode_tree
                .clone()
                .ok_or_else(|| attached_client_required("choose-buffer"))?
        };
        let had_tagged_items_before_rebuild = !mode.tagged.is_empty();
        let selected_id_before_rebuild = mode.selected_id.clone();
        let build = self.build_mode_tree(&mut mode, attach_pid).await?;
        if had_tagged_items_before_rebuild && mode.tagged.is_empty() {
            return self
                .refresh_mode_tree_overlay_for_action_identity(identity)
                .await;
        }
        if mode.tagged.is_empty() {
            match selected_id_before_rebuild {
                Some(selected_id) if build.items.contains_key(&selected_id) => {
                    mode.selected_id = Some(selected_id);
                }
                Some(_) => {
                    return self
                        .refresh_mode_tree_overlay_for_action_identity(identity)
                        .await
                }
                None => {}
            }
        }
        let actions = selected_items(&mode, &build)
            .into_iter()
            .map(|item| item.action.clone())
            .collect();
        self.perform_buffer_delete_actions_for_identity(identity, actions)
            .await
    }

    pub(super) async fn perform_buffer_delete_actions(
        &self,
        attach_pid: u32,
        actions: Vec<ModeTreeAction>,
    ) -> Result<(), RmuxError> {
        self.perform_buffer_delete_actions_inner(attach_pid, None, actions)
            .await
    }

    async fn perform_buffer_delete_actions_for_identity(
        &self,
        identity: ModeTreeActionIdentity,
        actions: Vec<ModeTreeAction>,
    ) -> Result<(), RmuxError> {
        self.perform_buffer_delete_actions_inner(identity.attach_pid(), Some(identity), actions)
            .await
    }

    async fn perform_buffer_delete_actions_inner(
        &self,
        attach_pid: u32,
        expected_requester: Option<ModeTreeActionIdentity>,
        actions: Vec<ModeTreeAction>,
    ) -> Result<(), RmuxError> {
        #[cfg(test)]
        if let Some(identity) = expected_requester {
            pause_before_mode_tree_buffer_delete(identity).await;
        }

        let deleted = {
            let mut state = self.state.lock().await;
            // Match attach registration's state -> active-attach lock order.
            // Holding both makes requester validation and the whole selected
            // buffer deletion batch one logical commit: a same-PID reconnect
            // cannot inherit any of the old key event's actions.
            let _active_attach = match expected_requester {
                Some(expected) => {
                    let active_attach = self.active_attach.lock().await;
                    let requester_is_current = active_attach
                        .by_pid
                        .get(&expected.attach_pid())
                        .is_some_and(|active| {
                            active.id == expected.attach_id()
                                && active.mode_tree_state_id == expected.state_id()
                                && active.mode_tree.is_some()
                                && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                        });
                    if !requester_is_current {
                        return Err(attached_client_required("choose-buffer"));
                    }
                    Some(active_attach)
                }
                None => None,
            };
            actions
                .into_iter()
                .filter_map(|action| {
                    let ModeTreeAction::Buffer { name, order } = action else {
                        return None;
                    };
                    state
                        .buffers
                        .delete_if_order_matches(&name, order)
                        .then_some(name)
                })
                .collect::<Vec<_>>()
        };
        for buffer_name in deleted {
            self.emit(LifecycleEvent::PasteBufferDeleted { buffer_name })
                .await;
        }
        match expected_requester {
            Some(identity) => {
                self.refresh_mode_tree_overlay_for_action_identity(identity)
                    .await
            }
            None => self.refresh_mode_tree_overlay_if_active(attach_pid).await,
        }
    }
}
