use rmux_core::command_parser::ControlClientSize;
use rmux_proto::{RmuxError, TerminalSize};

use super::super::super::{
    attach_support::{
        prepare_applied_window_resize_events, resize_control_session_for_client,
        ControlResizeClient,
    },
    control_support::ControlClientIdentity,
    RequestHandler,
};
use crate::handler_support::attached_client_required;

impl RequestHandler {
    pub(in crate::handler) async fn apply_control_size(
        &self,
        identity: ControlClientIdentity,
        declaration: ControlClientSize,
    ) -> Result<(), RmuxError> {
        self.apply_control_size_and_flags(identity, Some(declaration), [])
            .await
    }

    pub(in crate::handler) async fn apply_control_size_and_flags(
        &self,
        identity: ControlClientIdentity,
        declaration: Option<ControlClientSize>,
        flags: impl IntoIterator<Item = &str>,
    ) -> Result<(), RmuxError> {
        // Keep the client and its attachment fixed across the native-readiness wait.
        let attachment = {
            let controls = self.active_control.lock().await;
            let active = controls
                .by_pid
                .get(&identity.requester_pid())
                .filter(|active| {
                    active.id == identity.control_id()
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .ok_or_else(|| attached_client_required("refresh-client"))?;
            (active.session_name.clone(), active.session_id)
        };
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let prepared = {
            let mut state = self.state.lock().await;
            let attaches = self.active_attach.lock().await;
            let mut controls = self.active_control.lock().await;
            let active = controls
                .by_pid
                .get_mut(&identity.requester_pid())
                .filter(|active| {
                    active.id == identity.control_id()
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                        && (active.session_name.clone(), active.session_id) == attachment
                })
                .ok_or_else(|| attached_client_required("refresh-client"))?;
            match &attachment {
                (Some(name), Some(id))
                    if state
                        .sessions
                        .session(name)
                        .is_some_and(|session| session.id() == *id) => {}
                (None, None) => {}
                _ => return Err(attached_client_required("refresh-client")),
            }
            let live = state
                .sessions
                .iter()
                .flat_map(|session| session.windows().map(|(_, window)| window.id()))
                .collect::<std::collections::HashSet<_>>();
            // Never reserve a future ID or let a numeric slot stand in for an owner.
            if let Some(ControlClientSize::Window(id, _)) = declaration {
                if !live.contains(&id) {
                    return Err(RmuxError::invalid_target(
                        format!("@{}", id.as_u32()),
                        "window not found",
                    ));
                }
            }
            active.window_sizes.retain_live(&live);
            let previous = (
                active.client_width,
                active.client_height,
                active.size_declared,
                active.window_sizes.clone(),
            );
            let previous_flags = active.flags;
            for value in flags {
                active.flags.apply_flag_string(value);
            }
            let eligibility_changed = active.flags.ignore_size != previous_flags.ignore_size;
            match declaration {
                Some(ControlClientSize::Global(size)) => {
                    active.client_width = size.cols;
                    active.client_height = size.rows;
                    active.size_declared = true;
                }
                Some(ControlClientSize::Window(id, size)) => active.window_sizes.set(id, size),
                Some(ControlClientSize::RemoveWindow(id)) => {
                    if !active.window_sizes.remove(id) && !eligibility_changed {
                        return Ok(());
                    }
                }
                None if !eligibility_changed => return Ok(()),
                None => {}
            }
            let declared = ((active.size_declared || active.window_sizes.declared())
                && !active.flags.ignore_size)
                .then_some(TerminalSize {
                    cols: active.client_width,
                    rows: active.client_height,
                });
            let sequence = active.size_sequence;
            match attachment {
                (Some(ref name), Some(session_id)) => {
                    let result = resize_control_session_for_client(
                        &mut state,
                        ControlResizeClient::new(
                            &attaches,
                            &controls,
                            identity.requester_pid(),
                            declared,
                            sequence,
                        ),
                        name,
                        session_id,
                    );
                    if let Err(error) = result {
                        let active = controls
                            .by_pid
                            .get_mut(&identity.requester_pid())
                            .expect("client remains locked");
                        (
                            active.client_width,
                            active.client_height,
                            active.size_declared,
                            active.window_sizes,
                        ) = previous;
                        active.flags = previous_flags;
                        return Err(error);
                    }
                    // Explicit size declarations echo each window, even when
                    // unchanged. A flags-only transition publishes only the
                    // geometry it applied, under the same client/session locks.
                    let mut events = if declaration.is_some() {
                        super::prepare_control_size_layout_echo_events(&mut state, name, session_id)
                    } else {
                        prepare_applied_window_resize_events(&mut state)
                    };
                    super::super::super::control_support::freeze_control_layout_notifications(
                        &state,
                        &controls,
                        &mut events,
                    );
                    events
                }
                (None, None) => Vec::new(),
                _ => return Err(attached_client_required("refresh-client")),
            }
        };
        self.pause_before_window_lifecycle_emit().await;
        for event in prepared {
            self.emit_prepared(event).await;
        }
        Ok(())
    }
}
