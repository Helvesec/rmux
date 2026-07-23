use rmux_core::LifecycleEvent;
use rmux_os::identity::UserIdentity;
use rmux_proto::{CommandOutput, ErrorResponse, Response, RmuxError, ServerAccessResponse};
#[cfg(test)]
use std::sync::{Arc, Mutex};

use super::{ClientFlags, RequestHandler};
use crate::pane_io::AttachControl;
use crate::server_access::{
    resolve_user, validate_server_access_request, AccessMode, ServerAccessStore,
};

#[cfg(test)]
#[derive(Debug, Default)]
struct AccessRevocationPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
static ACCESS_REVOCATION_PAUSES: Mutex<Vec<(u32, Arc<AccessRevocationPause>)>> =
    Mutex::new(Vec::new());

#[cfg(test)]
fn install_access_revocation_pause(uid: u32) -> Arc<AccessRevocationPause> {
    let pause = Arc::new(AccessRevocationPause::default());
    ACCESS_REVOCATION_PAUSES
        .lock()
        .expect("access revocation pause lock")
        .push((uid, pause.clone()));
    pause
}

#[cfg(test)]
async fn pause_after_access_revocation(uid: u32) {
    let pause = {
        let mut pauses = ACCESS_REVOCATION_PAUSES
            .lock()
            .expect("access revocation pause lock");
        pauses
            .iter()
            .position(|(paused_uid, _)| *paused_uid == uid)
            .map(|position| pauses.swap_remove(position).1)
    };
    if let Some(pause) = pause {
        pause.reached.notify_one();
        pause.release.notified().await;
    }
}

impl RequestHandler {
    pub(in crate::handler) fn server_owner_identity(&self) -> UserIdentity {
        self.server_access
            .lock()
            .expect("server access mutex must not be poisoned")
            .owner_identity()
            .clone()
    }

    pub(in crate::handler) async fn handle_server_access(
        &self,
        request: rmux_proto::ServerAccessRequest,
    ) -> Response {
        if let Err(error) = validate_server_access_request(&request) {
            return Response::Error(ErrorResponse { error });
        }

        if request.list {
            let output = self
                .server_access
                .lock()
                .expect("server access mutex must not be poisoned")
                .render_list();
            return Response::ServerAccess(ServerAccessResponse { output });
        }

        let Some(user) = request.user.as_deref() else {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server("missing user argument".to_owned()),
            });
        };
        let resolved = match resolve_user(user) {
            Ok(resolved) => resolved,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };

        let owner_uid = self
            .server_access
            .lock()
            .expect("server access mutex must not be poisoned")
            .owner_uid();
        if resolved.uid == 0 || resolved.uid == owner_uid {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server(format!(
                    "{} owns the server, can't change access",
                    resolved.name
                )),
            });
        }

        // Keep ACL mutations ordered through their live-client effects. In
        // particular, a concurrent re-add cannot publish fresh clients while
        // an earlier deny is still disconnecting the revoked generation.
        let _mutation = self.server_access_mutation.lock().await;

        if request.deny {
            match self.revoke_server_access(resolved.uid).await {
                Ok(true) => {}
                Ok(false) => {
                    return Response::Error(ErrorResponse {
                        error: RmuxError::Server(format!("user {} not found", resolved.name)),
                    });
                }
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
            return Response::ServerAccess(ServerAccessResponse {
                output: CommandOutput::from_stdout(Vec::new()),
            });
        }

        let access_update = self.mutate_server_access_store(|server_access| {
            if request.add && server_access.contains_uid(resolved.uid) {
                return Err(RmuxError::Server(format!(
                    "user {} is already added",
                    resolved.name
                )));
            }
            let should_add = request.add
                || ((request.read_only || request.write)
                    && !server_access.contains_uid(resolved.uid));
            if should_add {
                server_access.set_mode(resolved.uid, AccessMode::ReadWrite)?;
            }
            if request.write {
                server_access.set_mode(resolved.uid, AccessMode::ReadWrite)?;
            }
            if request.read_only {
                server_access.set_mode(resolved.uid, AccessMode::ReadOnly)?;
            }
            Ok(())
        });
        if let Err(error) = access_update {
            return Response::Error(ErrorResponse { error });
        }

        if request.write {
            self.update_live_access_mode(resolved.uid, true).await;
        } else if request.read_only {
            self.update_live_access_mode(resolved.uid, false).await;
        }

        Response::ServerAccess(ServerAccessResponse {
            output: CommandOutput::from_stdout(Vec::new()),
        })
    }

    async fn revoke_server_access(&self, uid: u32) -> Result<bool, RmuxError> {
        let removed = self.mutate_server_access_store(|server_access| {
            if !server_access.contains_uid(uid) {
                return Ok(false);
            }
            server_access.remove_uid(uid)?;
            Ok(true)
        })?;
        if !removed {
            return Ok(false);
        }
        #[cfg(test)]
        pause_after_access_revocation(uid).await;
        self.disconnect_clients_by_uid(uid).await;
        Ok(true)
    }

    fn mutate_server_access_store<T>(
        &self,
        mutate: impl FnOnce(&mut ServerAccessStore) -> Result<T, RmuxError>,
    ) -> Result<T, RmuxError> {
        let mut server_access = self
            .server_access
            .lock()
            .expect("server access mutex must not be poisoned");
        let previous = server_access.clone();
        let output = match mutate(&mut server_access) {
            Ok(output) => output,
            Err(error) => {
                *server_access = previous;
                return Err(error);
            }
        };
        let desired_shared = server_access.has_delegated_users();
        if let Err(error) = self.transition_unix_transport(desired_shared) {
            *server_access = previous;
            return Err(error);
        }
        Ok(output)
    }

    pub(in crate::handler) async fn update_live_access_mode(&self, uid: u32, can_write: bool) {
        let mut sessions = Vec::new();
        {
            let mut active_attach = self.active_attach.lock().await;
            for active in active_attach.by_pid.values_mut() {
                if active.uid != uid {
                    continue;
                }
                active.can_write = can_write;
                if can_write {
                    active.flags.remove(ClientFlags::READONLY);
                    active.flags.remove(ClientFlags::IGNORESIZE);
                } else {
                    active.flags.insert(ClientFlags::READONLY);
                    active.flags.insert(ClientFlags::IGNORESIZE);
                }
                sessions.push(active.session_name.clone());
            }
        }
        {
            let mut active_control = self.active_control.lock().await;
            for active in active_control.by_pid.values_mut() {
                if active.uid == uid {
                    active.can_write = can_write;
                }
            }
        }

        sessions.sort_by_key(|session_name| session_name.to_string());
        sessions.dedup();
        for session_name in sessions {
            self.refresh_attached_session(&session_name).await;
        }
    }

    pub(in crate::handler) async fn disconnect_clients_by_uid(&self, uid: u32) {
        let (attached, controls) = {
            let active_attach = self.active_attach.lock().await;
            let active_control = self.active_control.lock().await;
            let attached = active_attach
                .by_pid
                .iter()
                .filter_map(|(pid, active)| (active.uid == uid).then_some((*pid, active.id)))
                .collect::<Vec<_>>();
            let controls = active_control
                .by_pid
                .iter()
                .filter_map(|(pid, active)| (active.uid == uid).then_some((*pid, active.id)))
                .collect::<Vec<_>>();
            (attached, controls)
        };
        for (attach_pid, attach_id) in attached {
            if let Ok(session_name) = self
                .send_attach_control_for_client_identity(
                    attach_pid,
                    attach_id,
                    AttachControl::Detach,
                    "server-access",
                )
                .await
            {
                self.emit(LifecycleEvent::ClientDetached {
                    session_name,
                    client_name: Some(attach_pid.to_string()),
                })
                .await;
            }
        }

        for (control_pid, control_id) in controls {
            if let Ok(outcome) = self
                .exit_control_client_for_identity(
                    control_pid,
                    control_id,
                    Some("access not allowed".to_owned()),
                )
                .await
            {
                if let Some(event) = outcome.lifecycle_event {
                    self.emit_prepared(event).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
    use std::sync::Arc;
    use std::time::Duration;

    use rmux_ipc::PeerIdentity;
    use rmux_os::identity::UserIdentity;
    use rmux_proto::{
        ControlMode, NewSessionRequest, Request, Response, SessionName, TerminalSize,
    };
    use tokio::sync::mpsc;

    use super::{install_access_revocation_pause, RequestHandler};
    use crate::control::{ControlModeUpgrade, ControlServerEvent};
    use crate::handler::attach_support::{
        install_attach_control_identity_pause, AttachRegistration, ClientFlags,
    };
    use crate::handler::control_support::ControlRegistrationError;
    use crate::handler::ControlRegistration;
    use crate::outer_terminal::OuterTerminalContext;
    use crate::pane_io::AttachControl;
    use crate::server_access::{
        install_access_registration_pause, AccessMode, AccessRegistrationKind,
        ServerAccessAdmission,
    };

    const RACE_TIMEOUT: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn attach_registration_revalidates_a_downgraded_admission() {
        let handler = Arc::new(RequestHandler::new());
        let uid = synthetic_uid(15_001);
        let requester_pid = 915_001;
        let session_name = create_session(&handler, "attach-downgrade").await;
        let admission = grant_write_admission(&handler, uid);
        let pause =
            install_access_registration_pause(AccessRegistrationKind::Attach, requester_pid);

        let registration_handler = Arc::clone(&handler);
        let registration = tokio::spawn(async move {
            registration_handler
                .register_attach_identity_with_server_access(
                    requester_pid,
                    session_name,
                    None,
                    attach_registration(uid),
                    admission,
                )
                .await
        });
        wait_for_pause(&pause.reached).await;
        handler
            .set_test_access_mode_for_uid(uid, AccessMode::ReadOnly)
            .expect("test access downgrades");
        pause.release.notify_one();

        let identity = tokio::time::timeout(RACE_TIMEOUT, registration)
            .await
            .expect("attach registration resumes")
            .expect("attach registration task joins")
            .expect("read-only access still permits attach registration");
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&requester_pid)
            .expect("attach registration is published");
        assert!(!active.can_write);
        assert!(active.flags.contains(ClientFlags::READONLY));
        assert!(active.flags.contains(ClientFlags::IGNORESIZE));
        assert_eq!(active.id, identity.attach_id());
    }

    #[tokio::test]
    async fn control_registration_revalidates_a_downgraded_admission() {
        let handler = Arc::new(RequestHandler::new());
        let uid = synthetic_uid(15_002);
        let requester_pid = 915_002;
        let admission = grant_write_admission(&handler, uid);
        let pause =
            install_access_registration_pause(AccessRegistrationKind::Control, requester_pid);

        let registration_handler = Arc::clone(&handler);
        let registration = tokio::spawn(async move {
            registration_handler
                .register_control_with_server_access(
                    requester_pid,
                    control_upgrade(),
                    control_registration(uid),
                    admission,
                )
                .await
        });
        wait_for_pause(&pause.reached).await;
        handler
            .set_test_access_mode_for_uid(uid, AccessMode::ReadOnly)
            .expect("test access downgrades");
        pause.release.notify_one();

        let control_id = tokio::time::timeout(RACE_TIMEOUT, registration)
            .await
            .expect("control registration resumes")
            .expect("control registration task joins")
            .expect("read-only access still permits control registration");
        let active_control = handler.active_control.lock().await;
        let active = active_control
            .by_pid
            .get(&requester_pid)
            .expect("control registration is published");
        assert_eq!(active.id, control_id);
        assert!(!active.can_write);
    }

    #[tokio::test]
    async fn deny_linearizes_before_attach_registration() {
        let handler = Arc::new(RequestHandler::new());
        let uid = synthetic_uid(15_003);
        let requester_pid = 915_003;
        let session_name = create_session(&handler, "attach-revoke").await;
        let admission = grant_write_admission(&handler, uid);
        let pause = install_access_revocation_pause(uid);

        let revoke_handler = Arc::clone(&handler);
        let revocation = tokio::spawn(async move {
            let _mutation = revoke_handler.server_access_mutation.lock().await;
            revoke_handler.revoke_server_access(uid).await
        });
        wait_for_pause(&pause.reached).await;
        assert!(
            handler.server_access_mutation.try_lock().is_err(),
            "deny retains the ACL mutation fence until disconnection stabilizes"
        );

        let registered = handler
            .register_attach_identity_with_server_access(
                requester_pid,
                session_name,
                None,
                attach_registration(uid),
                admission,
            )
            .await;
        assert!(registered.is_none());
        assert!(!handler
            .active_attach
            .lock()
            .await
            .by_pid
            .contains_key(&requester_pid));

        pause.release.notify_one();
        assert!(tokio::time::timeout(RACE_TIMEOUT, revocation)
            .await
            .expect("revocation resumes")
            .expect("revocation task joins")
            .expect("revocation succeeds"));
    }

    #[tokio::test]
    async fn deny_linearizes_before_control_registration() {
        let handler = Arc::new(RequestHandler::new());
        let uid = synthetic_uid(15_004);
        let requester_pid = 915_004;
        let admission = grant_write_admission(&handler, uid);
        let pause = install_access_revocation_pause(uid);

        let revoke_handler = Arc::clone(&handler);
        let revocation = tokio::spawn(async move {
            let _mutation = revoke_handler.server_access_mutation.lock().await;
            revoke_handler.revoke_server_access(uid).await
        });
        wait_for_pause(&pause.reached).await;

        let error = handler
            .register_control_with_server_access(
                requester_pid,
                control_upgrade(),
                control_registration(uid),
                admission,
            )
            .await
            .expect_err("revoked access cannot publish a control client");
        assert_eq!(error, ControlRegistrationError::AccessRevoked);
        assert!(!handler
            .active_control
            .lock()
            .await
            .by_pid
            .contains_key(&requester_pid));

        pause.release.notify_one();
        assert!(tokio::time::timeout(RACE_TIMEOUT, revocation)
            .await
            .expect("revocation resumes")
            .expect("revocation task joins")
            .expect("revocation succeeds"));
    }

    #[tokio::test]
    async fn deny_preserves_authorized_same_pid_replacements() {
        let handler = Arc::new(RequestHandler::new());
        let revoked_uid = synthetic_uid(15_005);
        let replacement_uid = synthetic_uid(15_006);
        let attach_pid = 915_005;
        let control_pid = 915_006;
        let session_name = create_session(&handler, "revoke-same-pid-replacement").await;
        let revoked_admission = grant_write_admission(&handler, revoked_uid);
        let replacement_admission = grant_write_admission(&handler, replacement_uid);

        let (old_attach_tx, _old_attach_rx) = mpsc::unbounded_channel();
        let old_attach = handler
            .register_attach_identity_with_server_access(
                attach_pid,
                session_name.clone(),
                None,
                attach_registration_with_sender(revoked_uid, old_attach_tx),
                revoked_admission.clone(),
            )
            .await
            .expect("revoked attach registration is published");
        let (old_control_tx, mut old_control_rx) = mpsc::channel(8);
        let old_control_id = handler
            .register_control_with_server_access(
                control_pid,
                control_upgrade(),
                control_registration_with_sender(revoked_uid, old_control_tx),
                revoked_admission.clone(),
            )
            .await
            .expect("revoked control registration is published");
        handler
            .set_control_session(control_pid, Some(session_name.clone()))
            .await
            .expect("revoked control attaches to the session");
        while old_control_rx.try_recv().is_ok() {}

        let pause = install_attach_control_identity_pause(attach_pid);
        let revoke_handler = Arc::clone(&handler);
        let revocation = tokio::spawn(async move {
            let _mutation = revoke_handler.server_access_mutation.lock().await;
            revoke_handler.revoke_server_access(revoked_uid).await
        });
        wait_for_pause(&pause.reached).await;

        let (replacement_attach_tx, mut replacement_attach_rx) = mpsc::unbounded_channel();
        let replacement_attach = handler
            .register_attach_identity_with_server_access(
                attach_pid,
                session_name.clone(),
                None,
                attach_registration_with_sender(replacement_uid, replacement_attach_tx),
                replacement_admission.clone(),
            )
            .await
            .expect("authorized replacement attach is published");
        assert_ne!(replacement_attach.attach_id(), old_attach.attach_id());
        let (replacement_control_tx, mut replacement_control_rx) = mpsc::channel(8);
        let replacement_control_id = handler
            .register_control_with_server_access(
                control_pid,
                control_upgrade(),
                control_registration_with_sender(replacement_uid, replacement_control_tx),
                replacement_admission.clone(),
            )
            .await
            .expect("authorized replacement control is published");
        assert_ne!(replacement_control_id, old_control_id);
        handler
            .set_control_session(control_pid, Some(session_name))
            .await
            .expect("replacement control attaches to the session");
        while replacement_control_rx.try_recv().is_ok() {}
        pause.release.notify_one();

        assert!(tokio::time::timeout(RACE_TIMEOUT, revocation)
            .await
            .expect("revocation resumes")
            .expect("revocation task joins")
            .expect("revocation succeeds"));

        {
            let server_access = handler
                .server_access
                .lock()
                .expect("server access mutex must not be poisoned");
            assert_eq!(
                server_access
                    .revalidate_admission(&revoked_admission, &UserIdentity::Uid(revoked_uid)),
                None
            );
            assert_eq!(
                server_access.revalidate_admission(
                    &replacement_admission,
                    &UserIdentity::Uid(replacement_uid)
                ),
                Some(AccessMode::ReadWrite)
            );
        }
        {
            let active_attach = handler.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .expect("authorized replacement attach survives");
            assert_eq!(active.id, replacement_attach.attach_id());
            assert_eq!(active.uid, replacement_uid);
            assert!(active.can_write);
            assert!(!active.closing.load(std::sync::atomic::Ordering::SeqCst));
        }
        {
            let active_control = handler.active_control.lock().await;
            let active = active_control
                .by_pid
                .get(&control_pid)
                .expect("authorized replacement control survives");
            assert_eq!(active.id, replacement_control_id);
            assert_eq!(active.uid, replacement_uid);
            assert!(active.can_write);
            assert!(!active.closing.load(std::sync::atomic::Ordering::SeqCst));
        }
        assert!(std::iter::from_fn(|| replacement_attach_rx.try_recv().ok())
            .all(|event| !matches!(event, AttachControl::Detach)));
        assert!(
            std::iter::from_fn(|| replacement_control_rx.try_recv().ok())
                .all(|event| !matches!(event, ControlServerEvent::Exit(_)))
        );
    }

    fn synthetic_uid(offset: u32) -> u32 {
        crate::server_access::current_owner_uid()
            .wrapping_add(offset)
            .max(1)
    }

    fn grant_write_admission(handler: &RequestHandler, uid: u32) -> ServerAccessAdmission {
        handler
            .set_test_access_mode_for_uid(uid, AccessMode::ReadWrite)
            .expect("test access is granted");
        handler
            .server_access_admission_for_peer(&PeerIdentity {
                pid: 0,
                uid,
                user: UserIdentity::Uid(uid),
            })
            .expect("granted test peer has an admission")
    }

    async fn create_session(handler: &RequestHandler, name: &str) -> SessionName {
        let session_name = SessionName::new(name).expect("valid test session name");
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(response, Response::NewSession(_)), "{response:?}");
        session_name
    }

    fn attach_registration(uid: u32) -> AttachRegistration {
        let (control_tx, _control_rx) = mpsc::unbounded_channel::<AttachControl>();
        attach_registration_with_sender(uid, control_tx)
    }

    fn attach_registration_with_sender(
        uid: u32,
        control_tx: mpsc::UnboundedSender<AttachControl>,
    ) -> AttachRegistration {
        AttachRegistration {
            control_tx,
            control_backlog: Arc::new(AtomicUsize::new(0)),
            closing: Arc::new(AtomicBool::new(false)),
            persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
            terminal_context: OuterTerminalContext::default(),
            flags: ClientFlags::default(),
            render_stream: false,
            uid,
            user: UserIdentity::Uid(uid),
            can_write: true,
            client_size: None,
        }
    }

    fn control_registration(uid: u32) -> ControlRegistration {
        let (event_tx, _event_rx) = mpsc::channel::<ControlServerEvent>(1);
        control_registration_with_sender(uid, event_tx)
    }

    fn control_registration_with_sender(
        uid: u32,
        event_tx: mpsc::Sender<ControlServerEvent>,
    ) -> ControlRegistration {
        ControlRegistration {
            event_tx,
            closing: Arc::new(AtomicBool::new(false)),
            uid,
            user: UserIdentity::Uid(uid),
            can_write: true,
        }
    }

    fn control_upgrade() -> ControlModeUpgrade {
        ControlModeUpgrade {
            initial_command_count: 0,
            mode: ControlMode::Plain,
            terminal_context: OuterTerminalContext::default(),
        }
    }

    async fn wait_for_pause(reached: &tokio::sync::Notify) {
        tokio::time::timeout(RACE_TIMEOUT, reached.notified())
            .await
            .expect("race barrier is reached");
    }
}
