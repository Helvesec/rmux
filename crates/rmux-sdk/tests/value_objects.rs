use std::fmt::Debug;
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::json;

use rmux_sdk::{
    AttachSessionReuse, AttachSessionSpec, ClientTerminalSpec, NewSessionReuse, NewSessionSpec,
    PaneRef, ProcessSpec, RefreshClientSpec, RmuxCommand, RmuxCommandKind, RmuxEndpoint,
    SessionName, SplitDirectionSpec, SplitSpec, SplitTargetSpec, SubscriptionSpec, TargetRef,
    TerminalSizeSpec, WindowRef,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn round_trip<T>(value: T) -> T
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let bytes = bincode::serialize(&value).expect("value serializes");
    let decoded = bincode::deserialize::<T>(&bytes).expect("value deserializes");
    assert_eq!(decoded, value);

    let json = serde_json::to_string(&value).expect("value serializes as JSON");
    let decoded_json = serde_json::from_str::<T>(&json).expect("value deserializes from JSON");
    assert_eq!(decoded_json, value);

    decoded
}

#[test]
fn endpoint_value_objects_round_trip() {
    round_trip(RmuxEndpoint::Default);
    round_trip(RmuxEndpoint::UnixSocket(PathBuf::from("/tmp/rmux.sock")));
    round_trip(RmuxEndpoint::WindowsPipe("rmux-pipe".to_owned()));
}

#[test]
fn process_subscription_reuse_and_terminal_specs_round_trip() {
    round_trip(ProcessSpec {
        command: Some(vec!["printf".to_owned(), "ok".to_owned()]),
        process_command: None,
        environment: Some(vec!["A=B".to_owned()]),
    });
    round_trip(SubscriptionSpec {
        subscriptions: vec!["pane-output:%1".to_owned()],
        subscriptions_format: vec!["client:#{client_name}".to_owned()],
    });
    round_trip(NewSessionReuse {
        attach_if_exists: true,
        detach_other_clients: false,
        kill_other_clients: true,
        skip_environment_update: false,
        flags: Some(vec!["active-pane".to_owned()]),
    });
    round_trip(AttachSessionReuse {
        detach_other_clients: true,
        kill_other_clients: false,
        read_only: true,
        skip_environment_update: true,
        flags: Some(vec!["read-only".to_owned()]),
    });
    round_trip(ClientTerminalSpec {
        terminal_features: vec!["RGB".to_owned()],
        utf8: true,
    });
}

#[test]
fn structured_targets_round_trip_through_proto_without_parsing() {
    let window = round_trip(WindowRef::new(session_name("alpha"), 2));
    let window_target =
        rmux_proto::WindowTarget::with_window(window.session_name.clone(), window.window_index);
    assert_eq!(
        WindowRef::new(
            window_target.session_name().clone(),
            window_target.window_index()
        ),
        window
    );

    let pane = round_trip(PaneRef::new(session_name("alpha"), 2, 3));
    let pane_target = rmux_proto::PaneTarget::with_window(
        pane.session_name.clone(),
        pane.window_index,
        pane.pane_index,
    );
    assert_eq!(
        PaneRef::new(
            pane_target.session_name().clone(),
            pane_target.window_index(),
            pane_target.pane_index()
        ),
        pane
    );

    let target = round_trip(TargetRef::Pane(PaneRef::new(session_name("alpha"), 4, 5)));
    let proto_target = rmux_proto::Target::Pane(rmux_proto::PaneTarget::with_window(
        session_name("alpha"),
        4,
        5,
    ));
    assert_eq!(
        bincode::serialize(&proto_target).unwrap(),
        bincode::serialize(&target).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&proto_target).unwrap(),
        serde_json::to_value(&target).unwrap()
    );

    let split_target = round_trip(SplitTargetSpec::Pane(PaneRef::new(
        session_name("alpha"),
        6,
        7,
    )));
    let proto_split_target = rmux_proto::SplitWindowTarget::Pane(
        rmux_proto::PaneTarget::with_window(session_name("alpha"), 6, 7),
    );
    assert_eq!(
        bincode::serialize(&proto_split_target).unwrap(),
        bincode::serialize(&split_target).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&proto_split_target).unwrap(),
        serde_json::to_value(&split_target).unwrap()
    );
}

#[test]
fn new_session_spec_round_trip_preserves_reuse_flags() {
    let spec = round_trip(NewSessionSpec {
        session_name: Some(session_name("alpha")),
        working_directory: Some("#{pane_current_path}".to_owned()),
        detached: true,
        size: Some(TerminalSizeSpec::new(100, 30)),
        process: ProcessSpec {
            command: Some(vec!["top".to_owned()]),
            process_command: None,
            environment: Some(vec!["TERM=xterm-256color".to_owned()]),
        },
        group_target: Some(session_name("group")),
        reuse: NewSessionReuse {
            attach_if_exists: true,
            detach_other_clients: true,
            kill_other_clients: false,
            skip_environment_update: false,
            flags: Some(vec!["active-pane".to_owned()]),
        },
        window_name: Some("main".to_owned()),
        print_session_info: true,
        print_format: Some("#{session_name}".to_owned()),
    });

    let value = spec;
    assert_eq!(value.session_name, Some(session_name("alpha")));
    assert_eq!(
        value.working_directory.as_deref(),
        Some("#{pane_current_path}")
    );
    assert!(value.detached);
    assert_eq!(
        value.size,
        Some(TerminalSizeSpec {
            cols: 100,
            rows: 30
        })
    );
    assert_eq!(
        value.process.environment,
        Some(vec!["TERM=xterm-256color".to_owned()])
    );
    assert_eq!(value.group_target, Some(session_name("group")));
    assert!(value.reuse.attach_if_exists);
    assert!(value.reuse.detach_other_clients);
    assert!(!value.reuse.kill_other_clients);
    assert_eq!(value.reuse.flags, Some(vec!["active-pane".to_owned()]));
    assert_eq!(value.process.command, Some(vec!["top".to_owned()]));
}

#[test]
fn attach_session_spec_round_trip_preserves_reuse_flags() {
    let spec = round_trip(AttachSessionSpec {
        target: Some(session_name("alpha")),
        target_spec: Some("alpha:1.2".to_owned()),
        reuse: AttachSessionReuse {
            detach_other_clients: true,
            kill_other_clients: true,
            read_only: true,
            skip_environment_update: true,
            flags: Some(vec!["read-only".to_owned()]),
        },
        working_directory: Some("/work".to_owned()),
        client_terminal: ClientTerminalSpec {
            terminal_features: vec!["RGB".to_owned()],
            utf8: true,
        },
        client_size: Some(TerminalSizeSpec::new(120, 40)),
    });

    let value = spec;
    assert_eq!(value.target, Some(session_name("alpha")));
    assert_eq!(value.target_spec.as_deref(), Some("alpha:1.2"));
    assert!(value.reuse.detach_other_clients);
    assert!(value.reuse.kill_other_clients);
    assert!(value.reuse.read_only);
    assert!(value.reuse.skip_environment_update);
    assert_eq!(value.reuse.flags, Some(vec!["read-only".to_owned()]));
    assert_eq!(value.working_directory.as_deref(), Some("/work"));
    assert_eq!(
        value.client_terminal.terminal_features,
        vec!["RGB".to_owned()]
    );
    assert!(value.client_terminal.utf8);
    assert_eq!(
        value.client_size,
        Some(TerminalSizeSpec {
            cols: 120,
            rows: 40
        })
    );
}

#[test]
fn refresh_client_spec_round_trip_preserves_subscriptions() {
    let spec = round_trip(RefreshClientSpec {
        target_client: Some("client-1".to_owned()),
        adjustment: Some(5),
        clear_pan: true,
        pan_left: false,
        pan_right: true,
        pan_up: false,
        pan_down: true,
        status_only: true,
        clipboard_query: true,
        flags: Some("read-only".to_owned()),
        flags_alias: Some("active-pane".to_owned()),
        subscriptions: SubscriptionSpec {
            subscriptions: vec!["pane-output:%1".to_owned()],
            subscriptions_format: vec!["client:#{client_name}".to_owned()],
        },
        control_size: Some("80x24".to_owned()),
        colour_report: Some("fg".to_owned()),
    });

    let value = spec;
    assert_eq!(value.target_client.as_deref(), Some("client-1"));
    assert_eq!(value.adjustment, Some(5));
    assert!(value.clear_pan);
    assert!(!value.pan_left);
    assert!(value.pan_right);
    assert_eq!(
        value.subscriptions.subscriptions,
        vec!["pane-output:%1".to_owned()]
    );
    assert_eq!(
        value.subscriptions.subscriptions_format,
        vec!["client:#{client_name}".to_owned()]
    );
    assert_eq!(value.control_size.as_deref(), Some("80x24"));
    assert_eq!(value.colour_report.as_deref(), Some("fg"));
}

#[test]
fn split_spec_round_trip_preserves_direction_and_target() {
    let spec = round_trip(SplitSpec {
        target: SplitTargetSpec::Pane(PaneRef::new(session_name("alpha"), 2, 3)),
        direction: SplitDirectionSpec::Horizontal,
        before: false,
        process: ProcessSpec {
            command: Some(vec!["htop".to_owned()]),
            process_command: None,
            environment: Some(vec!["A=B".to_owned()]),
        },
    });

    let value = spec;
    assert_eq!(value.direction, SplitDirectionSpec::Horizontal);
    assert_eq!(
        value.target,
        SplitTargetSpec::Pane(PaneRef::new(session_name("alpha"), 2, 3,))
    );
    assert_eq!(value.process.command, Some(vec!["htop".to_owned()]));
    assert_eq!(value.process.environment, Some(vec!["A=B".to_owned()]));
}

#[test]
fn sparse_serde_payloads_preserve_sdk_defaults() {
    let new_session =
        serde_json::from_value::<NewSessionSpec>(json!({})).expect("sparse new-session DTO");
    assert_eq!(new_session, NewSessionSpec::default());
    let value = round_trip(new_session);
    assert_eq!(value.session_name, None);
    assert_eq!(value.working_directory, None);
    assert!(!value.detached);
    assert_eq!(value.size, None);
    assert_eq!(value.process.environment, None);
    assert_eq!(value.group_target, None);
    assert!(!value.reuse.attach_if_exists);
    assert!(!value.reuse.detach_other_clients);
    assert!(!value.reuse.kill_other_clients);
    assert_eq!(value.reuse.flags, None);
    assert_eq!(value.process.command, None);

    let attach = serde_json::from_value::<AttachSessionSpec>(json!({})).expect("sparse attach DTO");
    assert_eq!(attach, AttachSessionSpec::default());
    let value = round_trip(attach);
    assert_eq!(value.target, None);
    assert_eq!(value.target_spec, None);
    assert!(!value.reuse.detach_other_clients);
    assert!(!value.reuse.kill_other_clients);
    assert!(!value.reuse.read_only);
    assert!(!value.reuse.skip_environment_update);
    assert_eq!(value.reuse.flags, None);
    assert_eq!(value.working_directory, None);
    assert_eq!(value.client_terminal, ClientTerminalSpec::default());
    assert_eq!(value.client_size, None);

    let refresh =
        serde_json::from_value::<RefreshClientSpec>(json!({})).expect("sparse refresh DTO");
    assert_eq!(refresh, RefreshClientSpec::default());
    let value = round_trip(refresh);
    assert_eq!(value.target_client, None);
    assert_eq!(value.adjustment, None);
    assert!(!value.clear_pan);
    assert!(!value.pan_left);
    assert!(!value.pan_right);
    assert!(!value.pan_up);
    assert!(!value.pan_down);
    assert!(!value.status_only);
    assert!(!value.clipboard_query);
    assert_eq!(value.flags, None);
    assert_eq!(value.flags_alias, None);
    assert_eq!(value.subscriptions.subscriptions, Vec::<String>::new());
    assert_eq!(
        value.subscriptions.subscriptions_format,
        Vec::<String>::new()
    );
    assert_eq!(value.control_size, None);
    assert_eq!(value.colour_report, None);

    let split = serde_json::from_value::<SplitSpec>(json!({
        "target": { "Session": "alpha" }
    }))
    .expect("sparse split DTO");
    assert_eq!(split.direction, SplitDirectionSpec::Vertical);
    assert_eq!(split.process, ProcessSpec::default());
    let value = round_trip(split);
    assert_eq!(value.direction, SplitDirectionSpec::Vertical);
    assert_eq!(
        value.target,
        SplitTargetSpec::Session(session_name("alpha"))
    );
    assert_eq!(value.process.environment, None);
    assert_eq!(value.process.command, None);

    let command = serde_json::from_value::<RmuxCommand>(json!({
        "command": { "RefreshClient": {} }
    }))
    .expect("sparse command DTO");
    assert_eq!(command.endpoint, RmuxEndpoint::Default);
    assert!(matches!(
        command.command,
        RmuxCommandKind::RefreshClient(value)
            if value.target_client.is_none()
                && value.adjustment.is_none()
                && !value.clear_pan
                && !value.pan_left
                && !value.pan_right
                && !value.pan_up
                && !value.pan_down
                && !value.status_only
                && !value.clipboard_query
                && value.flags.is_none()
                && value.flags_alias.is_none()
                && value.subscriptions.subscriptions.is_empty()
                && value.subscriptions.subscriptions_format.is_empty()
                && value.control_size.is_none()
                && value.colour_report.is_none()
    ));
}

#[test]
fn explicit_empty_options_are_not_normalized_away_in_sdk_values() {
    let new_session = NewSessionSpec {
        process: ProcessSpec {
            command: Some(Vec::new()),
            process_command: None,
            environment: Some(Vec::new()),
        },
        reuse: NewSessionReuse {
            flags: Some(Vec::new()),
            ..NewSessionReuse::default()
        },
        ..NewSessionSpec::default()
    };
    let value = round_trip(new_session);
    assert_eq!(value.process.environment, Some(Vec::<String>::new()));
    assert_eq!(value.reuse.flags, Some(Vec::<String>::new()));
    assert_eq!(value.process.command, Some(Vec::<String>::new()));

    let attach = AttachSessionSpec {
        target_spec: Some(String::new()),
        reuse: AttachSessionReuse {
            flags: Some(Vec::new()),
            ..AttachSessionReuse::default()
        },
        working_directory: Some(String::new()),
        ..AttachSessionSpec::default()
    };
    let value = round_trip(attach);
    assert_eq!(value.target_spec, Some(String::new()));
    assert_eq!(value.reuse.flags, Some(Vec::<String>::new()));
    assert_eq!(value.working_directory, Some(String::new()));

    let refresh = RefreshClientSpec {
        flags: Some(String::new()),
        flags_alias: Some(String::new()),
        control_size: Some(String::new()),
        colour_report: Some(String::new()),
        ..RefreshClientSpec::default()
    };
    let value = round_trip(refresh);
    assert_eq!(value.flags, Some(String::new()));
    assert_eq!(value.flags_alias, Some(String::new()));
    assert_eq!(value.control_size, Some(String::new()));
    assert_eq!(value.colour_report, Some(String::new()));

    let split = SplitSpec {
        target: SplitTargetSpec::Session(session_name("alpha")),
        direction: SplitDirectionSpec::Vertical,
        before: false,
        process: ProcessSpec {
            command: Some(Vec::new()),
            process_command: None,
            environment: Some(Vec::new()),
        },
    };
    let value = round_trip(split);
    assert_eq!(value.process.environment, Some(Vec::<String>::new()));
    assert_eq!(value.process.command, Some(Vec::<String>::new()));
}

#[test]
fn command_value_object_round_trip_preserves_payload() {
    let command = round_trip(RmuxCommand::with_endpoint(
        RmuxEndpoint::UnixSocket(PathBuf::from("/tmp/rmux.sock")),
        RmuxCommandKind::SplitWindow(SplitSpec {
            target: SplitTargetSpec::Session(session_name("alpha")),
            direction: SplitDirectionSpec::Vertical,
            before: false,
            process: ProcessSpec::default(),
        }),
    ));

    assert_eq!(command.command_name(), "split-window");

    let value = command.command;
    assert!(matches!(
        value,
        RmuxCommandKind::SplitWindow(value)
            if matches!(value.target, SplitTargetSpec::Session(_))
                && value.direction == SplitDirectionSpec::Vertical
                && !value.before
                && value.process.environment.is_none()
                && value.process.command.is_none()
                && value.process.process_command.is_none()
    ));
}
