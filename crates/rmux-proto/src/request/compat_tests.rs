use super::{
    DisplayMessageExtRequest, DisplayMessageRequest, LastPaneRequest, MoveWindowRequest,
    MoveWindowTarget, NewSessionExtRequest, NewWindowRequest, RespawnPaneRequest,
    RespawnWindowRequest, SelectPaneAdjacentRequest, SelectPaneRequest, SetOptionByNameRequest,
    ShowOptionsRequest, SplitWindowExtRequest, SplitWindowTarget,
};
use crate::{
    OptionScopeSelector, PaneTarget, ProcessCommand, SelectPaneDirection, SessionName,
    SetOptionMode, SplitDirection, Target, TerminalSize, WindowTarget,
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
struct OldDisplayMessageRequest {
    target: Option<Target>,
    print: bool,
    message: Option<String>,
}

#[derive(Serialize)]
struct OldDisplayMessageExtRequest {
    target: Option<Target>,
    print: bool,
    message: Option<String>,
    target_client: Option<String>,
}

#[derive(Serialize)]
struct OldShowOptionsRequest {
    scope: OptionScopeSelector,
    name: Option<String>,
    value_only: bool,
}

#[derive(Serialize)]
struct OldSetOptionByNameRequest {
    scope: OptionScopeSelector,
    name: String,
    value: Option<String>,
    mode: SetOptionMode,
    only_if_unset: bool,
    unset: bool,
    unset_pane_overrides: bool,
}

#[derive(Serialize)]
struct OldMoveWindowRequest {
    source: Option<WindowTarget>,
    target: MoveWindowTarget,
    renumber: bool,
    kill_destination: bool,
    detached: bool,
}

#[derive(Serialize)]
struct OldLastPaneRequest {
    target: WindowTarget,
}

#[derive(Serialize)]
struct OldSelectPaneRequest {
    target: PaneTarget,
    title: Option<String>,
    input_disabled: Option<bool>,
}

#[derive(Serialize)]
struct OldSelectPaneAdjacentRequest {
    target: PaneTarget,
    direction: SelectPaneDirection,
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
    assert_eq!(decoded.process_command, None);
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
        process_command: Some(ProcessCommand::Argv(vec!["sleep".to_owned()])),
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
    assert!(!decoded.detached);
    assert_eq!(decoded.size, None);
    assert!(!decoded.preserve_zoom);
    assert!(!decoded.full_size);
}

#[test]
fn display_message_requests_deserialize_old_payloads_with_defaulted_fields() {
    let target = Some(Target::Pane(PaneTarget::with_window(
        session_name("alpha"),
        0,
        1,
    )));
    let bytes = bincode::serialize(&OldDisplayMessageRequest {
        target: target.clone(),
        print: true,
        message: Some("#{pane_id}".to_owned()),
    })
    .expect("old display-message request serializes");

    let decoded: DisplayMessageRequest =
        bincode::deserialize(&bytes).expect("new request decodes old payload");

    assert_eq!(decoded.target, target);
    assert!(decoded.print);
    assert_eq!(decoded.message.as_deref(), Some("#{pane_id}"));
    assert!(!decoded.empty_target_context);

    let ext_target = Some(Target::Session(session_name("alpha")));
    let bytes = bincode::serialize(&OldDisplayMessageExtRequest {
        target: ext_target.clone(),
        print: false,
        message: Some("#{client_name}".to_owned()),
        target_client: Some("client".to_owned()),
    })
    .expect("old display-message-ext request serializes");

    let decoded: DisplayMessageExtRequest =
        bincode::deserialize(&bytes).expect("new request decodes old payload");

    assert_eq!(decoded.target, ext_target);
    assert!(!decoded.print);
    assert_eq!(decoded.message.as_deref(), Some("#{client_name}"));
    assert_eq!(decoded.target_client.as_deref(), Some("client"));
    assert!(!decoded.empty_target_context);
}

#[test]
fn split_window_ext_request_round_trips_current_payload_fields() {
    let target = SplitWindowTarget::Pane(PaneTarget::with_window(session_name("alpha"), 0, 1));
    let request = SplitWindowExtRequest {
        target: target.clone(),
        direction: SplitDirection::Vertical,
        before: false,
        environment: Some(vec!["FOO=1".to_owned()]),
        command: Some(vec!["printf ready".to_owned()]),
        process_command: None,
        start_directory: Some(PathBuf::from("/tmp/logs")),
        keep_alive_on_exit: Some(true),
        detached: true,
        size: Some("5".to_owned()),
        preserve_zoom: true,
        full_size: true,
        stdin_payload: None,
    };

    let decoded: SplitWindowExtRequest =
        bincode::deserialize(&bincode::serialize(&request).expect("request serializes"))
            .expect("current request decodes");

    assert_eq!(decoded, request);
    assert_eq!(decoded.target, target);
    assert!(decoded.detached);
    assert_eq!(decoded.size.as_deref(), Some("5"));
    assert!(decoded.preserve_zoom);
    assert!(decoded.full_size);
}

#[test]
fn last_pane_request_deserializes_old_payloads_with_defaulted_fields() {
    let target = WindowTarget::with_window(session_name("alpha"), 0);
    let bytes = bincode::serialize(&OldLastPaneRequest {
        target: target.clone(),
    })
    .expect("old last-pane request serializes");

    let decoded: LastPaneRequest =
        bincode::deserialize(&bytes).expect("new request decodes old payload");

    assert_eq!(decoded.target, target);
    assert!(!decoded.preserve_zoom);
    assert_eq!(decoded.input_disabled, None);
}

#[test]
fn select_pane_request_deserializes_old_payloads_with_defaulted_fields() {
    let target = PaneTarget::with_window(session_name("alpha"), 0, 1);
    let bytes = bincode::serialize(&OldSelectPaneRequest {
        target: target.clone(),
        title: Some("logs".to_owned()),
        input_disabled: None,
    })
    .expect("old select-pane request serializes");

    let decoded: SelectPaneRequest =
        bincode::deserialize(&bytes).expect("new request decodes old payload");

    assert_eq!(decoded.target, target);
    assert_eq!(decoded.title.as_deref(), Some("logs"));
    assert_eq!(decoded.input_disabled, None);
    assert!(!decoded.preserve_zoom);
    assert_eq!(decoded.style, None);
}

#[test]
fn select_pane_adjacent_request_deserializes_old_payloads_with_defaulted_fields() {
    let target = PaneTarget::with_window(session_name("alpha"), 0, 1);
    let bytes = bincode::serialize(&OldSelectPaneAdjacentRequest {
        target: target.clone(),
        direction: SelectPaneDirection::Left,
    })
    .expect("old select-pane-adjacent request serializes");

    let decoded: SelectPaneAdjacentRequest =
        bincode::deserialize(&bytes).expect("new request decodes old payload");

    assert_eq!(decoded.target, target);
    assert_eq!(decoded.direction, SelectPaneDirection::Left);
    assert!(!decoded.preserve_zoom);
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
    assert!(!decoded.skip_environment_update);
}

#[test]
fn new_session_ext_request_map_deserialize_respects_defaulted_fields() {
    let decoded: NewSessionExtRequest =
        serde_json::from_str(r#"{"detached":false}"#).expect("sparse map decodes");

    assert_eq!(decoded.session_name, None);
    assert_eq!(decoded.working_directory, None);
    assert!(!decoded.detached);
    assert_eq!(decoded.size, None);
    assert_eq!(decoded.environment, None);
    assert_eq!(decoded.group_target, None);
    assert!(!decoded.attach_if_exists);
    assert!(!decoded.detach_other_clients);
    assert!(!decoded.kill_other_clients);
    assert_eq!(decoded.flags, None);
    assert_eq!(decoded.window_name, None);
    assert!(!decoded.print_session_info);
    assert_eq!(decoded.print_format, None);
    assert_eq!(decoded.command, None);
    assert_eq!(decoded.process_command, None);
    assert_eq!(decoded.client_environment, None);
    assert!(!decoded.skip_environment_update);

    let error = serde_json::from_str::<NewSessionExtRequest>("{}")
        .expect_err("detached remains required for sparse maps");
    assert!(
        error.to_string().contains("missing field `detached`"),
        "unexpected error: {error}"
    );
}

#[test]
fn request_map_deserialize_respects_option_and_default_fields() {
    let session = session_name("alpha");
    let window_target = WindowTarget::with_window(session.clone(), 1);
    let pane_target = PaneTarget::with_window(session.clone(), 1, 2);
    let split_target = SplitWindowTarget::Pane(pane_target.clone());

    let new_window: NewWindowRequest =
        serde_json::from_str(&format!(r#"{{"target":"{}","detached":false}}"#, session))
            .expect("sparse new-window map decodes");
    assert_eq!(new_window.name, None);
    assert_eq!(new_window.environment, None);
    assert_eq!(new_window.command, None);
    assert_eq!(new_window.start_directory, None);
    assert_eq!(new_window.target_window_index, None);
    assert!(!new_window.insert_at_target);
    assert_eq!(new_window.process_command, None);

    let respawn_window: RespawnWindowRequest = serde_json::from_value(serde_json::json!({
        "target": window_target,
    }))
    .expect("sparse respawn-window map decodes");
    assert!(!respawn_window.kill);
    assert_eq!(respawn_window.environment, None);
    assert_eq!(respawn_window.command, None);
    assert_eq!(respawn_window.start_directory, None);

    let move_window: MoveWindowRequest = serde_json::from_value(serde_json::json!({
        "target": MoveWindowTarget::Session(session.clone()),
        "renumber": true,
        "kill_destination": false,
        "detached": false,
    }))
    .expect("sparse move-window map decodes");
    assert_eq!(move_window.source, None);
    assert!(!move_window.after);
    assert!(!move_window.before);

    let display: DisplayMessageRequest =
        serde_json::from_str(r#"{"print":true}"#).expect("sparse display-message map decodes");
    assert_eq!(display.target, None);
    assert_eq!(display.message, None);
    assert!(!display.empty_target_context);

    let display_ext: DisplayMessageExtRequest =
        serde_json::from_str(r#"{"print":false}"#).expect("sparse display-message-ext map decodes");
    assert_eq!(display_ext.target, None);
    assert_eq!(display_ext.message, None);
    assert_eq!(display_ext.target_client, None);
    assert!(!display_ext.empty_target_context);

    let set_option: SetOptionByNameRequest = serde_json::from_value(serde_json::json!({
        "scope": OptionScopeSelector::SessionGlobal,
        "name": "@probe",
        "mode": SetOptionMode::Replace,
        "only_if_unset": false,
        "unset": false,
        "unset_pane_overrides": false,
    }))
    .expect("sparse set-option-by-name map decodes");
    assert_eq!(set_option.value, None);
    assert!(!set_option.format);
    assert_eq!(set_option.format_target, None);

    let split: SplitWindowExtRequest = serde_json::from_value(serde_json::json!({
        "target": split_target,
        "direction": SplitDirection::Horizontal,
    }))
    .expect("sparse split-window-ext map decodes");
    assert!(!split.before);
    assert_eq!(split.environment, None);
    assert_eq!(split.command, None);
    assert_eq!(split.process_command, None);
    assert_eq!(split.start_directory, None);
    assert_eq!(split.keep_alive_on_exit, None);
    assert!(!split.detached);
    assert_eq!(split.size, None);
    assert!(!split.preserve_zoom);
    assert!(!split.full_size);
    assert_eq!(split.stdin_payload, None);

    let respawn_pane: RespawnPaneRequest = serde_json::from_value(serde_json::json!({
        "target": pane_target,
    }))
    .expect("sparse respawn-pane map decodes");
    assert!(!respawn_pane.kill);
    assert_eq!(respawn_pane.start_directory, None);
    assert_eq!(respawn_pane.environment, None);
    assert_eq!(respawn_pane.command, None);
    assert_eq!(respawn_pane.process_command, None);
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
    assert!(!decoded.quiet);
    assert!(!decoded.include_hooks);
}

#[test]
fn set_option_by_name_request_deserializes_old_payloads_with_defaulted_fields() {
    let scope = OptionScopeSelector::SessionGlobal;
    let bytes = bincode::serialize(&OldSetOptionByNameRequest {
        scope: scope.clone(),
        name: "@probe".to_owned(),
        value: Some("#{session_name}".to_owned()),
        mode: SetOptionMode::Replace,
        only_if_unset: false,
        unset: false,
        unset_pane_overrides: false,
    })
    .expect("old set-option-by-name request serializes");

    let decoded: SetOptionByNameRequest =
        bincode::deserialize(&bytes).expect("new request decodes old payload");

    assert_eq!(decoded.scope, scope);
    assert_eq!(decoded.name, "@probe");
    assert_eq!(decoded.value.as_deref(), Some("#{session_name}"));
    assert_eq!(decoded.mode, SetOptionMode::Replace);
    assert!(!decoded.only_if_unset);
    assert!(!decoded.unset);
    assert!(!decoded.unset_pane_overrides);
    assert!(!decoded.format);
    assert_eq!(decoded.format_target, None);
}

#[test]
fn move_window_request_deserializes_old_payloads_with_defaulted_fields() {
    let source = WindowTarget::with_window(session_name("alpha"), 0);
    let target = WindowTarget::with_window(session_name("alpha"), 1);
    let bytes = bincode::serialize(&OldMoveWindowRequest {
        source: Some(source.clone()),
        target: MoveWindowTarget::Window(target.clone()),
        renumber: false,
        kill_destination: true,
        detached: false,
    })
    .expect("old move-window request serializes");

    let decoded: MoveWindowRequest =
        bincode::deserialize(&bytes).expect("new request decodes old payload");

    assert_eq!(decoded.source, Some(source));
    assert_eq!(decoded.target, MoveWindowTarget::Window(target));
    assert!(!decoded.renumber);
    assert!(decoded.kill_destination);
    assert!(!decoded.detached);
    assert!(!decoded.after);
    assert!(!decoded.before);
}
