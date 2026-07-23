#[cfg(test)]
use std::sync::{Arc, Mutex};

use rmux_os::identity::{IdentityResolver, UserIdentity};
use rmux_proto::request::{AttachSessionExt2Request, AttachSessionExt3Request};
#[cfg(test)]
use rmux_proto::INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH;
use rmux_proto::{
    decode_internal_list_windows_all_arguments, AttachSessionExtRequest, Request, RmuxError,
    ServerAccessRequest, SessionName, SourceFileRequest, Target,
    INTERNAL_LIST_WINDOWS_ALL_EXECUTION_PATH, INTERNAL_RUNTIME_COMMAND_EXPANSION_PATH,
};

#[path = "server_access/access_store.rs"]
mod access_store;

pub(crate) use self::access_store::{
    AccessMode, ResolvedUser, ServerAccessAdmission, ServerAccessStore,
};

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessRegistrationKind {
    Attach,
    Control,
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct AccessRegistrationPause {
    pub(crate) reached: tokio::sync::Notify,
    pub(crate) release: tokio::sync::Notify,
}

#[cfg(test)]
static ACCESS_REGISTRATION_PAUSES: Mutex<
    Vec<(AccessRegistrationKind, u32, Arc<AccessRegistrationPause>)>,
> = Mutex::new(Vec::new());

#[cfg(test)]
pub(crate) fn install_access_registration_pause(
    kind: AccessRegistrationKind,
    requester_pid: u32,
) -> Arc<AccessRegistrationPause> {
    let pause = Arc::new(AccessRegistrationPause::default());
    ACCESS_REGISTRATION_PAUSES
        .lock()
        .expect("access registration pause lock")
        .push((kind, requester_pid, pause.clone()));
    pause
}

#[cfg(test)]
pub(crate) async fn pause_before_access_registration(
    kind: AccessRegistrationKind,
    requester_pid: u32,
) {
    let pause = {
        let mut pauses = ACCESS_REGISTRATION_PAUSES
            .lock()
            .expect("access registration pause lock");
        pauses
            .iter()
            .position(|(paused_kind, paused_pid, _)| {
                *paused_kind == kind && *paused_pid == requester_pid
            })
            .map(|position| pauses.swap_remove(position).2)
    };
    if let Some(pause) = pause {
        pause.reached.notify_one();
        pause.release.notified().await;
    }
}

pub(crate) fn current_owner_uid() -> u32 {
    current_user_identity()
        .ok()
        .and_then(|identity| match identity {
            UserIdentity::Uid(uid) => Some(uid),
            UserIdentity::Sid(_) => None,
        })
        .unwrap_or(0)
}

fn current_user_identity() -> std::io::Result<UserIdentity> {
    IdentityResolver::current()
}

pub(crate) fn resolve_user(value: &str) -> Result<ResolvedUser, RmuxError> {
    #[cfg(unix)]
    if let Some(user) = IdentityResolver::unix_user_by_name(value).map_err(resolve_user_error)? {
        return Ok(ResolvedUser {
            uid: user.uid,
            name: user.name,
        });
    }

    let uid = value
        .parse::<u32>()
        .map_err(|_| RmuxError::Server(format!("unknown user: {value}")))?;
    #[cfg(unix)]
    let Some(user) = IdentityResolver::unix_user_by_uid(uid).map_err(resolve_user_error)?
    else {
        return Err(RmuxError::Server(format!("unknown user: {value}")));
    };

    #[cfg(windows)]
    let _ = uid;
    #[cfg(windows)]
    return Err(RmuxError::Server(format!("unknown user: {value}")));

    #[cfg(unix)]
    Ok(ResolvedUser {
        uid,
        name: user.name,
    })
}

#[cfg(unix)]
fn resolve_user_error(error: std::io::Error) -> RmuxError {
    RmuxError::Server(format!("failed to resolve user: {error}"))
}

#[must_use]
pub(crate) fn user_name_for_uid(uid: u32) -> String {
    #[cfg(unix)]
    {
        IdentityResolver::unix_user_by_uid(uid)
            .ok()
            .flatten()
            .map(|entry| entry.name)
            .unwrap_or_else(|| uid.to_string())
    }

    #[cfg(windows)]
    {
        uid.to_string()
    }
}

pub(crate) fn apply_access_policy(request: Request, can_write: bool) -> Result<Request, RmuxError> {
    if can_write {
        return Ok(request);
    }

    match request {
        Request::AttachSession(request) => Ok(Request::AttachSessionExt(AttachSessionExtRequest {
            target: Some(request.target),
            detach_other_clients: false,
            kill_other_clients: false,
            read_only: true,
            skip_environment_update: true,
            flags: None,
        })),
        Request::AttachSessionExt(request) => Ok(Request::AttachSessionExt(
            sanitize_read_only_attach_session_ext(request),
        )),
        Request::AttachSessionExt2(request) => Ok(Request::AttachSessionExt2(Box::new(
            sanitize_read_only_attach_session_ext2(*request),
        ))),
        Request::AttachSessionExt3(request) => Ok(Request::AttachSessionExt3(Box::new(
            sanitize_read_only_attach_session_ext3(*request),
        ))),
        request if read_only_request_allowed(&request) => Ok(request),
        _ => Err(RmuxError::Server("client is read-only".to_owned())),
    }
}

fn sanitize_read_only_attach_session_ext(
    mut request: AttachSessionExtRequest,
) -> AttachSessionExtRequest {
    request.detach_other_clients = false;
    request.kill_other_clients = false;
    request.read_only = true;
    request.skip_environment_update = true;
    request
}

fn sanitize_read_only_attach_session_ext2(
    mut request: AttachSessionExt2Request,
) -> AttachSessionExt2Request {
    request.target = read_only_attach_target(request.target, request.target_spec.as_deref());
    request.target_spec = None;
    request.detach_other_clients = false;
    request.kill_other_clients = false;
    request.read_only = true;
    request.skip_environment_update = true;
    request.working_directory = None;
    request.client_size = None;
    request
}

fn sanitize_read_only_attach_session_ext3(
    mut request: AttachSessionExt3Request,
) -> AttachSessionExt3Request {
    request.target = read_only_attach_target(request.target, request.target_spec.as_deref());
    request.target_spec = None;
    request.detach_other_clients = false;
    request.kill_other_clients = false;
    request.read_only = true;
    request.skip_environment_update = true;
    request.working_directory = None;
    request.client_size = None;
    request
}

fn read_only_attach_target(
    target: Option<SessionName>,
    target_spec: Option<&str>,
) -> Option<SessionName> {
    target.or_else(|| {
        target_spec
            .and_then(|spec| Target::parse(spec).ok())
            .map(|target| target.session_name().clone())
    })
}

fn read_only_request_allowed(request: &Request) -> bool {
    match request {
        Request::CapturePane(request) => {
            capture_pane_request_is_read_only(request.print, request.buffer_name.as_deref())
        }
        Request::CapturePaneTargetAction(request) => {
            capture_pane_request_is_read_only(request.print, request.buffer_name.as_deref())
        }
        Request::DisplayMessage(request) => display_message_request_is_read_only(request.print),
        Request::DisplayMessageExt(request) => display_message_request_is_read_only(request.print),
        Request::SourceFile(request) => {
            internal_runtime_command_expansion_is_read_only(request)
                || internal_list_windows_all_is_read_only(request)
        }
        _ => matches!(
            request,
            Request::HasSession(_)
                | Request::ListWindows(_)
                | Request::ListPanes(_)
                | Request::AttachSession(_)
                | Request::AttachSessionExt(_)
                | Request::AttachSessionExt2(_)
                | Request::AttachSessionExt3(_)
                | Request::ListClients(_)
                | Request::ShowOptions(_)
                | Request::ShowEnvironment(_)
                | Request::ShowHooks(_)
                | Request::ShowBuffer(_)
                | Request::ListBuffers(_)
                | Request::SubscribePaneOutput(_)
                | Request::SubscribePaneOutputRef(_)
                | Request::UnsubscribePaneOutput(_)
                | Request::PaneOutputCursor(_)
                | Request::PaneSnapshot(_)
                | Request::PaneSnapshotRef(_)
                | Request::PaneOptionGet(_)
                | Request::SubscribePaneState(_)
                | Request::PaneStateCursor(_)
                | Request::UnsubscribePaneState(_)
                | Request::PaneForegroundState(_)
                | Request::ResolveTarget(_)
                | Request::SdkWaitForOutput(_)
                | Request::SdkWaitForOutputRef(_)
                | Request::CancelSdkWait(_)
                | Request::ShowMessages(_)
                | Request::ListSessions(_)
                | Request::ListKeys(_)
                | Request::ControlMode(_)
                | Request::Handshake(_)
                | Request::DaemonStatus(_)
                | Request::ServerAccess(ServerAccessRequest { list: true, .. })
        ),
    }
}

fn internal_runtime_command_expansion_is_read_only(request: &SourceFileRequest) -> bool {
    request.paths.as_slice() == [INTERNAL_RUNTIME_COMMAND_EXPANSION_PATH]
        && !request.quiet
        && request.parse_only
        && request.verbose
        && !request.expand_paths
        && request.target.is_none()
        && request.stdin.is_some()
}

fn internal_list_windows_all_is_read_only(request: &SourceFileRequest) -> bool {
    request.paths.as_slice() == [INTERNAL_LIST_WINDOWS_ALL_EXECUTION_PATH]
        && !request.quiet
        && !request.parse_only
        && !request.verbose
        && !request.expand_paths
        && request.target.is_none()
        && request
            .stdin
            .as_deref()
            .and_then(decode_internal_list_windows_all_arguments)
            .is_some()
}

fn capture_pane_request_is_read_only(print: bool, buffer_name: Option<&str>) -> bool {
    print && buffer_name.is_none()
}

fn display_message_request_is_read_only(print: bool) -> bool {
    print
}

pub(crate) fn validate_server_access_request(
    request: &ServerAccessRequest,
) -> Result<(), RmuxError> {
    if request.target.is_some() {
        return Err(RmuxError::Server(
            "command server-access: unknown flag -t".to_owned(),
        ));
    }
    if request.list {
        return Ok(());
    }
    if request.add && request.deny {
        return Err(RmuxError::Server(
            "-a and -d cannot be used together".to_owned(),
        ));
    }
    if request.read_only && request.write {
        return Err(RmuxError::Server(
            "-r and -w cannot be used together".to_owned(),
        ));
    }
    #[cfg(windows)]
    {
        Err(RmuxError::Server(
            "server-access user mutations are unsupported on Windows; named-pipe access is scoped to the current Windows SID".to_owned(),
        ))
    }
    #[cfg(not(windows))]
    {
        if request.user.is_none() {
            return Err(RmuxError::Server("missing user argument".to_owned()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_proto::{
        AttachSessionExt2Request, AttachSessionExt3Request, AttachSessionExtRequest,
        AttachSessionRequest, CancelSdkWaitRequest, CapturePaneRequest,
        CapturePaneTargetActionRequest, ClockModeRequest, CopyModeRequest, DetachClientExtRequest,
        DetachClientRequest, DisplayMessageExtRequest, DisplayMessageRequest, DisplayPanesRequest,
        LastPaneRequest, LastWindowRequest, NextLayoutRequest, NextWindowRequest,
        PaneOutputSubscriptionStart, PaneSnapshotRequest, PaneTarget, PreviousLayoutRequest,
        PreviousWindowRequest, RefreshClientRequest, ResolveTargetRequest, ResolveTargetType,
        SdkWaitForOutputRequest, SdkWaitId, SdkWaitOwnerId, SelectPaneAdjacentRequest,
        SelectPaneDirection, SelectPaneRequest, SessionName, SuspendClientRequest,
        SwitchClientExt2Request, SwitchClientExt3Request, SwitchClientExtRequest,
        SwitchClientRequest, TerminalSize, WindowTarget,
    };

    #[test]
    fn access_store_can_key_owner_by_windows_sid() {
        let owner = UserIdentity::Sid("S-1-5-21-1000".into());
        let store = ServerAccessStore::new_for_identity(0, owner.clone());

        assert_eq!(store.owner_identity(), &owner);
        assert_eq!(store.mode_for_identity(&owner), Some(AccessMode::ReadWrite));
        assert_eq!(
            store.mode_for_identity(&UserIdentity::Sid("S-1-5-21-2000".into())),
            None
        );
    }

    #[test]
    fn access_admission_keeps_identity_epoch_across_mode_changes() {
        let mut store =
            ServerAccessStore::new_for_identity(42, UserIdentity::Sid("S-1-5-21-owner".into()));
        let identity = UserIdentity::Uid(1001);
        store
            .set_mode(1001, AccessMode::ReadWrite)
            .expect("grant access");
        let admission = store
            .admission_for_identity(&identity)
            .expect("admission exists");

        store
            .set_mode(1001, AccessMode::ReadOnly)
            .expect("downgrade access");
        assert_eq!(
            store.revalidate_admission(&admission, &identity),
            Some(AccessMode::ReadOnly)
        );

        store
            .set_mode(1001, AccessMode::ReadWrite)
            .expect("restore access");
        assert_eq!(
            store.revalidate_admission(&admission, &identity),
            Some(AccessMode::ReadWrite)
        );
    }

    #[test]
    fn access_admission_is_invalid_after_remove_and_reinsert() {
        let mut store =
            ServerAccessStore::new_for_identity(42, UserIdentity::Sid("S-1-5-21-owner".into()));
        let identity = UserIdentity::Uid(1001);
        store
            .set_mode(1001, AccessMode::ReadWrite)
            .expect("grant access");
        let stale = store
            .admission_for_identity(&identity)
            .expect("admission exists");

        store.remove_uid(1001).expect("revoke access");
        store
            .set_mode(1001, AccessMode::ReadWrite)
            .expect("regrant access");

        assert_eq!(store.revalidate_admission(&stale, &identity), None);
        assert_eq!(store.revalidate_detached_admission(&stale), None);
    }

    #[test]
    fn access_admission_is_not_invalidated_by_another_identity_mutation() {
        let mut store =
            ServerAccessStore::new_for_identity(42, UserIdentity::Sid("S-1-5-21-owner".into()));
        let identity = UserIdentity::Uid(1001);
        store
            .set_mode(1001, AccessMode::ReadWrite)
            .expect("grant first identity");
        let admission = store
            .admission_for_identity(&identity)
            .expect("admission exists");

        store
            .set_mode(1002, AccessMode::ReadOnly)
            .expect("grant second identity");
        store
            .set_mode(1002, AccessMode::ReadWrite)
            .expect("change second identity");
        store.remove_uid(1002).expect("revoke second identity");

        assert_eq!(
            store.revalidate_admission(&admission, &identity),
            Some(AccessMode::ReadWrite)
        );
    }

    #[test]
    fn detached_admission_never_widens_its_initial_write_cap() {
        let mut store =
            ServerAccessStore::new_for_identity(42, UserIdentity::Sid("S-1-5-21-owner".into()));
        store
            .set_mode(1001, AccessMode::ReadWrite)
            .expect("grant access");
        let admission = store
            .admission_for_identity_with_write_cap(&UserIdentity::Uid(1001), false)
            .expect("admission exists");

        assert_eq!(
            store.revalidate_detached_admission(&admission),
            Some(AccessMode::ReadOnly)
        );
    }

    #[cfg(windows)]
    #[test]
    fn access_store_does_not_trust_uid_zero_on_windows() {
        let owner = UserIdentity::Sid("S-1-5-21-1000".into());
        let store = ServerAccessStore::new_for_identity(0, owner.clone());

        assert_eq!(store.mode_for_identity(&owner), Some(AccessMode::ReadWrite));
        assert_eq!(store.mode_for_identity(&UserIdentity::Uid(0)), None);
    }

    #[cfg(unix)]
    #[test]
    fn access_store_trusts_uid_zero_only_on_unix() {
        let owner = UserIdentity::Uid(1000);
        let store = ServerAccessStore::new_for_identity(1000, owner);

        assert_eq!(
            store.mode_for_identity(&UserIdentity::Uid(0)),
            Some(AccessMode::ReadWrite)
        );
    }

    #[test]
    fn access_store_tracks_current_platform_identity_for_owner() {
        let owner = current_user_identity().expect("current identity");
        let store = ServerAccessStore::new(current_owner_uid());

        assert_eq!(store.mode_for_identity(&owner), Some(AccessMode::ReadWrite));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_user_uses_platform_account_database() {
        let UserIdentity::Uid(uid) = current_user_identity().expect("current identity") else {
            panic!("Unix current identity should be a uid");
        };
        let by_uid = resolve_user(&uid.to_string()).expect("current uid resolves");
        let by_name = resolve_user(&by_uid.name).expect("current name resolves");

        assert_eq!(by_uid.uid, uid);
        assert_eq!(by_name.uid, uid);
        assert_eq!(by_name.name, by_uid.name);
    }

    #[test]
    fn read_only_access_allows_sdk_wait_observation_and_cancel() {
        let target = PaneTarget::new(SessionName::new("s").expect("session name"), 0);
        let wait = Request::SdkWaitForOutput(SdkWaitForOutputRequest {
            owner_id: SdkWaitOwnerId::new(7),
            wait_id: SdkWaitId::new(1),
            target,
            bytes: b"ready".to_vec(),
            start: PaneOutputSubscriptionStart::Now,
        });
        let cancel = Request::CancelSdkWait(CancelSdkWaitRequest {
            owner_id: SdkWaitOwnerId::new(7),
            wait_id: SdkWaitId::new(1),
        });

        assert_eq!(
            apply_access_policy(wait.clone(), false).expect("SDK wait is read-only observation"),
            wait
        );
        assert_eq!(
            apply_access_policy(cancel.clone(), false)
                .expect("SDK wait cancel is read-only cleanup"),
            cancel
        );
    }

    #[test]
    fn read_only_access_allows_only_the_non_mutating_internal_source_shape() {
        let expansion = Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec![INTERNAL_RUNTIME_COMMAND_EXPANSION_PATH.to_owned()],
            quiet: false,
            parse_only: true,
            verbose: true,
            expand_paths: false,
            target: None,
            caller_cwd: None,
            stdin: Some("[\"list-sessions\"]".to_owned()),
        }));
        assert_eq!(
            apply_access_policy(expansion.clone(), false).expect("runtime expansion is read-only"),
            expansion
        );

        let list_windows_payload = rmux_proto::encode_internal_runtime_command_arguments(&[
            "list-windows".to_owned(),
            "-a".to_owned(),
            "-F".to_owned(),
            "#{window_name}".to_owned(),
        ])
        .expect("list-windows argv encodes");
        let list_windows = Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec![INTERNAL_LIST_WINDOWS_ALL_EXECUTION_PATH.to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            caller_cwd: None,
            stdin: Some(list_windows_payload),
        }));
        assert_eq!(
            apply_access_policy(list_windows.clone(), false)
                .expect("validated list-windows -a is read-only"),
            list_windows
        );

        let mutating_payload = rmux_proto::encode_internal_runtime_command_arguments(&[
            "kill-server".to_owned(),
            "-a".to_owned(),
        ])
        .expect("mutating argv encodes");
        let mutating_list_path = Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec![INTERNAL_LIST_WINDOWS_ALL_EXECUTION_PATH.to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            caller_cwd: None,
            stdin: Some(mutating_payload),
        }));
        assert!(apply_access_policy(mutating_list_path, false).is_err());

        let assignments = Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec![rmux_proto::INTERNAL_PARSE_TIME_ASSIGNMENTS_PATH.to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            caller_cwd: None,
            stdin: Some("FOO=bar".to_owned()),
        }));
        assert!(apply_access_policy(assignments, false).is_err());

        let canonical_execution = Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec![INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH.to_owned()],
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            caller_cwd: None,
            stdin: Some("list-sessions".to_owned()),
        }));
        assert!(apply_access_policy(canonical_execution, false).is_err());

        let public_source = Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec!["-".to_owned()],
            quiet: false,
            parse_only: true,
            verbose: true,
            expand_paths: false,
            target: None,
            caller_cwd: None,
            stdin: Some("list-sessions".to_owned()),
        }));
        assert!(apply_access_policy(public_source, false).is_err());
    }

    #[test]
    fn read_only_access_allows_sdk_target_discovery_and_snapshot() {
        let session = session_name();
        let pane = PaneTarget::new(session.clone(), 0);
        let resolve = Request::ResolveTarget(ResolveTargetRequest {
            target: Some("s:0.0".to_owned()),
            target_type: ResolveTargetType::Pane,
            window_index: false,
            prefer_unattached: false,
        });
        let snapshot = Request::PaneSnapshot(PaneSnapshotRequest { target: pane });

        assert_eq!(
            apply_access_policy(resolve.clone(), false)
                .expect("target resolution is read-only discovery"),
            resolve
        );
        assert_eq!(
            apply_access_policy(snapshot.clone(), false)
                .expect("pane snapshot is read-only observation"),
            snapshot
        );
    }

    #[test]
    fn read_only_access_allows_pane_state_observation() {
        let session = session_name();
        let pane = PaneTarget::new(session.clone(), 0);
        let pane_ref = rmux_proto::PaneTargetRef::slot(pane.clone());
        let subscription_id = rmux_proto::PaneStateSubscriptionId::new(1);
        let requests = [
            Request::PaneOptionGet(rmux_proto::PaneOptionGetRequest {
                target: pane_ref.clone(),
                name: "@agent.kind".to_owned(),
            }),
            Request::SubscribePaneState(rmux_proto::SubscribePaneStateRequest {
                target: pane_ref.clone(),
                include_title: true,
                include_options: true,
                include_foreground: true,
            }),
            Request::PaneStateCursor(rmux_proto::PaneStateCursorRequest {
                subscription_id,
                after_revision: 0,
                wait: true,
                max_events: Some(1),
            }),
            Request::UnsubscribePaneState(rmux_proto::UnsubscribePaneStateRequest {
                subscription_id,
            }),
            Request::PaneForegroundState(rmux_proto::PaneForegroundStateRequest {
                target: pane_ref,
            }),
        ];

        for request in requests {
            assert_eq!(
                apply_access_policy(request.clone(), false)
                    .expect("pane-state SDK request is read-only observation"),
                request
            );
        }
    }

    #[test]
    fn read_only_access_allows_capture_pane_target_action() {
        let capture =
            Request::CapturePaneTargetAction(Box::new(capture_pane_target_action(true, None)));

        assert_eq!(
            apply_access_policy(capture.clone(), false)
                .expect("capture-pane target action is read-only observation"),
            capture
        );
    }

    #[test]
    fn read_only_access_rejects_capture_pane_target_action_that_writes_buffer() {
        let unnamed_buffer =
            Request::CapturePaneTargetAction(Box::new(capture_pane_target_action(false, None)));
        let named_buffer = Request::CapturePaneTargetAction(Box::new(capture_pane_target_action(
            false,
            Some("clip".to_owned()),
        )));

        assert_read_only_rejected(unnamed_buffer);
        assert_read_only_rejected(named_buffer);
    }

    #[test]
    fn read_only_access_allows_direct_capture_pane_output_only() {
        let capture = Request::CapturePane(Box::new(capture_pane_request(true, None)));

        assert_eq!(
            apply_access_policy(capture.clone(), false)
                .expect("printed capture-pane is read-only observation"),
            capture
        );
    }

    #[test]
    fn read_only_access_rejects_direct_capture_pane_that_writes_buffer() {
        let unnamed_buffer = Request::CapturePane(Box::new(capture_pane_request(false, None)));
        let named_buffer = Request::CapturePane(Box::new(capture_pane_request(
            false,
            Some("clip".to_owned()),
        )));

        assert_read_only_rejected(unnamed_buffer);
        assert_read_only_rejected(named_buffer);
    }

    #[test]
    fn read_only_access_allows_printed_display_message() {
        let message = Request::DisplayMessage(DisplayMessageRequest {
            target: None,
            print: true,
            message: Some("#{session_name}".to_owned()),
            empty_target_context: false,
        });
        let extended = Request::DisplayMessageExt(Box::new(DisplayMessageExtRequest {
            target: None,
            print: true,
            message: Some("#{client_name}".to_owned()),
            target_client: Some("=".to_owned()),
            empty_target_context: false,
        }));

        assert_eq!(
            apply_access_policy(message.clone(), false)
                .expect("display-message -p is read-only format expansion"),
            message
        );
        assert_eq!(
            apply_access_policy(extended.clone(), false)
                .expect("display-message -p -c is read-only format expansion"),
            extended
        );
    }

    #[test]
    fn read_only_access_rejects_display_overlays() {
        assert_read_only_rejected(Request::DisplayMessage(DisplayMessageRequest {
            target: None,
            print: false,
            message: Some("visible overlay".to_owned()),
            empty_target_context: false,
        }));
        assert_read_only_rejected(Request::DisplayMessageExt(Box::new(
            DisplayMessageExtRequest {
                target: None,
                print: false,
                message: Some("visible overlay".to_owned()),
                target_client: Some("=".to_owned()),
                empty_target_context: false,
            },
        )));
        assert_read_only_rejected(Request::DisplayPanes(Box::new(DisplayPanesRequest {
            target: session_name(),
            duration_ms: None,
            non_blocking: false,
            no_command: false,
            template: None,
            target_client: None,
        })));
    }

    #[test]
    fn read_only_access_rejects_session_window_and_pane_mutations() {
        let session = session_name();
        let window = WindowTarget::new(session.clone());
        let pane = PaneTarget::new(session.clone(), 0);
        let select_pane = SelectPaneRequest {
            target: pane.clone(),
            title: None,
            input_disabled: None,
            preserve_zoom: false,
            style: None,
        };

        for request in [
            Request::NextWindow(NextWindowRequest {
                target: session.clone(),
                alerts_only: false,
            }),
            Request::PreviousWindow(PreviousWindowRequest {
                target: session.clone(),
                alerts_only: false,
            }),
            Request::LastWindow(LastWindowRequest {
                target: session.clone(),
            }),
            Request::LastPane(LastPaneRequest {
                target: window.clone(),
                preserve_zoom: false,
                input_disabled: None,
            }),
            Request::NextLayout(NextLayoutRequest {
                target: window.clone(),
            }),
            Request::PreviousLayout(PreviousLayoutRequest {
                target: window.clone(),
            }),
            Request::SelectPane(Box::new(select_pane)),
            Request::SelectPaneAdjacent(SelectPaneAdjacentRequest {
                target: pane,
                direction: SelectPaneDirection::Right,
                preserve_zoom: false,
            }),
            Request::CopyMode(CopyModeRequest {
                target: None,
                page_down: false,
                exit_on_scroll: false,
                hide_position: false,
                mouse_drag_start: false,
                cancel_mode: false,
                scrollbar_scroll: false,
                source: None,
                page_up: false,
            }),
            Request::ClockMode(ClockModeRequest { target: None }),
        ] {
            assert_read_only_rejected(request);
        }
    }

    #[test]
    fn read_only_access_rejects_client_control_mutations() {
        let session = session_name();
        for request in [
            Request::SwitchClient(SwitchClientRequest {
                target: session.clone(),
            }),
            Request::SwitchClientExt(SwitchClientExtRequest {
                target: Some(session.clone()),
                key_table: None,
            }),
            Request::SwitchClientExt2(Box::new(SwitchClientExt2Request {
                target: Some(session.clone()),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                flags: None,
                sort_order: None,
                skip_environment_update: false,
            })),
            Request::SwitchClientExt3(Box::new(SwitchClientExt3Request {
                target_client: Some("123".to_owned()),
                target: Some(session.to_string()),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                sort_order: None,
                skip_environment_update: false,
                zoom: false,
            })),
            Request::DetachClient(DetachClientRequest),
            Request::DetachClientExt(DetachClientExtRequest {
                target_client: Some("123".to_owned()),
                all_other_clients: false,
                target_session: None,
                kill_on_detach: false,
                exec_command: None,
            }),
            Request::RefreshClient(Box::new(RefreshClientRequest {
                target_client: Some("123".to_owned()),
                adjustment: None,
                clear_pan: false,
                pan_left: false,
                pan_right: false,
                pan_up: false,
                pan_down: false,
                status_only: true,
                clipboard_query: false,
                flags: None,
                flags_alias: None,
                subscriptions: Vec::new(),
                subscriptions_format: Vec::new(),
                control_size: None,
                colour_report: None,
            })),
            Request::SuspendClient(SuspendClientRequest {
                target_client: Some("123".to_owned()),
            }),
        ] {
            assert_read_only_rejected(request);
        }
    }

    #[test]
    fn read_only_access_sanitizes_attach_session_ext_options() {
        let session = session_name();
        let request = Request::AttachSessionExt(AttachSessionExtRequest {
            target: Some(session.clone()),
            detach_other_clients: true,
            kill_other_clients: true,
            read_only: false,
            skip_environment_update: false,
            flags: Some(vec!["active-pane".to_owned()]),
        });

        let Request::AttachSessionExt(sanitized) =
            apply_access_policy(request, false).expect("read-only attach is allowed")
        else {
            panic!("expected sanitized attach-session ext request");
        };

        assert_eq!(sanitized.target, Some(session));
        assert!(!sanitized.detach_other_clients);
        assert!(!sanitized.kill_other_clients);
        assert!(sanitized.read_only);
        assert!(sanitized.skip_environment_update);
        assert_eq!(sanitized.flags, Some(vec!["active-pane".to_owned()]));
    }

    #[test]
    fn read_only_access_sanitizes_legacy_attach_session_options() {
        let session = session_name();
        let request = Request::AttachSession(AttachSessionRequest {
            target: session.clone(),
        });

        let Request::AttachSessionExt(sanitized) =
            apply_access_policy(request, false).expect("read-only legacy attach is allowed")
        else {
            panic!("expected sanitized attach-session ext request");
        };

        assert_eq!(sanitized.target, Some(session));
        assert!(!sanitized.detach_other_clients);
        assert!(!sanitized.kill_other_clients);
        assert!(sanitized.read_only);
        assert!(sanitized.skip_environment_update);
        assert_eq!(sanitized.flags, None);
    }

    #[test]
    fn read_only_access_sanitizes_attach_session_ext2_options() {
        let request = Request::AttachSessionExt2(Box::new(AttachSessionExt2Request {
            target: None,
            target_spec: Some("s:1.2".to_owned()),
            detach_other_clients: true,
            kill_other_clients: true,
            read_only: false,
            skip_environment_update: false,
            flags: None,
            working_directory: Some("/tmp".to_owned()),
            client_terminal: Default::default(),
            client_size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),
        }));

        let Request::AttachSessionExt2(sanitized) =
            apply_access_policy(request, false).expect("read-only attach is allowed")
        else {
            panic!("expected sanitized attach-session ext2 request");
        };

        assert_eq!(sanitized.target, Some(session_name()));
        assert_eq!(sanitized.target_spec, None);
        assert!(!sanitized.detach_other_clients);
        assert!(!sanitized.kill_other_clients);
        assert!(sanitized.read_only);
        assert!(sanitized.skip_environment_update);
        assert_eq!(sanitized.working_directory, None);
        assert_eq!(sanitized.client_size, None);
    }

    #[test]
    fn read_only_access_sanitizes_attach_session_ext3_options() {
        let request = Request::AttachSessionExt3(Box::new(AttachSessionExt3Request {
            target: None,
            target_spec: Some("s:2.3".to_owned()),
            detach_other_clients: true,
            kill_other_clients: true,
            read_only: false,
            skip_environment_update: false,
            flags: None,
            working_directory: Some("/tmp".to_owned()),
            client_terminal: Default::default(),
            client_size: Some(TerminalSize {
                cols: 100,
                rows: 30,
            }),
            attach_capabilities: vec!["attach-render".to_owned()],
        }));

        let Request::AttachSessionExt3(sanitized) =
            apply_access_policy(request, false).expect("read-only attach is allowed")
        else {
            panic!("expected sanitized attach-session ext3 request");
        };

        assert_eq!(sanitized.target, Some(session_name()));
        assert_eq!(sanitized.target_spec, None);
        assert!(!sanitized.detach_other_clients);
        assert!(!sanitized.kill_other_clients);
        assert!(sanitized.read_only);
        assert!(sanitized.skip_environment_update);
        assert_eq!(sanitized.working_directory, None);
        assert_eq!(sanitized.client_size, None);
        assert_eq!(sanitized.attach_capabilities, vec!["attach-render"]);
    }

    fn assert_read_only_rejected(request: Request) {
        let error =
            apply_access_policy(request, false).expect_err("write request must be rejected");
        assert_eq!(error.to_string(), "server error: client is read-only");
    }

    fn session_name() -> SessionName {
        SessionName::new("s").expect("session name")
    }

    fn capture_pane_target_action(
        print: bool,
        buffer_name: Option<String>,
    ) -> CapturePaneTargetActionRequest {
        CapturePaneTargetActionRequest {
            target: Some("s:0.0".to_owned()),
            start: None,
            end: None,
            print,
            buffer_name,
            alternate: false,
            escape_ansi: false,
            escape_sequences: false,
            include_format: false,
            hyperlinks: false,
            line_numbers: false,
            join_wrapped: false,
            use_mode_screen: false,
            preserve_trailing_spaces: false,
            do_not_trim_spaces: false,
            pending_input: false,
            quiet: false,
            start_is_absolute: false,
            end_is_absolute: false,
        }
    }

    fn capture_pane_request(print: bool, buffer_name: Option<String>) -> CapturePaneRequest {
        CapturePaneRequest {
            target: PaneTarget::new(SessionName::new("s").expect("session name"), 0),
            start: None,
            end: None,
            print,
            buffer_name,
            alternate: false,
            escape_ansi: false,
            escape_sequences: false,
            include_format: false,
            hyperlinks: false,
            line_numbers: false,
            join_wrapped: false,
            use_mode_screen: false,
            preserve_trailing_spaces: false,
            do_not_trim_spaces: false,
            pending_input: false,
            quiet: false,
            start_is_absolute: false,
            end_is_absolute: false,
        }
    }

    #[cfg(windows)]
    #[test]
    fn server_access_user_mutations_are_explicitly_unsupported_on_windows() {
        let error = validate_server_access_request(&ServerAccessRequest {
            add: true,
            deny: false,
            list: false,
            read_only: false,
            write: false,
            target: None,
            user: Some("someone".to_owned()),
        })
        .expect_err("Windows cannot safely map server-access users to Unix UIDs");

        assert!(error
            .to_string()
            .contains("unsupported on Windows; named-pipe access"));
    }

    #[cfg(windows)]
    #[test]
    fn server_access_list_still_validates_on_windows() {
        validate_server_access_request(&ServerAccessRequest {
            add: false,
            deny: false,
            list: true,
            read_only: false,
            write: false,
            target: None,
            user: None,
        })
        .expect("server-access -l remains read-only and portable");
    }
}
