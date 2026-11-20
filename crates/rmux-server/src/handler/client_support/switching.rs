#[cfg(test)]
use crate::session_fixture::SessionSetupExt as _;
use std::cell::RefCell;
use std::future::Future;
use std::sync::atomic::Ordering;
use std::time::Instant;

use rmux_core::{TargetFindContext, TargetFindFlags, TargetFindType, UnresolvedTarget};
use rmux_proto::request::{SwitchClientExt2Request, SwitchClientExt3Request};
#[cfg(test)]
use rmux_proto::OptionName;
use rmux_proto::{
    ErrorResponse, Response, RmuxError, SessionId, SessionName, SwitchClientResponse, Target,
    TerminalGeometry,
};
#[cfg(test)]
use rmux_proto::{PaneId, PaneTarget, WindowId, WindowTarget};

use crate::control::ControlFlagChange;
use crate::handler_support::{ambiguous_attached_client, attached_client_required};
#[cfg(test)]
use crate::pane_io::AttachControl;
use crate::pane_terminals::{session_not_found, HandlerState};

use super::super::{
    active_session_target,
    attach_support::{
        ActiveAttachIdentity, AttachedSwitchCommitRequest, AttachedSwitchCommittedTarget,
    },
    attached_client_matches_target, client_environment_snapshot, control_client_target_pid,
    control_support::{current_control_queue_identity, ManagedClient},
    normalize_target_client, parse_session_sort_order, switch_client_target_find_type,
    switch_target_selector_count, with_visible_pane_bases, RequestHandler, SessionSortOrder,
};
use super::SwitchTargetSelection;

#[cfg(test)]
#[path = "../../handler_client/attach_target_identity_tests.rs"]
mod attach_target_identity_tests;

mod client_identity;
mod switch_commit;
mod target_capture;
mod target_resolution;

pub(in crate::handler) use target_capture::capture_switch_client_target_identity;
use target_capture::{record_switch_client_committed_target, record_switch_client_target_identity};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SwitchSessionIdentity {
    pub(super) session_name: SessionName,
    pub(super) session_id: SessionId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ResolvedSwitchTarget {
    pub(super) session: SwitchSessionIdentity,
    pub(super) selection: Option<SwitchTargetSelection>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::handler) enum SwitchManagedClientIdentity {
    Attach { pid: u32, attach_id: u64 },
    Control { pid: u32, control_id: u64 },
}

impl SwitchManagedClientIdentity {
    const fn client(self) -> ManagedClient {
        match self {
            Self::Attach { pid, attach_id } => ManagedClient::Attach { pid, attach_id },
            Self::Control { pid, control_id } => ManagedClient::Control(
                super::super::control_support::ControlClientIdentity::new(pid, control_id),
            ),
        }
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
struct SwitchTargetIdentityPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
static SWITCH_TARGET_IDENTITY_PAUSE: std::sync::Mutex<
    Vec<(SessionName, std::sync::Arc<SwitchTargetIdentityPause>)>,
> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn install_switch_target_identity_pause(
    session_name: SessionName,
) -> std::sync::Arc<SwitchTargetIdentityPause> {
    let pause = std::sync::Arc::new(SwitchTargetIdentityPause::default());
    let mut pauses = SWITCH_TARGET_IDENTITY_PAUSE
        .lock()
        .expect("switch target identity pause lock");
    pauses.retain(|(paused_session, _)| paused_session != &session_name);
    pauses.push((session_name, pause.clone()));
    pause
}

#[cfg(test)]
pub(super) async fn pause_after_switch_target_identity_capture(session_name: &SessionName) {
    let pause = SWITCH_TARGET_IDENTITY_PAUSE
        .lock()
        .expect("switch target identity pause lock")
        .iter()
        .filter(|(paused_session, _)| paused_session == session_name)
        .map(|(_, pause)| pause.clone())
        .next();
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
    let mut installed = SWITCH_TARGET_IDENTITY_PAUSE
        .lock()
        .expect("switch target identity pause lock");
    installed.retain(|(paused_session, current)| {
        paused_session != session_name || !std::sync::Arc::ptr_eq(current, &pause)
    });
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_switch_client(
        &self,
        requester_pid: u32,
        request: rmux_proto::SwitchClientRequest,
    ) -> Response {
        self.handle_switch_client_ext3(
            requester_pid,
            SwitchClientExt3Request {
                target_client: None,
                target: Some(request.target.to_string()),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                sort_order: None,
                skip_environment_update: false,
                zoom: false,
            },
        )
        .await
    }

    pub(in crate::handler) async fn handle_switch_client_ext(
        &self,
        requester_pid: u32,
        request: rmux_proto::SwitchClientExtRequest,
    ) -> Response {
        self.handle_switch_client_ext3(
            requester_pid,
            SwitchClientExt3Request {
                target_client: None,
                target: request.target.map(|target| target.to_string()),
                key_table: request.key_table,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                sort_order: None,
                skip_environment_update: false,
                zoom: false,
            },
        )
        .await
    }

    pub(in crate::handler) async fn handle_switch_client_ext2(
        &self,
        requester_pid: u32,
        request: SwitchClientExt2Request,
    ) -> Response {
        self.handle_switch_client_ext3(
            requester_pid,
            SwitchClientExt3Request {
                target_client: None,
                target: request.target.map(|target| target.to_string()),
                key_table: request.key_table,
                last_session: request.last_session,
                next_session: request.next_session,
                previous_session: request.previous_session,
                toggle_read_only: request.toggle_read_only,
                sort_order: request.sort_order,
                skip_environment_update: request.skip_environment_update,
                zoom: false,
            },
        )
        .await
    }

    pub(in crate::handler) async fn handle_switch_client_ext3(
        &self,
        requester_pid: u32,
        request: SwitchClientExt3Request,
    ) -> Response {
        let client = match self
            .resolve_switch_managed_client_identity(requester_pid, request.target_client.as_deref())
            .await
        {
            Ok(client) => client,
            Err(error)
                if request.target_client.is_none()
                    && matches!(
                        &error,
                        RmuxError::Server(message)
                            if message == "switch-client requires an attached client"
                    ) =>
            {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Message("no current client".to_owned()),
                });
            }
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        self.handle_switch_client_ext3_for_managed_client(requester_pid, client, request)
            .await
    }

    pub(in crate::handler) async fn handle_switch_client_ext3_for_attach_identity(
        &self,
        identity: ActiveAttachIdentity,
        request: SwitchClientExt3Request,
    ) -> Response {
        if request.target_client.is_some() {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server(
                    "identity-bound switch-client does not accept -c".to_owned(),
                ),
            });
        }
        let client = SwitchManagedClientIdentity::Attach {
            pid: identity.attach_pid(),
            attach_id: identity.attach_id(),
        };
        if let Err(error) = self.validate_switch_managed_client_identity(client).await {
            return Response::Error(ErrorResponse { error });
        }
        self.handle_switch_client_ext3_for_managed_client(identity.attach_pid(), client, request)
            .await
    }

    async fn handle_switch_client_ext3_for_managed_client(
        &self,
        requester_pid: u32,
        client: SwitchManagedClientIdentity,
        request: SwitchClientExt3Request,
    ) -> Response {
        record_switch_client_target_identity(client);
        if switch_target_selector_count(&request) > 1 {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server(
                    "switch-client accepts only one of -t, -l, -n, or -p".to_owned(),
                ),
            });
        }
        if switch_target_selector_count(&request) == 0
            && request.key_table.is_none()
            && !request.toggle_read_only
        {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server(
                    "switch-client requires -t target, -T key-table, -l, -n, -p, or -r".to_owned(),
                ),
            });
        }
        if matches!(client, SwitchManagedClientIdentity::Control { .. })
            && request.key_table.is_some()
        {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server(
                    "switch-client -T is not available for control clients".to_owned(),
                ),
            });
        }
        // tmux 3.7b `switch-client -r` toggles a control client's read-only and
        // ignore-size flags, then `server_client_set_session()` commits its
        // session again: the window geometry is recalculated without (or with)
        // this client and it receives `%session-changed`.
        let control_flag_change = if request.toggle_read_only
            && matches!(client, SwitchManagedClientIdentity::Control { .. })
        {
            ControlFlagChange::ToggleReadOnly
        } else {
            ControlFlagChange::Keep
        };

        if let SwitchManagedClientIdentity::Attach {
            pid: attach_pid,
            attach_id,
        } = client
        {
            // Repeat state is cleared before target resolution. The transient key table is
            // resolved by the commit: a same-session switch keeps the live table, while a
            // session change falls back to the default and clears it after delivery.
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .filter(|active| active.id == attach_id)
            else {
                return Response::Error(ErrorResponse {
                    error: attached_client_required("switch-client"),
                });
            };
            active.repeat_active = false;
            active.repeat_deadline = None;
            active.last_key = None;
        }

        let current_session = match self.current_managed_session_identity(client).await {
            Ok(identity) => Some(identity),
            Err(error) if switch_target_selector_count(&request) == 0 => {
                return Response::Error(ErrorResponse { error });
            }
            Err(_) => None,
        };
        let mut session_identity = current_session.clone();

        let switch_target = if let Some(target) = request.target.as_deref() {
            match self
                .apply_switch_target_for_client_identity(
                    target,
                    current_session.as_ref(),
                    TargetFindFlags::NONE,
                    request.zoom,
                    client,
                )
                .await
            {
                Ok(session_name) => Some(session_name),
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        } else if request.last_session {
            match client {
                SwitchManagedClientIdentity::Attach {
                    pid: attach_pid,
                    attach_id,
                } => {
                    let active_attach = self.active_attach.lock().await;
                    let Some(active) = active_attach
                        .by_pid
                        .get(&attach_pid)
                        .filter(|active| active.id == attach_id)
                    else {
                        return Response::Error(ErrorResponse {
                            error: attached_client_required("switch-client"),
                        });
                    };
                    active.last_session.clone().zip(active.last_session_id).map(
                        |(session_name, session_id)| ResolvedSwitchTarget {
                            session: SwitchSessionIdentity {
                                session_name,
                                session_id,
                            },
                            selection: None,
                        },
                    )
                }
                SwitchManagedClientIdentity::Control {
                    pid: control_pid,
                    control_id,
                } => {
                    let state = self.state.lock().await;
                    let active_control = self.active_control.lock().await;
                    let Some(active) = active_control
                        .by_pid
                        .get(&control_pid)
                        .filter(|active| active.id == control_id)
                    else {
                        return Response::Error(ErrorResponse {
                            error: attached_client_required("switch-client"),
                        });
                    };
                    active
                        .last_session
                        .clone()
                        .zip(active.last_session_id)
                        .filter(|(session_name, session_id)| {
                            state
                                .sessions
                                .session(session_name)
                                .is_some_and(|session| session.id() == *session_id)
                        })
                        .map(|(session_name, session_id)| ResolvedSwitchTarget {
                            session: SwitchSessionIdentity {
                                session_name,
                                session_id,
                            },
                            selection: None,
                        })
                }
            }
        } else if request.next_session || request.previous_session {
            match self
                .adjacent_session_name(
                    current_session.as_ref(),
                    request.next_session,
                    request.sort_order.as_deref(),
                )
                .await
            {
                Ok(session_name) => session_name.map(|session| ResolvedSwitchTarget {
                    session,
                    selection: None,
                }),
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        } else {
            None
        };
        let switch_target = switch_target.or_else(|| {
            (control_flag_change != ControlFlagChange::Keep)
                .then(|| current_session.clone())
                .flatten()
                .map(|session| ResolvedSwitchTarget {
                    session,
                    selection: None,
                })
        });

        if let Some(target_session) = switch_target {
            #[cfg(test)]
            pause_after_switch_target_identity_capture(&target_session.session.session_name).await;
            let target_session_identity = target_session.session.clone();
            let response = self
                .switch_managed_client_to_session_with_control_flags(
                    requester_pid,
                    client,
                    target_session.session,
                    target_session.selection,
                    request.skip_environment_update,
                    control_flag_change,
                )
                .await;
            let Response::SwitchClient(_) = &response else {
                return response;
            };
            session_identity = Some(target_session_identity);
        }

        let Some(session_identity) = session_identity else {
            return Response::Error(ErrorResponse {
                error: attached_client_required("switch-client"),
            });
        };
        let session_name = session_identity.session_name;

        if let Some(key_table) = request.key_table {
            let SwitchManagedClientIdentity::Attach {
                pid: attach_pid,
                attach_id,
            } = client
            else {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(
                        "switch-client -T is not available for control clients".to_owned(),
                    ),
                });
            };
            if let Err(error) = self
                .apply_attached_key_table(
                    attach_pid,
                    attach_id,
                    &session_name,
                    session_identity.session_id,
                    key_table,
                )
                .await
            {
                return Response::Error(ErrorResponse { error });
            }
        }

        // A control client's toggle was committed with its session above.
        let attached_toggle = match client {
            SwitchManagedClientIdentity::Attach { pid, attach_id } if request.toggle_read_only => {
                Some((pid, attach_id))
            }
            _ => None,
        };
        if let Some((attach_pid, attach_id)) = attached_toggle {
            let mut active_attach = self.active_attach.lock().await;
            if let Err(error) = active_attach.toggle_read_only_for_identity(attach_pid, attach_id) {
                return Response::Error(ErrorResponse { error });
            }
        }

        Response::SwitchClient(SwitchClientResponse { session_name })
    }

    async fn apply_attached_key_table(
        &self,
        attach_pid: u32,
        attach_id: u64,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        key_table: String,
    ) -> Result<(), RmuxError> {
        #[cfg(test)]
        self.pause_before_attached_key_table_switch_apply(attach_pid)
            .await;
        let key_table_set_at = Instant::now();
        let commit = self
            .set_attached_key_table_for_client_session_identity_and_reset_repeat(
                super::super::attach_support::ActiveAttachIdentity::new(
                    attach_pid, attach_id, session_id,
                ),
                session_name,
                Some(key_table.clone()),
                Some(key_table_set_at),
            )
            .await?;

        if key_table == "prefix" && commit.prefix_timeout_ms != 0 {
            self.schedule_attached_prefix_timeout_for_identity(
                commit.identity,
                key_table_set_at,
                commit.key_table_generation,
                commit.prefix_timeout_ms,
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_flags::ClientFlags;
    use crate::control::{ControlModeUpgrade, ControlServerEvent, CONTROL_SERVER_EVENT_CAPACITY};
    use rmux_proto::{
        AttachSessionExtRequest, ControlMode, KillSessionRequest, KillWindowRequest,
        NewSessionRequest, NewWindowRequest, Request, Response, ScopeSelector, SetOptionMode,
        SetOptionRequest, TerminalSize,
    };
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
    }

    const fn content_size_for_default_status(terminal_size: TerminalSize) -> TerminalSize {
        TerminalSize {
            cols: terminal_size.cols,
            rows: terminal_size.rows.saturating_sub(1),
        }
    }

    fn drain_control_events(
        events: &mut mpsc::Receiver<ControlServerEvent>,
    ) -> Vec<ControlServerEvent> {
        let mut drained = Vec::new();
        while let Ok(event) = events.try_recv() {
            drained.push(event);
        }
        drained
    }

    struct SwitchEnvironmentChild(std::process::Child);

    impl Drop for SwitchEnvironmentChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    const SWITCH_ENVIRONMENT_HELPER: &str = "RMUX_TEST_SWITCH_ENVIRONMENT_HELPER";

    #[test]
    fn switch_environment_probe_helper() {
        if std::env::var_os(SWITCH_ENVIRONMENT_HELPER).is_some() {
            std::thread::sleep(std::time::Duration::from_secs(120));
        }
    }

    async fn spawn_switch_environment_child(display: &str) -> SwitchEnvironmentChild {
        let executable = std::env::current_exe().expect("current test executable");
        let mut command = std::process::Command::new(executable);
        command.args([
            "--exact",
            "handler::client_support::switching::tests::switch_environment_probe_helper",
            "--test-threads=1",
        ]);
        command.env(SWITCH_ENVIRONMENT_HELPER, "1");
        command.env("DISPLAY", display);
        command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = SwitchEnvironmentChild(
            command
                .spawn()
                .expect("spawn switch environment helper process"),
        );
        let pid = child.0.id();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if rmux_os::process::environment(pid)
                    .as_ref()
                    .and_then(|environment| environment.get("DISPLAY"))
                    .is_some_and(|value| value == display)
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("switch environment helper installs its environment");
        child
    }

    async fn create_session(handler: &RequestHandler, name: SessionName) {
        // Keep session, split and window readers free of interactive prompts.
        let response = handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::DefaultCommand,
                value: crate::test_shell::stdin_discard_command(),
                mode: SetOptionMode::Replace,
            }))
            .await;
        assert!(matches!(response, Response::SetOption(_)), "{response:?}");
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: name,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        crate::session_fixture::assert_new_session(&response);
        handler.wait_for_initial_pane_tasks_for_test().await;
    }

    async fn create_window_with_name(
        handler: &RequestHandler,
        session_name: &SessionName,
        window_name: &str,
    ) -> u32 {
        let mut state = handler.state.lock().await;
        let mut session = state
            .sessions
            .session_setup(session_name)
            .expect("session exists");
        let (window_index, _) = session
            .create_window(TerminalSize { cols: 80, rows: 24 })
            .expect("window create succeeds");
        session
            .rename_window(window_index, window_name.to_owned())
            .expect("window rename succeeds");
        window_index
    }

    async fn create_detached_runtime_window(
        handler: &RequestHandler,
        session_name: &SessionName,
        window_name: &str,
        target_window_index: Option<u32>,
    ) -> u32 {
        let response = handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: session_name.clone(),
                name: Some(window_name.to_owned()),
                detached: true,
                environment: None,
                command: None,
                start_directory: None,
                target_window_index,
                insert_at_target: false,
                process_command: None,
            })))
            .await;
        let Response::NewWindow(response) = response else {
            panic!("runtime window creation failed: {response:?}");
        };
        response.target.window_index()
    }

    async fn resize_test_window(
        handler: &RequestHandler,
        session_name: &SessionName,
        window_index: u32,
        size: TerminalSize,
    ) {
        let mut state = handler.state.lock().await;
        state
            .mutate_session_and_resize_window_terminal(session_name, window_index, |session| {
                if session.active_window_index() == window_index {
                    session.resize_active_window_geometry(size, size);
                    Ok(())
                } else {
                    session.resize_window(window_index, size)
                }
            })
            .expect("test window resize succeeds");
    }

    async fn set_test_window_size_policy(
        handler: &RequestHandler,
        session_name: &SessionName,
        window_index: u32,
        value: &str,
    ) {
        let response = handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(
                    session_name.clone(),
                    window_index,
                )),
                option: OptionName::WindowSize,
                value: value.to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await;
        assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    }

    async fn set_test_attached_client_size(
        handler: &RequestHandler,
        attach_pid: u32,
        size: TerminalSize,
    ) {
        let mut active_attach = handler.active_attach.lock().await;
        let size_sequence = handler.next_client_size_sequence();
        let active = active_attach
            .by_pid
            .get_mut(&attach_pid)
            .expect("test attached client exists");
        active.set_declared_client_size(size);
        active.size_sequence = size_sequence;
        drop(active_attach);
        handler.bump_active_attach_epoch();
    }

    async fn test_pane_terminal_size(
        handler: &RequestHandler,
        session_name: &SessionName,
        window_index: u32,
    ) -> TerminalSize {
        let master = {
            let mut state = handler.state.lock().await;
            state
                .clone_pane_master_if_alive(session_name, window_index, 0)
                .expect("test pane terminal is alive")
        };
        let size = master.size().expect("test pane terminal size is available");
        TerminalSize {
            cols: size.cols,
            rows: size.rows,
        }
    }

    #[tokio::test]
    async fn switch_selection_rejects_reused_pane_index() {
        let handler = RequestHandler::new();
        let beta = session_name("switch-pane-index-identity");
        create_session(&handler, beta.clone()).await;
        let mut state = handler.state.lock().await;
        let mut session = state.sessions.session_setup(&beta).expect("session exists");
        let pane_index = session.split_pane(0).expect("second pane splits");
        let window = session.window_at(0).expect("window exists");
        let window_id = window.id();
        let captured_pane_id = window.pane(pane_index).expect("pane exists").id();
        let selection = SwitchTargetSelection::Pane {
            target: PaneTarget::with_window(beta.clone(), 0, pane_index),
            window_id,
            pane_id: captured_pane_id,
            pane_output_generation: None,
            zoom: false,
        };

        let _ = session
            .kill_pane(pane_index)
            .expect("captured pane is removed");
        let replacement_index = session.split_pane(0).expect("replacement pane splits");
        assert_eq!(replacement_index, pane_index);
        let replacement_pane_id = session
            .window_at(0)
            .and_then(|window| window.pane(replacement_index))
            .expect("replacement pane exists")
            .id();
        assert_ne!(replacement_pane_id, captured_pane_id);

        let error = selection
            .apply_to_state(&mut state)
            .expect_err("stable pane identity must reject index reuse");
        assert!(
            matches!(error, RmuxError::InvalidTarget { .. }),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn switch_client_fails_closed_when_resolved_session_name_is_recreated() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-target-identity-alpha");
        let beta = session_name("switch-target-identity-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;

        let attach_pid = 91_341;
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        handler
            .register_attach(attach_pid, alpha.clone(), control_tx)
            .await;
        let original_attach_id = handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&attach_pid)
            .expect("attach exists")
            .id;
        let pause = install_switch_target_identity_pause(beta.clone());

        let switch_handler = handler.clone();
        let switch_beta = beta.clone();
        let switch = tokio::spawn(async move {
            switch_handler
                .handle_switch_client_ext3(
                    attach_pid,
                    SwitchClientExt3Request {
                        target_client: None,
                        target: Some(switch_beta.to_string()),
                        key_table: None,
                        last_session: false,
                        next_session: false,
                        previous_session: false,
                        toggle_read_only: false,
                        sort_order: None,
                        skip_environment_update: true,
                        zoom: false,
                    },
                )
                .await
        });

        pause.reached.notified().await;
        let killed = handler
            .handle(Request::KillSession(KillSessionRequest {
                target: beta.clone(),
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await;
        assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
        create_session(&handler, beta.clone()).await;
        pause.release.notify_one();

        assert_eq!(
            switch.await.expect("switch task joins"),
            Response::Error(ErrorResponse {
                error: RmuxError::SessionNotFound(beta.to_string()),
            })
        );
        assert!(matches!(
            control_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("original attach survives");
        assert_eq!(active.id, original_attach_id);
        assert_eq!(active.session_name, alpha);
    }

    #[tokio::test]
    async fn switch_client_fails_closed_when_window_index_is_reused_before_commit() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-window-identity-alpha");
        let beta = session_name("switch-window-identity-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;
        let target_index = create_window_with_name(&handler, &beta, "captured").await;
        let target_display = "switch-target-before";
        let client_display = "switch-client-after";
        {
            let mut state = handler.state.lock().await;
            state.environment.set(
                ScopeSelector::Session(beta.clone()),
                "DISPLAY".to_owned(),
                target_display.to_owned(),
            );
        }
        let captured_window_id = handler
            .state
            .lock()
            .await
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(target_index))
            .expect("captured target window exists")
            .id();

        let requester = spawn_switch_environment_child(client_display).await;
        let attach_pid = requester.0.id();
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        handler
            .register_attach(attach_pid, alpha.clone(), control_tx)
            .await;
        let client_size = TerminalSize {
            cols: 111,
            rows: 37,
        };
        handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get_mut(&attach_pid)
            .expect("attach exists")
            .set_declared_client_size(client_size);
        let pause = install_switch_target_identity_pause(beta.clone());

        let switch_handler = handler.clone();
        let switch_target = format!("{beta}:{target_index}");
        let switch = tokio::spawn(async move {
            switch_handler
                .handle_switch_client_ext3(
                    attach_pid,
                    SwitchClientExt3Request {
                        target_client: None,
                        target: Some(switch_target),
                        key_table: None,
                        last_session: false,
                        next_session: false,
                        previous_session: false,
                        toggle_read_only: false,
                        sort_order: None,
                        skip_environment_update: false,
                        zoom: false,
                    },
                )
                .await
        });

        pause.reached.notified().await;
        let replacement_index = {
            let mut state = handler.state.lock().await;
            let mut session = state
                .sessions
                .session_setup(&beta)
                .expect("target session survives");
            let _ = session
                .remove_window(target_index)
                .expect("captured window is removed");
            let (replacement_index, _) = session
                .create_window(TerminalSize { cols: 80, rows: 24 })
                .expect("replacement window is created");
            session
                .rename_window(replacement_index, "replacement".to_owned())
                .expect("replacement window is named");
            replacement_index
        };
        assert_eq!(replacement_index, target_index);
        let replacement_window_id = handler
            .state
            .lock()
            .await
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(target_index))
            .expect("replacement window exists")
            .id();
        assert_ne!(replacement_window_id, captured_window_id);
        pause.release.notify_one();

        let response = switch.await.expect("switch task joins");
        assert!(
            matches!(
                response,
                Response::Error(ErrorResponse {
                    error: RmuxError::InvalidTarget { .. },
                })
            ),
            "{response:?}"
        );
        assert!(matches!(
            control_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        {
            let state = handler.state.lock().await;
            assert_eq!(
                state.environment.session_value(&beta, "DISPLAY"),
                Some(target_display),
                "a stale window target must not update the target environment"
            );
            let alpha_session = state.sessions.session(&alpha).expect("alpha survives");
            assert_eq!(
                alpha_session.window().size(),
                TerminalSize { cols: 80, rows: 24 },
                "the original attached session geometry remains unchanged"
            );
            let beta_session = state.sessions.session(&beta).expect("beta survives");
            assert_eq!(
                beta_session.window().size(),
                TerminalSize { cols: 80, rows: 24 },
                "a stale target must not resize the target session"
            );
            assert_eq!(
                beta_session
                    .window_at(target_index)
                    .expect("replacement window survives")
                    .size(),
                TerminalSize { cols: 80, rows: 24 },
                "the replacement window geometry remains unchanged"
            );
        }
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("original attach survives");
        assert_eq!(active.session_name, alpha);
        assert_eq!(active.client_size, client_size);
    }

    #[tokio::test]
    async fn switch_control_rejects_reused_window_before_environment_update() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-control-target-alpha");
        let beta = session_name("switch-control-target-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;
        let target_index = create_window_with_name(&handler, &beta, "captured").await;
        let captured_window_id = {
            let mut state = handler.state.lock().await;
            state.environment.set(
                ScopeSelector::Session(beta.clone()),
                "DISPLAY".to_owned(),
                "switch-control-target-before".to_owned(),
            );
            state
                .sessions
                .session(&beta)
                .and_then(|session| session.window_at(target_index))
                .expect("captured control target exists")
                .id()
        };

        let requester = spawn_switch_environment_child("switch-control-client-after").await;
        let control_pid = requester.0.id();
        let (control_tx, mut control_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
        let control_id = handler
            .register_control_with_closing(
                control_pid,
                ControlModeUpgrade {
                    mode: ControlMode::Plain,
                    terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
                    initial_command_count: 0,
                },
                control_tx,
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        handler
            .set_control_session(control_pid, Some(alpha.clone()))
            .await
            .expect("initial control session is set");
        assert!(matches!(
            control_rx.try_recv(),
            Ok(ControlServerEvent::SessionChanged(Some(ref session_name)))
                | Ok(ControlServerEvent::SessionChangedAt {
                    ref session_name,
                    ..
                })
                if session_name == &alpha
        ));
        let pause = install_switch_target_identity_pause(beta.clone());

        let switch_handler = handler.clone();
        let switch_target = format!("{beta}:{target_index}");
        let switch = tokio::spawn(async move {
            switch_handler
                .handle_switch_client_ext3(
                    control_pid,
                    SwitchClientExt3Request {
                        target_client: None,
                        target: Some(switch_target),
                        key_table: None,
                        last_session: false,
                        next_session: false,
                        previous_session: false,
                        toggle_read_only: false,
                        sort_order: None,
                        skip_environment_update: false,
                        zoom: false,
                    },
                )
                .await
        });

        pause.reached.notified().await;
        let replacement_window_id = {
            let mut state = handler.state.lock().await;
            let mut session = state
                .sessions
                .session_setup(&beta)
                .expect("control target session survives");
            let _ = session
                .remove_window(target_index)
                .expect("captured control target is removed");
            let (replacement_index, _) = session
                .create_window(TerminalSize { cols: 80, rows: 24 })
                .expect("replacement control target is created");
            assert_eq!(replacement_index, target_index);
            session
                .window_at(replacement_index)
                .expect("replacement control target exists")
                .id()
        };
        assert_ne!(replacement_window_id, captured_window_id);
        pause.release.notify_one();

        let response = switch.await.expect("control switch task joins");
        assert!(
            matches!(
                response,
                Response::Error(ErrorResponse {
                    error: RmuxError::InvalidTarget { .. },
                })
            ),
            "{response:?}"
        );
        assert!(matches!(
            control_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let state = handler.state.lock().await;
        assert_eq!(
            state.environment.session_value(&beta, "DISPLAY"),
            Some("switch-control-target-before"),
            "a stale control target must not update the target environment"
        );
        drop(state);
        let active_control = handler.active_control.lock().await;
        let active = active_control
            .by_pid
            .get(&control_pid)
            .expect("original control client survives");
        assert_eq!(active.id, control_id);
        assert_eq!(active.session_name.as_ref(), Some(&alpha));
    }

    #[tokio::test]
    async fn switch_client_fails_closed_when_attach_pid_is_reregistered() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-attach-generation-alpha");
        let beta = session_name("switch-attach-generation-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;
        let beta_window = create_window_with_name(&handler, &beta, "target").await;

        let attach_pid = 91_343;
        let (old_tx, mut old_rx) = mpsc::unbounded_channel();
        let old_id = handler
            .register_attach(attach_pid, alpha.clone(), old_tx)
            .await;
        let pause = install_switch_target_identity_pause(beta.clone());

        let switch_handler = handler.clone();
        let switch_target = format!("{beta}:{beta_window}");
        let switch = tokio::spawn(async move {
            switch_handler
                .handle_switch_client_ext3(
                    attach_pid,
                    SwitchClientExt3Request {
                        target_client: None,
                        target: Some(switch_target),
                        key_table: Some("copy-mode".to_owned()),
                        last_session: false,
                        next_session: false,
                        previous_session: false,
                        toggle_read_only: true,
                        sort_order: None,
                        skip_environment_update: true,
                        zoom: false,
                    },
                )
                .await
        });

        pause.reached.notified().await;
        let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
        let replacement_id = handler
            .register_attach(attach_pid, alpha.clone(), replacement_tx)
            .await;
        assert_ne!(replacement_id, old_id);
        pause.release.notify_one();

        assert_eq!(
            switch.await.expect("switch task joins"),
            Response::Error(ErrorResponse {
                error: attached_client_required("switch-client"),
            })
        );
        assert!(matches!(
            replacement_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        while let Ok(control) = old_rx.try_recv() {
            assert!(!matches!(control, AttachControl::Switch(_)));
        }
        let active_attach = handler.active_attach.lock().await;
        let replacement = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("replacement attach survives");
        assert_eq!(replacement.id, replacement_id);
        assert_eq!(replacement.session_name, alpha);
        assert_eq!(replacement.key_table_name, None);
        assert!(!replacement.flags.contains(ClientFlags::READONLY));
        drop(active_attach);
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&beta)
                .expect("beta session survives")
                .active_window_index(),
            0,
            "a failed client identity commit must not change the target session selection"
        );
    }

    #[tokio::test]
    async fn switch_client_commits_window_selection_with_attach_identity() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-window-commit-alpha");
        let beta = session_name("switch-window-commit-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;
        let created_window = handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: beta.clone(),
                name: Some("target".to_owned()),
                detached: true,
                environment: None,
                command: None,
                start_directory: None,
                target_window_index: None,
                insert_at_target: false,
                process_command: None,
            })))
            .await;
        let Response::NewWindow(created_window) = created_window else {
            panic!("target window creation failed: {created_window:?}");
        };
        let beta_window = created_window.target.window_index();

        let attach_pid = 91_348;
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        handler.register_attach(attach_pid, alpha, control_tx).await;

        let response = handler
            .handle_switch_client_ext3(
                attach_pid,
                SwitchClientExt3Request {
                    target_client: None,
                    target: Some(format!("{beta}:{beta_window}")),
                    key_table: None,
                    last_session: false,
                    next_session: false,
                    previous_session: false,
                    toggle_read_only: false,
                    sort_order: None,
                    skip_environment_update: true,
                    zoom: false,
                },
            )
            .await;

        assert_eq!(
            response,
            Response::SwitchClient(SwitchClientResponse {
                session_name: beta.clone(),
            })
        );
        assert!(matches!(
            control_rx.try_recv(),
            Ok(AttachControl::Switch(target))
                if target
                    .with_target(|target| target.session_name == beta)
                    .unwrap_or(false)
        ));
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&beta)
                .expect("beta session survives")
                .active_window_index(),
            beta_window
        );
        drop(state);
        let active_attach = handler.active_attach.lock().await;
        assert_eq!(
            active_attach
                .by_pid
                .get(&attach_pid)
                .map(|active| active.session_name.clone()),
            Some(beta)
        );
    }

    #[tokio::test]
    async fn switch_client_resizes_selected_inactive_window_with_its_policy() {
        const OLD_ACTIVE_SIZE: TerminalSize = TerminalSize { cols: 70, rows: 20 };
        const TARGET_INITIAL_SIZE: TerminalSize = TerminalSize { cols: 90, rows: 30 };
        const TARGET_CLIENT_SIZE: TerminalSize = TerminalSize {
            cols: 100,
            rows: 32,
        };
        const SWITCHING_CLIENT_SIZE: TerminalSize = TerminalSize {
            cols: 120,
            rows: 40,
        };

        let handler = RequestHandler::new();
        let alpha = session_name("switch-target-size-alpha");
        let beta = session_name("switch-target-size-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;
        let beta_window =
            create_detached_runtime_window(&handler, &beta, "sized-target", Some(1)).await;
        handler.wait_for_initial_panes_for_test().await;
        resize_test_window(&handler, &beta, 0, OLD_ACTIVE_SIZE).await;
        resize_test_window(&handler, &beta, beta_window, TARGET_INITIAL_SIZE).await;
        set_test_window_size_policy(&handler, &beta, 0, "largest").await;
        set_test_window_size_policy(&handler, &beta, beta_window, "smallest").await;

        let target_client_pid = 91_349;
        let (target_client_tx, _target_client_rx) = mpsc::unbounded_channel();
        handler
            .register_attach(target_client_pid, beta.clone(), target_client_tx)
            .await;
        set_test_attached_client_size(&handler, target_client_pid, TARGET_CLIENT_SIZE).await;

        let switching_client_pid = 91_350;
        let (switching_client_tx, mut switching_client_rx) = mpsc::unbounded_channel();
        handler
            .register_attach(switching_client_pid, alpha, switching_client_tx)
            .await;
        set_test_attached_client_size(&handler, switching_client_pid, SWITCHING_CLIENT_SIZE).await;
        let old_active_pty_before = test_pane_terminal_size(&handler, &beta, 0).await;

        let response = handler
            .handle_switch_client_ext3(
                switching_client_pid,
                SwitchClientExt3Request {
                    target_client: None,
                    target: Some(format!("{beta}:{beta_window}")),
                    key_table: None,
                    last_session: false,
                    next_session: false,
                    previous_session: false,
                    toggle_read_only: false,
                    sort_order: None,
                    skip_environment_update: true,
                    zoom: false,
                },
            )
            .await;

        assert_eq!(
            response,
            Response::SwitchClient(SwitchClientResponse {
                session_name: beta.clone(),
            })
        );
        let switched_target = match switching_client_rx.try_recv() {
            Ok(AttachControl::Switch(target)) => target.into_target(),
            other => panic!("expected one switch target, got {other:?}"),
        };
        assert_eq!(
            switched_target.active_pane_geometry.cols(),
            TARGET_CLIENT_SIZE.cols,
            "the first switch frame must use the target window's reconciled width"
        );
        {
            let state = handler.state.lock().await;
            let session = state
                .sessions
                .session(&beta)
                .expect("target session survives");
            assert_eq!(session.active_window_index(), beta_window);
            assert_eq!(
                session.window_at(0).expect("old active survives").size(),
                OLD_ACTIVE_SIZE,
                "switching to an inactive target must not resize the old active window"
            );
            assert_eq!(
                session
                    .window_at(beta_window)
                    .expect("selected target survives")
                    .size(),
                content_size_for_default_status(TARGET_CLIENT_SIZE),
                "the target's smallest policy must choose the existing target client over the larger incoming client"
            );
            assert_eq!(
                session.terminal_size(),
                TARGET_CLIENT_SIZE,
                "the committed session terminal size must follow the newly active target"
            );
        }
        assert_eq!(
            test_pane_terminal_size(&handler, &beta, 0).await,
            old_active_pty_before,
            "the old active PTY must remain untouched"
        );
        let target_pty_size = test_pane_terminal_size(&handler, &beta, beta_window).await;
        assert_eq!(
            target_pty_size,
            content_size_for_default_status(TARGET_CLIENT_SIZE),
            "the selected target PTY follows its reconciled content geometry"
        );
    }

    #[tokio::test]
    async fn switch_client_targeted_resize_rejects_window_index_aba_after_size_selection() {
        const OLD_ACTIVE_SIZE: TerminalSize = TerminalSize { cols: 73, rows: 21 };
        const REPLACEMENT_SIZE: TerminalSize = TerminalSize { cols: 66, rows: 18 };
        const SWITCHING_CLIENT_SIZE: TerminalSize = TerminalSize {
            cols: 121,
            rows: 41,
        };

        let handler = RequestHandler::new();
        let alpha = session_name("switch-size-aba-alpha");
        let beta = session_name("switch-size-aba-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;
        let target_index =
            create_detached_runtime_window(&handler, &beta, "captured", Some(1)).await;
        handler.wait_for_initial_panes_for_test().await;
        resize_test_window(&handler, &beta, 0, OLD_ACTIVE_SIZE).await;

        let captured_window_id = handler
            .state
            .lock()
            .await
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(target_index))
            .expect("captured target exists")
            .id();
        let attach_pid = 91_351;
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        let attach_id = handler
            .register_attach(attach_pid, alpha.clone(), control_tx)
            .await;
        set_test_attached_client_size(&handler, attach_pid, SWITCHING_CLIENT_SIZE).await;
        let pause = handler.install_attached_size_selection_pause();

        let switch_handler = handler.clone();
        let switch_target = format!("{beta}:{target_index}");
        let switch = tokio::spawn(async move {
            switch_handler
                .handle_switch_client_ext3(
                    attach_pid,
                    SwitchClientExt3Request {
                        target_client: None,
                        target: Some(switch_target),
                        key_table: None,
                        last_session: false,
                        next_session: false,
                        previous_session: false,
                        toggle_read_only: false,
                        sort_order: None,
                        skip_environment_update: true,
                        zoom: false,
                    },
                )
                .await
        });

        pause.reached.notified().await;
        let killed = handler
            .handle(Request::KillWindow(KillWindowRequest {
                target: WindowTarget::with_window(beta.clone(), target_index),
                kill_all_others: false,
            }))
            .await;
        assert!(matches!(killed, Response::KillWindow(_)), "{killed:?}");
        let replacement_index =
            create_detached_runtime_window(&handler, &beta, "replacement", Some(target_index))
                .await;
        assert_eq!(replacement_index, target_index);
        handler.wait_for_initial_panes_for_test().await;
        resize_test_window(&handler, &beta, replacement_index, REPLACEMENT_SIZE).await;
        set_test_window_size_policy(&handler, &beta, replacement_index, "manual").await;
        let replacement_window_id = handler
            .state
            .lock()
            .await
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(replacement_index))
            .expect("replacement target exists")
            .id();
        assert_ne!(replacement_window_id, captured_window_id);
        let old_active_pty_before_release = test_pane_terminal_size(&handler, &beta, 0).await;
        let replacement_pty_before_release =
            test_pane_terminal_size(&handler, &beta, replacement_index).await;
        pause.release.notify_one();

        assert!(matches!(
            switch.await.expect("switch task joins"),
            Response::Error(ErrorResponse {
                error: RmuxError::InvalidTarget { .. },
            })
        ));
        assert!(matches!(
            control_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        {
            let state = handler.state.lock().await;
            let session = state
                .sessions
                .session(&beta)
                .expect("target session survives");
            assert_eq!(session.active_window_index(), 0);
            assert_eq!(session.window().size(), OLD_ACTIVE_SIZE);
            assert_eq!(
                session
                    .window_at(replacement_index)
                    .expect("replacement survives")
                    .size(),
                REPLACEMENT_SIZE,
                "a replacement at the captured index must not inherit the stale resize"
            );
            assert_eq!(session.terminal_size(), OLD_ACTIVE_SIZE);
        }
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("original attached client survives");
        assert_eq!(active.id, attach_id);
        assert_eq!(active.session_name, alpha);
        drop(active_attach);
        assert_eq!(
            test_pane_terminal_size(&handler, &beta, 0).await,
            old_active_pty_before_release
        );
        assert_eq!(
            test_pane_terminal_size(&handler, &beta, replacement_index).await,
            replacement_pty_before_release
        );
    }

    #[tokio::test]
    async fn attach_session_fails_closed_when_attach_pid_is_reregistered() {
        let handler = RequestHandler::new();
        let alpha = session_name("attach-generation-alpha");
        let beta = session_name("attach-generation-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;

        let attach_pid = 91_345;
        let (old_tx, _old_rx) = mpsc::unbounded_channel();
        let old_id = handler
            .register_attach(attach_pid, alpha.clone(), old_tx)
            .await;
        let pause = install_switch_target_identity_pause(beta.clone());

        let attach_handler = handler.clone();
        let attach_beta = beta.clone();
        let attach = tokio::spawn(async move {
            attach_handler
                .dispatch(
                    attach_pid,
                    Request::AttachSessionExt(AttachSessionExtRequest {
                        target: Some(attach_beta),
                        detach_other_clients: false,
                        kill_other_clients: false,
                        read_only: true,
                        skip_environment_update: true,
                        flags: None,
                    }),
                )
                .await
        });

        pause.reached.notified().await;
        let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
        let replacement_id = handler
            .register_attach(attach_pid, alpha.clone(), replacement_tx)
            .await;
        assert_ne!(replacement_id, old_id);
        pause.release.notify_one();

        assert_eq!(
            attach.await.expect("attach task joins").response,
            Response::Error(ErrorResponse {
                error: attached_client_required("attach-session"),
            })
        );
        assert!(matches!(
            replacement_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let active_attach = handler.active_attach.lock().await;
        let replacement = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("replacement attach survives");
        assert_eq!(replacement.id, replacement_id);
        assert_eq!(replacement.session_name, alpha);
        assert!(!replacement.flags.contains(ClientFlags::READONLY));
    }

    #[tokio::test]
    async fn switch_client_fails_closed_when_control_pid_is_reregistered() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-control-generation-alpha");
        let beta = session_name("switch-control-generation-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;
        let beta_window = create_window_with_name(&handler, &beta, "target").await;

        let control_pid = 91_344;
        let (old_tx, mut old_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
        let old_id = handler
            .register_control_with_closing(
                control_pid,
                ControlModeUpgrade {
                    mode: ControlMode::Plain,
                    terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
                    initial_command_count: 0,
                },
                old_tx,
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        handler
            .set_control_session(control_pid, Some(alpha.clone()))
            .await
            .expect("initial control session set succeeds");
        assert!(matches!(
            old_rx.try_recv(),
            Ok(ControlServerEvent::SessionChanged(Some(ref session_name)))
                | Ok(ControlServerEvent::SessionChangedAt {
                    ref session_name,
                    ..
                })
                if session_name == &alpha
        ));
        let pause = install_switch_target_identity_pause(beta.clone());

        let switch_handler = handler.clone();
        let switch_target = format!("{beta}:{beta_window}");
        let switch = tokio::spawn(async move {
            switch_handler
                .handle_switch_client_ext3(
                    control_pid,
                    SwitchClientExt3Request {
                        target_client: None,
                        target: Some(switch_target),
                        key_table: None,
                        last_session: false,
                        next_session: false,
                        previous_session: false,
                        toggle_read_only: false,
                        sort_order: None,
                        skip_environment_update: true,
                        zoom: false,
                    },
                )
                .await
        });

        pause.reached.notified().await;
        let (replacement_tx, mut replacement_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
        let replacement_id = handler
            .register_control_with_closing(
                control_pid,
                ControlModeUpgrade {
                    mode: ControlMode::Plain,
                    terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
                    initial_command_count: 0,
                },
                replacement_tx,
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        assert_ne!(replacement_id, old_id);
        handler
            .set_control_session(control_pid, Some(alpha.clone()))
            .await
            .expect("replacement control session set succeeds");
        let replacement_events = drain_control_events(&mut replacement_rx);
        assert!(
            replacement_events.iter().any(|event| matches!(
                event,
                ControlServerEvent::SessionChanged(Some(session_name))
                    | ControlServerEvent::SessionChangedAt { session_name, .. }
                    if session_name == &alpha
            )),
            "replacement control did not enter the original session: {replacement_events:?}"
        );
        pause.release.notify_one();

        assert_eq!(
            switch.await.expect("switch task joins"),
            Response::Error(ErrorResponse {
                error: attached_client_required("switch-client"),
            })
        );
        assert!(matches!(
            replacement_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let old_events = drain_control_events(&mut old_rx);
        assert!(
            old_events
                .iter()
                .any(|event| matches!(event, ControlServerEvent::Exit(_))),
            "replaced control did not exit: {old_events:?}"
        );
        let active_control = handler.active_control.lock().await;
        let replacement = active_control
            .by_pid
            .get(&control_pid)
            .expect("replacement control survives");
        assert_eq!(replacement.id, replacement_id);
        assert_eq!(replacement.session_name.as_ref(), Some(&alpha));
        assert_eq!(replacement.last_session, None);
        drop(active_control);
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&beta)
                .expect("beta session survives")
                .active_window_index(),
            0,
            "a failed control identity commit must not change the target session selection"
        );
    }

    #[tokio::test]
    async fn switch_control_rejects_attach_only_flags_before_switching_session() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-control-flags-alpha");
        let beta = session_name("switch-control-flags-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;

        let control_pid = 91_345;
        let (event_tx, mut event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
        let control_id = handler
            .register_control_with_closing(
                control_pid,
                ControlModeUpgrade {
                    mode: ControlMode::Plain,
                    terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
                    initial_command_count: 0,
                },
                event_tx,
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        handler
            .set_control_session(control_pid, Some(alpha.clone()))
            .await
            .expect("initial control session set succeeds");
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ControlServerEvent::SessionChanged(Some(ref session_name)))
                | Ok(ControlServerEvent::SessionChangedAt {
                    ref session_name,
                    ..
                })
                if session_name == &alpha
        ));

        // `-T` stays refused because a control client has no key table here,
        // and the refusal comes before a `-r` toggle could mutate the client.
        for (key_table, toggle_read_only, expected_message) in [
            (
                Some("copy-mode".to_owned()),
                false,
                "switch-client -T is not available for control clients",
            ),
            (
                Some("copy-mode".to_owned()),
                true,
                "switch-client -T is not available for control clients",
            ),
        ] {
            let response = handler
                .handle_switch_client_ext3(
                    control_pid,
                    SwitchClientExt3Request {
                        target_client: None,
                        target: Some(beta.to_string()),
                        key_table,
                        last_session: false,
                        next_session: false,
                        previous_session: false,
                        toggle_read_only,
                        sort_order: None,
                        skip_environment_update: true,
                        zoom: false,
                    },
                )
                .await;
            assert_eq!(
                response,
                Response::Error(ErrorResponse {
                    error: RmuxError::Server(expected_message.to_owned()),
                })
            );
            assert!(matches!(
                event_rx.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            let active_control = handler.active_control.lock().await;
            let active = active_control
                .by_pid
                .get(&control_pid)
                .expect("control remains registered");
            assert_eq!(active.id, control_id);
            assert_eq!(active.session_name.as_ref(), Some(&alpha));
            assert!(!active.flags.read_only && !active.flags.ignore_size);
        }
    }

    #[tokio::test]
    async fn switch_client_last_session_keeps_the_captured_session_identity() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-last-identity-alpha");
        let beta = session_name("switch-last-identity-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;
        let beta_id = handler
            .state
            .lock()
            .await
            .sessions
            .session(&beta)
            .expect("beta exists")
            .id();

        let attach_pid = 91_342;
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        handler
            .register_attach(attach_pid, alpha.clone(), control_tx)
            .await;

        let killed = handler
            .handle(Request::KillSession(KillSessionRequest {
                target: beta.clone(),
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await;
        assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
        create_session(&handler, beta.clone()).await;
        {
            let mut active_attach = handler.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .expect("attach exists");
            active.last_session = Some(beta.clone());
            active.last_session_id = Some(beta_id);
        }

        let response = handler
            .handle_switch_client_ext3(
                attach_pid,
                SwitchClientExt3Request {
                    target_client: None,
                    target: None,
                    key_table: None,
                    last_session: true,
                    next_session: false,
                    previous_session: false,
                    toggle_read_only: false,
                    sort_order: None,
                    skip_environment_update: true,
                    zoom: false,
                },
            )
            .await;

        assert_eq!(
            response,
            Response::Error(ErrorResponse {
                error: RmuxError::SessionNotFound(beta.to_string()),
            })
        );
        assert!(matches!(
            control_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let active_attach = handler.active_attach.lock().await;
        assert_eq!(
            active_attach
                .by_pid
                .get(&attach_pid)
                .map(|active| active.session_name.clone()),
            Some(alpha)
        );
    }

    #[tokio::test]
    async fn apply_switch_target_resolves_window_id_target() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-window-id-alpha");
        create_session(&handler, alpha.clone()).await;
        let response = handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: alpha.clone(),
                name: None,
                detached: true,
                environment: None,
                command: None,
                start_directory: None,
                target_window_index: None,
                insert_at_target: false,
                process_command: None,
            })))
            .await;
        assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
        let window_id = {
            let state = handler.state.lock().await;
            state
                .sessions
                .session(&alpha)
                .and_then(|session| session.window_at(1))
                .map(|window| window.id().to_string())
                .expect("second window id exists")
        };
        let current = handler
            .switch_session_identity(alpha.clone())
            .await
            .expect("current session identity exists");

        let switched = handler
            .apply_switch_target(&window_id, Some(&current), TargetFindFlags::NONE, false)
            .await
            .expect("window id target resolves");
        assert_eq!(switched.session_name, alpha);
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&alpha)
                .expect("alpha session exists")
                .active_window_index(),
            1
        );
    }

    #[tokio::test]
    async fn apply_switch_target_resolves_bare_session_name_before_window_name() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-bare-window-alpha");
        let editor = session_name("editor");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, editor.clone()).await;
        let editor_window = create_window_with_name(&handler, &alpha, "editor").await;
        let current = handler
            .switch_session_identity(alpha.clone())
            .await
            .expect("current session identity exists");

        let switched = handler
            .apply_switch_target("editor", Some(&current), TargetFindFlags::NONE, false)
            .await
            .expect("bare session name resolves before colliding window name");

        assert_eq!(switched.session_name, editor);
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&alpha)
                .expect("alpha session exists")
                .active_window_index(),
            0,
            "colliding alpha window {editor_window} must not be selected"
        );
    }

    #[tokio::test]
    async fn apply_switch_target_rejects_bare_numeric_window_without_session_match() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-bare-numeric-alpha");
        create_session(&handler, alpha.clone()).await;
        create_window_with_name(&handler, &alpha, "one").await;
        create_window_with_name(&handler, &alpha, "two").await;
        let current = handler
            .switch_session_identity(alpha.clone())
            .await
            .expect("current session identity exists");

        let error = handler
            .apply_switch_target("2", Some(&current), TargetFindFlags::NONE, false)
            .await
            .expect_err("bare numeric window index is not a session target");

        assert!(matches!(error, RmuxError::SessionNotFound(session) if session == "2"));
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&alpha)
                .expect("alpha session exists")
                .active_window_index(),
            0
        );
    }

    #[tokio::test]
    async fn switch_client_dot_target_keeps_current_attached_session() {
        let handler = RequestHandler::new();
        let requester_pid = std::process::id();
        let work = session_name("switch-dot-work");
        let idle = session_name("switch-dot-idle");
        create_session(&handler, work.clone()).await;
        create_session(&handler, idle).await;
        let (control_tx, _control_rx) = mpsc::unbounded_channel();
        handler
            .register_attach(requester_pid, work.clone(), control_tx)
            .await;

        let response = handler
            .handle(Request::SwitchClientExt3(Box::new(
                SwitchClientExt3Request {
                    target_client: None,
                    target: Some(".".to_owned()),
                    key_table: None,
                    last_session: false,
                    next_session: false,
                    previous_session: false,
                    toggle_read_only: false,
                    sort_order: None,
                    skip_environment_update: false,
                    zoom: false,
                },
            )))
            .await;

        assert_eq!(
            response,
            Response::SwitchClient(SwitchClientResponse {
                session_name: work.clone()
            })
        );
        assert_eq!(
            handler
                .attached_session_name_for_command(requester_pid, "switch-client")
                .await
                .expect("attached client remains registered"),
            work
        );
    }
    async fn check_resolved_client_environment_source(
        control: bool,
        skip: bool,
        stale: bool,
        missing_snapshot: bool,
        closing: bool,
    ) {
        let handler = RequestHandler::new();
        let alpha = session_name("environment-source-alpha");
        let beta = session_name("environment-source-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;
        handler.state.lock().await.environment.set(
            ScopeSelector::Session(beta.clone()),
            "DISPLAY".into(),
            "before-switch".into(),
        );
        let mut target_process = spawn_switch_environment_child("resolved-target").await;
        let requester_process = spawn_switch_environment_child("external-requester").await;
        let target_pid = target_process.0.id();
        let (attach_tx, _attach_rx) = mpsc::unbounded_channel();
        let (event_tx, _event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
        let closing_flag = Arc::new(AtomicBool::new(false));
        let client = if control {
            let control_id = handler
                .register_control_with_closing(
                    target_pid,
                    ControlModeUpgrade {
                        mode: ControlMode::Plain,
                        terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
                        initial_command_count: 0,
                    },
                    event_tx,
                    closing_flag.clone(),
                )
                .await;
            handler
                .set_control_session(target_pid, Some(alpha.clone()))
                .await
                .unwrap();
            SwitchManagedClientIdentity::Control {
                pid: target_pid,
                control_id: control_id + u64::from(stale),
            }
        } else {
            let attach_id = handler.register_attach(target_pid, alpha, attach_tx).await;
            SwitchManagedClientIdentity::Attach {
                pid: target_pid,
                attach_id: attach_id + u64::from(stale),
            }
        };
        if missing_snapshot {
            target_process.0.kill().unwrap();
            target_process.0.wait().unwrap();
            assert!(client_environment_snapshot(target_pid).is_none());
        }
        closing_flag.store(closing, Ordering::SeqCst);
        let destination = handler.switch_session_identity(beta.clone()).await.unwrap();
        let response = handler
            .switch_managed_client_to_session_with_control_flags(
                requester_process.0.id(),
                client,
                destination,
                None,
                skip,
                ControlFlagChange::Keep,
            )
            .await;
        if stale || closing {
            assert!(matches!(response, Response::Error(_)), "{response:?}");
        } else {
            assert!(
                matches!(response, Response::SwitchClient(_)),
                "{response:?}"
            );
        }
        let state = handler.state.lock().await;
        let expected = if skip || stale || missing_snapshot || closing {
            "before-switch"
        } else {
            "resolved-target"
        };
        assert_eq!(
            state.environment.session_value(&beta, "DISPLAY"),
            Some(expected)
        );
        let mut child_environment = std::collections::HashMap::new();
        state
            .environment
            .apply_to_process_environment(Some(&beta), &mut child_environment);
        assert_eq!(
            child_environment.get("DISPLAY").map(String::as_str),
            Some(expected)
        );
    }

    #[tokio::test]
    async fn switch_environment_source_uses_resolved_attach_client_not_requester() {
        check_resolved_client_environment_source(false, false, false, false, false).await;
    }

    #[tokio::test]
    async fn switch_environment_source_uses_resolved_control_client_not_requester() {
        check_resolved_client_environment_source(true, false, false, false, false).await;
    }

    #[tokio::test]
    async fn switch_environment_source_skip_preserves_session_for_both_client_kinds() {
        for control in [false, true] {
            check_resolved_client_environment_source(control, true, false, false, false).await;
        }
    }

    #[tokio::test]
    async fn switch_environment_source_rejects_stale_client_generations_before_update() {
        for control in [false, true] {
            check_resolved_client_environment_source(control, false, true, false, false).await;
        }
    }

    #[tokio::test]
    async fn switch_environment_source_missing_snapshot_never_falls_back_to_requester() {
        for control in [false, true] {
            check_resolved_client_environment_source(control, false, false, true, false).await;
        }
    }

    #[tokio::test]
    async fn switch_environment_source_closing_control_client_cannot_update_session() {
        check_resolved_client_environment_source(true, false, false, false, true).await;
    }
}
