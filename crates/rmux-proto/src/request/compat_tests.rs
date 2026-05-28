use super::{
    NewSessionExtRequest, NewWindowRequest, RespawnPaneRequest, RespawnWindowRequest,
    ShowOptionsRequest, SplitWindowExtRequest, SplitWindowTarget,
};
use crate::{
    OptionScopeSelector, PaneTarget, SessionName, SplitDirection, TerminalSize, WindowTarget,
};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Serialize)]
struct OldNewWindowRequest {
    target: SessionName,
    name: Option<String>,
    detached: bool,
    environment: Option<Vec<String>>,
}

#[derive(Serialize)]
struct OldRespawnWindowRequest {
    target: WindowTarget,
    kill: bool,
    environment: Option<Vec<String>>,
}

#[derive(Serialize)]
struct OldSplitWindowExtRequest {
    target: SplitWindowTarget,
    direction: SplitDirection,
    before: bool,
    environment: Option<Vec<String>>,
    command: Option<Vec<String>>,
}

#[derive(Serialize)]
struct OldRespawnPaneRequest {
    target: PaneTarget,
    kill: bool,
    start_directory: Option<PathBuf>,
    environment: Option<Vec<String>>,
    command: Option<Vec<String>>,
}

#[derive(Serialize)]
struct OldNewSessionExtRequest {
    session_name: Option<SessionName>,
    working_directory: Option<String>,
    detached: bool,
    size: Option<TerminalSize>,
    environment: Option<Vec<String>>,
    group_target: Option<SessionName>,
    attach_if_exists: bool,
    detach_other_clients: bool,
    kill_other_clients: bool,
    flags: Option<Vec<String>>,
    window_name: Option<String>,
    print_session_info: bool,
    print_format: Option<String>,
    command: Option<Vec<String>>,
}

#[derive(Serialize)]
struct OldShowOptionsRequest {
    scope: OptionScopeSelector,
    name: Option<String>,
    value_only: bool,
}

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

#[test]
fn new_window_request_deserializes_old_payloads_with_defaulted_fields() {
    let bytes = bincode::serialize(&OldNewWindowRequest {
        target: session_name("alpha"),
        name: Some("logs".to_owned()),
        detached: true,
        environment: Some(vec!["FOO=1".to_owned()]),
    })
    .expect("old new-window request serializes");

    let decoded: NewWindowRequest =
        bincode::deserialize(&bytes).expect("new request decodes old payload");

    assert_eq!(decoded.target, session_name("alpha"));
    assert_eq!(decoded.name.as_deref(), Some("logs"));
    assert!(decoded.detached);
    assert_eq!(decoded.environment, Some(vec!["FOO=1".to_owned()]));
    assert_eq!(decoded.start_directory, None);
    assert_eq!(decoded.command, None);
    assert_eq!(decoded.target_window_index, None);
}

#[test]
fn respawn_window_request_deserializes_old_payloads_with_defaulted_fields() {
    let target = WindowTarget::with_window(session_name("alpha"), 2);
    let bytes = bincode::serialize(&OldRespawnWindowRequest {
        target: target.clone(),
        kill: true,
        environment: Some(vec!["FOO=1".to_owned()]),
    })
    .expect("old respawn-window request serializes");

    let decoded: RespawnWindowRequest =
        bincode::deserialize(&bytes).expect("new request decodes old payload");

    assert_eq!(decoded.target, target);
    assert!(decoded.kill);
    assert_eq!(decoded.environment, Some(vec!["FOO=1".to_owned()]));
    assert_eq!(decoded.start_directory, None);
    assert_eq!(decoded.command, None);
}

#[test]
fn new_and_respawn_window_requests_round_trip_with_spawn_fields() {
    let new_window = NewWindowRequest {
        target: session_name("alpha"),
        name: Some("logs".to_owned()),
        detached: true,
        start_directory: Some(PathBuf::from("/tmp/logs")),
        environment: Some(vec!["FOO=1".to_owned()]),
        command: Some(vec!["sleep".to_owned(), "30".to_owned()]),
        target_window_index: Some(5),
        insert_at_target: false,
    };
    let respawn_window = RespawnWindowRequest {
        target: WindowTarget::with_window(session_name("alpha"), 1),
        kill: true,
        start_directory: Some(PathBuf::from("/tmp/logs")),
        environment: Some(vec!["FOO=1".to_owned()]),
        command: Some(vec!["sleep".to_owned(), "30".to_owned()]),
    };

    assert_eq!(
        bincode::deserialize::<NewWindowRequest>(
            &bincode::serialize(&new_window).expect("new-window serializes")
        )
        .expect("new-window round-trips"),
        new_window
    );
    assert_eq!(
        bincode::deserialize::<RespawnWindowRequest>(
            &bincode::serialize(&respawn_window).expect("respawn-window serializes")
        )
        .expect("respawn-window round-trips"),
        respawn_window
    );
}

#[test]
fn split_window_ext_request_deserializes_old_payloads_with_defaulted_fields() {
    let target = SplitWindowTarget::Pane(PaneTarget::with_window(session_name("alpha"), 0, 1));
    let bytes = bincode::serialize(&OldSplitWindowExtRequest {
        target: target.clone(),
        direction: SplitDirection::Horizontal,
        before: true,
        environment: Some(vec!["FOO=1".to_owned()]),
        command: Some(vec!["printf ready".to_owned()]),
    })
    .expect("old split-window-ext request serializes");

    let decoded: SplitWindowExtRequest =
        bincode::deserialize(&bytes).expect("new request decodes old payload");

    assert_eq!(decoded.target, target);
    assert_eq!(decoded.direction, SplitDirection::Horizontal);
    assert!(decoded.before);
    assert_eq!(decoded.environment, Some(vec!["FOO=1".to_owned()]));
    assert_eq!(decoded.command, Some(vec!["printf ready".to_owned()]));
    assert_eq!(decoded.process_command, None);
    assert_eq!(decoded.start_directory, None);
    assert_eq!(decoded.keep_alive_on_exit, None);
}

#[test]
fn respawn_pane_request_deserializes_old_payloads_with_defaulted_fields() {
    let target = PaneTarget::with_window(session_name("alpha"), 0, 1);
    let bytes = bincode::serialize(&OldRespawnPaneRequest {
        target: target.clone(),
        kill: true,
        start_directory: Some(PathBuf::from("/tmp")),
        environment: Some(vec!["FOO=1".to_owned()]),
        command: Some(vec!["printf ready".to_owned()]),
    })
    .expect("old respawn-pane request serializes");

    let decoded: RespawnPaneRequest =
        bincode::deserialize(&bytes).expect("new request decodes old payload");

    assert_eq!(decoded.target, target);
    assert!(decoded.kill);
    assert_eq!(decoded.start_directory, Some(PathBuf::from("/tmp")));
    assert_eq!(decoded.environment, Some(vec!["FOO=1".to_owned()]));
    assert_eq!(decoded.command, Some(vec!["printf ready".to_owned()]));
    assert_eq!(decoded.process_command, None);
}

#[test]
fn new_session_ext_request_deserializes_old_payloads_with_defaulted_fields() {
    let bytes = bincode::serialize(&OldNewSessionExtRequest {
        session_name: Some(session_name("alpha")),
        working_directory: Some("/tmp".to_owned()),
        detached: true,
        size: Some(TerminalSize { cols: 80, rows: 24 }),
        environment: Some(vec!["FOO=1".to_owned()]),
        group_target: None,
        attach_if_exists: false,
        detach_other_clients: false,
        kill_other_clients: false,
        flags: None,
        window_name: Some("main".to_owned()),
        print_session_info: false,
        print_format: None,
        command: Some(vec!["printf ready".to_owned()]),
    })
    .expect("old new-session-ext request serializes");

    let decoded: NewSessionExtRequest =
        bincode::deserialize(&bytes).expect("new request decodes old payload");

    assert_eq!(decoded.session_name, Some(session_name("alpha")));
    assert_eq!(decoded.working_directory.as_deref(), Some("/tmp"));
    assert!(decoded.detached);
    assert_eq!(decoded.environment, Some(vec!["FOO=1".to_owned()]));
    assert_eq!(decoded.window_name.as_deref(), Some("main"));
    assert_eq!(decoded.command, Some(vec!["printf ready".to_owned()]));
    assert_eq!(decoded.process_command, None);
    assert_eq!(decoded.client_environment, None);
}

#[test]
fn show_options_request_deserializes_old_payloads_with_defaulted_fields() {
    let scope = OptionScopeSelector::SessionGlobal;
    let bytes = bincode::serialize(&OldShowOptionsRequest {
        scope: scope.clone(),
        name: Some("status-left".to_owned()),
        value_only: true,
    })
    .expect("old show-options request serializes");

    let decoded: ShowOptionsRequest =
        bincode::deserialize(&bytes).expect("new request decodes old payload");

    assert_eq!(decoded.scope, scope);
    assert_eq!(decoded.name.as_deref(), Some("status-left"));
    assert!(decoded.value_only);
    assert!(!decoded.include_inherited);
}
