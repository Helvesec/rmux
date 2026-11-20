use std::sync::atomic::Ordering;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::Arc;

use rmux_core::LifecycleEvent;
#[cfg(test)]
use tokio::sync::mpsc;
use tracing::warn;

#[cfg(any(feature = "web", test))]
use crate::client_names::attached_client_name;
use crate::handler::{current_client_activity_timestamp, RequestHandler};
use crate::mouse::ClientMouseState;
#[cfg(test)]
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::{AttachControl, AttachControlSender};
use crate::server_access::ServerAccessAdmission;
#[cfg(test)]
use crate::server_access::{
    current_owner_uid, pause_before_access_registration, AccessRegistrationKind,
};

use super::state::{
    ActiveAttach, ActiveAttachIdentity, AttachClientSizeProvenance, AttachRegistration,
    AttachSessionScope,
};
use crate::handler::focus_support::AttachFocusOrigin;

#[cfg(test)]
struct TestAttachRegistration {
    client_name: String,
    closing: Arc<AtomicBool>,
    terminal_context: OuterTerminalContext,
    flags: super::ClientFlags,
}

pub(crate) struct AttachRegistrationTarget {
    pub(crate) session_name: rmux_proto::SessionName,
    pub(crate) expected_session_id: Option<rmux_proto::SessionId>,
}

pub(super) struct AttachRegistrationClient {
    client_name: String,
    focus_origin: AttachFocusOrigin,
    session_scope: AttachSessionScope,
}

impl AttachRegistrationClient {
    pub(super) fn new(client_name: String, focus_origin: AttachFocusOrigin) -> Self {
        Self {
            client_name,
            focus_origin,
            session_scope: AttachSessionScope::Movable,
        }
    }

    #[cfg(all(any(unix, windows), feature = "web"))]
    fn web_session_share_view(client_name: String) -> Self {
        Self {
            client_name,
            focus_origin: AttachFocusOrigin::Virtual,
            session_scope: AttachSessionScope::SharedSessionOnly,
        }
    }
}

impl RequestHandler {
    pub(crate) async fn reserve_attach_id(&self) -> u64 {
        self.next_client_creation_sequence()
    }

    #[cfg(test)]
    pub(crate) async fn register_attach(
        &self,
        requester_pid: u32,
        session_name: rmux_proto::SessionName,
        control_tx: mpsc::UnboundedSender<AttachControl>,
    ) -> u64 {
        self.register_attach_with_terminal_context(
            requester_pid,
            session_name,
            control_tx,
            OuterTerminalContext::default(),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn register_attach_with_terminal_context(
        &self,
        requester_pid: u32,
        session_name: rmux_proto::SessionName,
        control_tx: mpsc::UnboundedSender<AttachControl>,
        terminal_context: OuterTerminalContext,
    ) -> u64 {
        self.register_attach_with_closing(
            requester_pid,
            session_name,
            control_tx,
            Arc::new(AtomicBool::new(false)),
            terminal_context,
            super::ClientFlags::default(),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn register_attach_with_closing(
        &self,
        requester_pid: u32,
        session_name: rmux_proto::SessionName,
        control_tx: mpsc::UnboundedSender<AttachControl>,
        closing: Arc<AtomicBool>,
        terminal_context: OuterTerminalContext,
        flags: super::ClientFlags,
    ) -> u64 {
        let client_name = attached_client_name(requester_pid);
        self.register_attach_with_closing_and_client_name(
            requester_pid,
            session_name,
            control_tx,
            TestAttachRegistration {
                client_name,
                closing,
                terminal_context,
                flags,
            },
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn register_attach_with_client_name(
        &self,
        requester_pid: u32,
        client_name: String,
        session_name: rmux_proto::SessionName,
        control_tx: mpsc::UnboundedSender<AttachControl>,
    ) -> u64 {
        self.register_attach_with_closing_and_client_name(
            requester_pid,
            session_name,
            control_tx,
            TestAttachRegistration {
                client_name,
                closing: Arc::new(AtomicBool::new(false)),
                terminal_context: OuterTerminalContext::default(),
                flags: super::ClientFlags::default(),
            },
        )
        .await
    }

    #[cfg(test)]
    async fn register_attach_with_closing_and_client_name(
        &self,
        requester_pid: u32,
        session_name: rmux_proto::SessionName,
        control_tx: mpsc::UnboundedSender<AttachControl>,
        registration: TestAttachRegistration,
    ) -> u64 {
        let TestAttachRegistration {
            client_name,
            closing,
            terminal_context,
            flags,
        } = registration;
        let attach_id = self
            .register_attach_identity(
                requester_pid,
                AttachRegistrationTarget {
                    session_name,
                    expected_session_id: None,
                },
                AttachRegistration {
                    control_tx,
                    control_backlog: Arc::new(AtomicUsize::new(0)),
                    closing,
                    persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                    terminal_context,
                    client_title: None,
                    flags,
                    render_stream: false,
                    uid: current_owner_uid(),
                    user: self.server_owner_identity(),
                    can_write: true,
                    client_size: None,
                },
                None,
                None,
                AttachRegistrationClient::new(client_name, AttachFocusOrigin::Native),
            )
            .await
            .map(ActiveAttachIdentity::attach_id)
            .expect("test attach registration session must remain current");
        attach_id
    }

    #[cfg(test)]
    pub(crate) async fn register_attach_with_access(
        &self,
        requester_pid: u32,
        session_name: rmux_proto::SessionName,
        expected_session_id: Option<rmux_proto::SessionId>,
        registration: AttachRegistration,
    ) -> Option<u64> {
        self.register_attach_identity_with_access(
            requester_pid,
            session_name,
            expected_session_id,
            registration,
        )
        .await
        .map(ActiveAttachIdentity::attach_id)
    }

    #[cfg(test)]
    pub(crate) async fn register_attach_identity_with_access(
        &self,
        requester_pid: u32,
        session_name: rmux_proto::SessionName,
        expected_session_id: Option<rmux_proto::SessionId>,
        registration: AttachRegistration,
    ) -> Option<ActiveAttachIdentity> {
        let client_name = attached_client_name(requester_pid);
        self.register_attach_identity(
            requester_pid,
            AttachRegistrationTarget {
                session_name,
                expected_session_id,
            },
            registration,
            None,
            None,
            AttachRegistrationClient::new(client_name, AttachFocusOrigin::Native),
        )
        .await
    }

    #[cfg(all(any(unix, windows), feature = "web"))]
    pub(crate) async fn register_virtual_attach_identity_with_access(
        &self,
        requester_pid: u32,
        session_name: rmux_proto::SessionName,
        expected_session_id: Option<rmux_proto::SessionId>,
        registration: AttachRegistration,
    ) -> Option<ActiveAttachIdentity> {
        let client_name = attached_client_name(requester_pid);
        self.register_attach_identity(
            requester_pid,
            AttachRegistrationTarget {
                session_name,
                expected_session_id,
            },
            registration,
            None,
            None,
            AttachRegistrationClient::web_session_share_view(client_name),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn register_attach_identity_with_server_access(
        &self,
        requester_pid: u32,
        session_name: rmux_proto::SessionName,
        expected_session_id: Option<rmux_proto::SessionId>,
        registration: AttachRegistration,
        admission: ServerAccessAdmission,
    ) -> Option<ActiveAttachIdentity> {
        let client_name = attached_client_name(requester_pid);
        self.register_attach_identity_with_server_access_and_client_name(
            requester_pid,
            client_name,
            session_name,
            expected_session_id,
            registration,
            admission,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn register_attach_identity_with_server_access_and_client_name(
        &self,
        requester_pid: u32,
        client_name: String,
        session_name: rmux_proto::SessionName,
        expected_session_id: Option<rmux_proto::SessionId>,
        registration: AttachRegistration,
        admission: ServerAccessAdmission,
    ) -> Option<ActiveAttachIdentity> {
        self.register_attach_identity(
            requester_pid,
            AttachRegistrationTarget {
                session_name,
                expected_session_id,
            },
            registration,
            None,
            Some(admission),
            AttachRegistrationClient::new(client_name, AttachFocusOrigin::Native),
        )
        .await
    }

    pub(crate) async fn register_reserved_attach_identity_with_server_access_and_client_name(
        &self,
        requester_pid: u32,
        client_name: String,
        target: AttachRegistrationTarget,
        attach_id: u64,
        registration: AttachRegistration,
        admission: ServerAccessAdmission,
    ) -> Option<ActiveAttachIdentity> {
        self.register_attach_identity(
            requester_pid,
            target,
            registration,
            Some(attach_id),
            Some(admission),
            AttachRegistrationClient::new(client_name, AttachFocusOrigin::Native),
        )
        .await
    }

    async fn register_attach_identity(
        &self,
        requester_pid: u32,
        target: AttachRegistrationTarget,
        registration: AttachRegistration,
        reserved_attach_id: Option<u64>,
        admission: Option<ServerAccessAdmission>,
        client: AttachRegistrationClient,
    ) -> Option<ActiveAttachIdentity> {
        let AttachRegistrationClient {
            client_name,
            focus_origin,
            session_scope,
        } = client;
        let AttachRegistrationTarget {
            session_name,
            expected_session_id,
        } = target;
        #[cfg(test)]
        if admission.is_some() {
            pause_before_access_registration(AccessRegistrationKind::Attach, requester_pid).await;
        }
        #[cfg(windows)]
        self.wait_for_windows_deferred_session_panes_ready(&session_name)
            .await;
        let mut replaced_key_table = None;
        let mut replaced_overlay = None;
        let attached_session_name = session_name.clone();
        let mut state = self.state.lock().await;
        let session = state.sessions.session(&attached_session_name)?;
        let session_id = session.id();
        if expected_session_id.is_some_and(|expected| expected != session_id) {
            return None;
        }
        let active_window_index = Some(session.active_window_index());
        // A client that declared no size still needs an outer terminal anchor,
        // because `client_size` is outer geometry everywhere it is read. The
        // session's own terminal size is that anchor; its window size is the
        // content geometry already reduced by the status rows, and storing it
        // here made every consumer subtract those rows a second time.
        let (client_size, client_size_provenance) = match registration.client_size {
            Some(client_size) => (client_size, AttachClientSizeProvenance::Declared),
            None => (
                session.terminal_size(),
                AttachClientSizeProvenance::InferredFromSession,
            ),
        };
        let (attach_id, focus_effects) = {
            let mut active_attach = self.active_attach.lock().await;
            // Keep the client-state lock across policy revalidation and publication.
            // ACL mutations commit to the policy store first and only then acquire
            // this lock to update or disconnect clients, so a later mutation must
            // observe this registration.
            let server_access = admission.as_ref().map(|_| {
                self.server_access
                    .lock()
                    .expect("server access mutex must not be poisoned")
            });
            let can_write = match (server_access.as_ref(), admission.as_ref()) {
                (Some(server_access), Some(admission)) => server_access
                    .revalidate_admission(admission, &registration.user)?
                    .can_write(),
                (None, None) => registration.can_write,
                _ => unreachable!("an admission and its policy guard are created together"),
            };
            let flags = if can_write {
                registration.flags
            } else {
                registration.flags.with_read_only()
            };
            let attach_id = if let Some(attach_id) = reserved_attach_id {
                if active_attach
                    .by_pid
                    .values()
                    .any(|active| active.id == attach_id)
                {
                    return None;
                }
                attach_id
            } else {
                self.next_client_creation_sequence()
            };
            let size_sequence = self.next_client_size_sequence();
            let activity_sequence = self.next_client_activity_sequence();
            let control_backlog = registration.control_backlog;
            let control_tx = AttachControlSender::new(
                registration.control_tx,
                Arc::clone(&control_backlog),
                super::ATTACH_CONTROL_BACKLOG_LIMIT,
                Arc::clone(&registration.closing),
            );
            if let Some(mut previous) = active_attach.by_pid.insert(
                requester_pid,
                ActiveAttach {
                    theme: crate::theme::ClientTheme::default(),
                    pan: Default::default(),
                    viewport: Default::default(),
                    id: attach_id,
                    client_name,
                    session_name,
                    session_id,
                    last_session: None,
                    last_session_id: None,
                    flags,
                    control_tx,
                    control_backlog,
                    _read_demand: state.pane_output_read_demand.acquire_unrestricted(),
                    render_stream: registration.render_stream,
                    render_refresh_pending: false,
                    uid: registration.uid,
                    user: registration.user,
                    can_write,
                    reported_focused: true,
                    focus_origin,
                    session_scope,
                    clipboard_queries_desynchronized: false,
                    suspended: false,
                    closing: registration.closing,
                    detach_event_owned: false,
                    emit_detached_on_finish: false,
                    terminal_context: registration.terminal_context,
                    client_title: registration.client_title.unwrap_or_default(),
                    client_size,
                    client_size_provenance,
                    client_pixels: None,
                    size_sequence,
                    last_activity_sequence: activity_sequence,
                    activity_at: current_client_activity_timestamp(),
                    persistent_overlay_epoch: registration.persistent_overlay_epoch,
                    render_generation: 0,
                    overlay_generation: 0,
                    overlay_state_id: 0,
                    display_panes_state_id: 0,
                    key_table_name: None,
                    key_table_set_at: None,
                    key_table_generation: 0,
                    repeat_deadline: None,
                    repeat_active: false,
                    last_key: None,
                    held_keyboard: Default::default(),
                    assumed_paste: Default::default(),
                    idle_lock_timer: None,
                    mouse: ClientMouseState {
                        slider_mpos: -1,
                        ..ClientMouseState::default()
                    },
                    prompt: None,
                    mode_tree_state_id: 0,
                    mode_tree: None,
                    mode_tree_frame: None,
                    overlay: None,
                    display_panes: None,
                    transient_message: None,
                    transient_terminal_prefix: Vec::new(),
                },
            ) {
                active_attach.forget_attached_client_windows(requester_pid);
                replaced_key_table = previous.key_table_name.clone();
                replaced_overlay = previous.overlay.take();
                let _ = previous.control_tx.send(AttachControl::Detach);
                previous.closing.store(true, Ordering::SeqCst);
            }
            if let Some(window_index) = active_window_index {
                active_attach.seed_active_client_for_window(
                    requester_pid,
                    &attached_session_name,
                    window_index,
                );
            }
            drop(server_access);
            let focus_effects = super::super::focus_support::prepare_server_focus_effects(
                &mut state,
                &mut active_attach,
            );
            (attach_id, focus_effects)
        };
        drop(state);
        self.bump_active_attach_epoch();
        if let Err(error) = focus_effects.execute(self).await {
            warn!("failed to publish attach focus transition: {error}");
        }
        super::terminate_overlay_job(replaced_overlay);

        if let Some(table_name) = replaced_key_table {
            let mut state = self.state.lock().await;
            state.key_bindings.unref_table(&table_name);
        }

        let identity = ActiveAttachIdentity::new(requester_pid, attach_id, session_id);
        self.pause_before_attach_registration_activity().await;

        // Publication released the state lock, so `attached_session_name` may
        // now address a different session: the captured one can have been
        // destroyed and its name reused, or it can still be right here under a
        // new name. Credit the attach only while this registration is still the
        // live one for the client and still owns the exact session lifetime it
        // published, which is the identity boundary
        // `record_attached_input_activity` already enforces for the input this
        // registration is about to start accepting — and, like that path, take
        // the store key off the attach rather than the captured name, because a
        // rename moves the key without ending the lifetime that was attached
        // to. Handler lock order is state before active_attach.
        let mut state = self.state.lock().await;
        let active_attach = self.active_attach.lock().await;
        let live_session_name = active_attach
            .by_pid
            .get(&requester_pid)
            .filter(|active| {
                identity.matches_active_lifetime(active, session_id)
                    && !active.closing.load(Ordering::SeqCst)
            })
            .map(|active| active.session_name.clone());
        if let Some(live_session_name) = live_session_name.as_ref() {
            if let Some(session) = state
                .sessions
                .session(live_session_name)
                .filter(|session| session.id() == session_id)
            {
                let name = session.name().clone();
                let _ = state.sessions.touch_attached(&name);
            }
        }
        drop(active_attach);
        drop(state);
        // The overlay belongs to the same session this just credited. With no
        // live attach left there is nothing to resolve a current name from, so
        // fall back to the captured one, which is what an unattached refresh
        // has always addressed.
        let overlay_session_name = live_session_name.unwrap_or(attached_session_name);
        self.reset_all_attached_lock_timers().await;
        self.refresh_theme_sources().await;
        self.refresh_clock_overlays_for_session(&overlay_session_name)
            .await;
        Some(identity)
    }

    pub(crate) async fn finish_attach(&self, requester_pid: u32, attach_id: u64) {
        let (
            removed_session,
            removed_key_table,
            removed_overlay,
            detached_client_name,
            focus_effects,
        ) = {
            let mut state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            if active_attach
                .by_pid
                .get(&requester_pid)
                .is_some_and(|active| active.id == attach_id)
            {
                active_attach
                    .remove_attached_client(requester_pid)
                    .map(|active| {
                        let emit_detached = active.needs_detached_event();
                        let focus_effects =
                            super::super::focus_support::prepare_server_focus_effects(
                                &mut state,
                                &mut active_attach,
                            );
                        (
                            Some((active.session_name, active.session_id)),
                            active.key_table_name,
                            active.overlay,
                            emit_detached.then_some(active.client_name),
                            Some(focus_effects),
                        )
                    })
                    .unwrap_or((None, None, None, None, None))
            } else {
                (None, None, None, None, None)
            }
        };
        if removed_session.is_some() {
            self.bump_active_attach_epoch();
            if let Some(focus_effects) = focus_effects {
                if let Err(error) = focus_effects.execute(self).await {
                    warn!("failed to publish detach focus transition: {error}");
                }
            }
            self.refresh_theme_sources().await;
        }
        super::terminate_overlay_job(removed_overlay);
        if let Some(table_name) = removed_key_table {
            let mut state = self.state.lock().await;
            state.key_bindings.unref_table(&table_name);
        }
        if let Some((session_name, session_id)) = removed_session {
            if let Some(client_name) = detached_client_name {
                self.emit(LifecycleEvent::ClientDetached {
                    session_name: session_name.clone(),
                    client_name: Some(client_name),
                })
                .await;
            }
            if let Ok(Some(target)) = self.reconcile_attached_session_size(&session_name).await {
                self.emit_applied_window_resize(target).await;
            }
            self.destroy_unattached_sessions(vec![(session_name, session_id)])
                .await;
            let _ = self.queue_shutdown_if_server_unattached().await;
        }
    }

    pub(crate) async fn current_live_attach_input(&self, identity: ActiveAttachIdentity) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&identity.attach_pid())
            .is_some_and(|active| {
                identity.matches_active(active) && !active.closing.load(Ordering::SeqCst)
            })
    }

    pub(crate) async fn active_attach_identity(
        &self,
        attach_pid: u32,
    ) -> Option<ActiveAttachIdentity> {
        self.active_attach
            .lock()
            .await
            .by_pid
            .get(&attach_pid)
            .map(|active| active.identity(attach_pid))
    }

    #[cfg(test)]
    pub(crate) async fn active_attach_identity_for_test(
        &self,
        attach_pid: u32,
    ) -> ActiveAttachIdentity {
        self.active_attach_identity(attach_pid)
            .await
            .expect("test attach must be registered")
    }

    /// What the server believes a registered client's outer terminal shows.
    ///
    /// Listener-level tests live outside `crate::handler`, so this is how they
    /// read the per-client title memory the connection loop seeded.
    #[cfg(test)]
    pub(crate) async fn remembered_client_title_for_test(&self, attach_pid: u32) -> Option<String> {
        self.active_attach
            .lock()
            .await
            .by_pid
            .get(&attach_pid)
            .expect("test attach must be registered")
            .client_title
            .title()
            .map(str::to_owned)
    }
}

#[cfg(test)]
#[path = "../../handler_attach/creation_order_tests.rs"]
mod creation_order_tests;
