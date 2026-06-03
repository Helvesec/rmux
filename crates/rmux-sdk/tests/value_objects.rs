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
    let window_target = rmux_proto::WindowTarget::from(window.clone());
    assert_eq!(WindowRef::from(window_target), window);

    let pane = round_trip(PaneRef::new(session_name("alpha"), 2, 3));
    let pane_target = rmux_proto::PaneTarget::from(pane.clone());
    assert_eq!(PaneRef::from(pane_target), pane);

    let target = round_trip(TargetRef::Pane(PaneRef::new(session_name("alpha"), 4, 5)));
    let proto_target = rmux_proto::Target::from(target.clone());
    assert_eq!(TargetRef::from(proto_target), target);

    let split_target = round_trip(SplitTargetSpec::Pane(PaneRef::new(
        session_name("alpha"),
        6,
        7,
    )));
    let proto_split_target = rmux_proto::SplitWindowTarget::from(split_target.clone());
    assert_eq!(SplitTargetSpec::from(proto_split_target), split_target);
}

#[test]
fn new_session_spec_round_trip_and_maps_reuse_flags() {
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
            flags: Some(vec!["active-pane".to_owned()]),
        },
        window_name: Some("main".to_owned()),
        print_session_info: true,
        print_format: Some("#{session_name}".to_owned()),
    });

    let request = rmux_proto::NewSessionExtRequest::from(spec);
    assert_eq!(request.session_name, Some(session_name("alpha")));
    assert_eq!(
        request.working_directory.as_deref(),
        Some("#{pane_current_path}")
    );
    assert!(request.detached);
    assert_eq!(
        request.size,
        Some(rmux_proto::TerminalSize {
            cols: 100,
            rows: 30
        })
    );
    assert_eq!(
        request.environment,
        Some(vec!["TERM=xterm-256color".to_owned()])
    );
    assert_eq!(request.group_target, Some(session_name("group")));
    assert!(request.attach_if_exists);
    assert!(request.detach_other_clients);
    assert!(!request.kill_other_clients);
    assert_eq!(request.flags, Some(vec!["active-pane".to_owned()]));
    assert_eq!(request.command, Some(vec!["top".to_owned()]));
}

#[test]
fn attach_session_spec_round_trip_and_maps_reuse_flags() {
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

    let request = rmux_proto::AttachSessionExt2Request::from(spec);
    assert_eq!(request.target, Some(session_name("alpha")));
    assert_eq!(request.target_spec.as_deref(), Some("alpha:1.2"));
    assert!(request.detach_other_clients);
    assert!(request.kill_other_clients);
    assert!(request.read_only);
    assert!(request.skip_environment_update);
    assert_eq!(request.flags, Some(vec!["read-only".to_owned()]));
    assert_eq!(request.working_directory.as_deref(), Some("/work"));
    assert_eq!(
        request.client_terminal.terminal_features,
        vec!["RGB".to_owned()]
    );
    assert!(request.client_terminal.utf8);
    assert_eq!(
        request.client_size,
        Some(rmux_proto::TerminalSize {
            cols: 120,
            rows: 40
        })
    );
}

#[test]
fn refresh_client_spec_round_trip_and_maps_subscriptions() {
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

    let request = rmux_proto::RefreshClientRequest::from(spec);
    assert_eq!(request.target_client.as_deref(), Some("client-1"));
    assert_eq!(request.adjustment, Some(5));
    assert!(request.clear_pan);
    assert!(!request.pan_left);
    assert!(request.pan_right);
    assert_eq!(request.subscriptions, vec!["pane-output:%1".to_owned()]);
    assert_eq!(
        request.subscriptions_format,
        vec!["client:#{client_name}".to_owned()]
    );
    assert_eq!(request.control_size.as_deref(), Some("80x24"));
    assert_eq!(request.colour_report.as_deref(), Some("fg"));
}

#[test]
fn split_spec_round_trip_and_maps_direction_and_target() {
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

    let request = rmux_proto::SplitWindowExtRequest::from(spec);
    assert_eq!(request.direction, rmux_proto::SplitDirection::Horizontal);
    assert_eq!(
        request.target,
        rmux_proto::SplitWindowTarget::Pane(rmux_proto::PaneTarget::with_window(
            session_name("alpha"),
            2,
            3,
        ))
    );
    assert_eq!(request.command, Some(vec!["htop".to_owned()]));
    assert_eq!(request.environment, Some(vec!["A=B".to_owned()]));
}

#[test]
fn sparse_serde_payloads_preserve_proto_defaults() {
    let new_session =
        serde_json::from_value::<NewSessionSpec>(json!({})).expect("sparse new-session DTO");
    assert_eq!(new_session, NewSessionSpec::default());
    let request = rmux_proto::NewSessionExtRequest::from(new_session);
    assert_eq!(request.session_name, None);
    assert_eq!(request.working_directory, None);
    assert!(!request.detached);
    assert_eq!(request.size, None);
    assert_eq!(request.environment, None);
    assert_eq!(request.group_target, None);
    assert!(!request.attach_if_exists);
    assert!(!request.detach_other_clients);
    assert!(!request.kill_other_clients);
    assert_eq!(request.flags, None);
    assert_eq!(request.command, None);

    let attach = serde_json::from_value::<AttachSessionSpec>(json!({})).expect("sparse attach DTO");
    assert_eq!(attach, AttachSessionSpec::default());
    let request = rmux_proto::AttachSessionExt2Request::from(attach);
    assert_eq!(request.target, None);
    assert_eq!(request.target_spec, None);
    assert!(!request.detach_other_clients);
    assert!(!request.kill_other_clients);
    assert!(!request.read_only);
    assert!(!request.skip_environment_update);
    assert_eq!(request.flags, None);
    assert_eq!(request.working_directory, None);
    assert_eq!(
        request.client_terminal,
        rmux_proto::ClientTerminalContext::default()
    );
    assert_eq!(request.client_size, None);

    let refresh =
        serde_json::from_value::<RefreshClientSpec>(json!({})).expect("sparse refresh DTO");
    assert_eq!(refresh, RefreshClientSpec::default());
    let request = rmux_proto::RefreshClientRequest::from(refresh);
    assert_eq!(request.target_client, None);
    assert_eq!(request.adjustment, None);
    assert!(!request.clear_pan);
    assert!(!request.pan_left);
    assert!(!request.pan_right);
    assert!(!request.pan_up);
    assert!(!request.pan_down);
    assert!(!request.status_only);
    assert!(!request.clipboard_query);
    assert_eq!(request.flags, None);
    assert_eq!(request.flags_alias, None);
    assert_eq!(request.subscriptions, Vec::<String>::new());
    assert_eq!(request.subscriptions_format, Vec::<String>::new());
    assert_eq!(request.control_size, None);
    assert_eq!(request.colour_report, None);

    let split = serde_json::from_value::<SplitSpec>(json!({
        "target": { "Session": "alpha" }
    }))
    .expect("sparse split DTO");
    assert_eq!(split.direction, SplitDirectionSpec::Vertical);
    assert_eq!(split.process, ProcessSpec::default());
    let request = rmux_proto::SplitWindowExtRequest::from(split);
    assert_eq!(request.direction, rmux_proto::SplitDirection::Vertical);
    assert_eq!(
        request.target,
        rmux_proto::SplitWindowTarget::Session(session_name("alpha"))
    );
    assert_eq!(request.environment, None);
    assert_eq!(request.command, None);

    let command = serde_json::from_value::<RmuxCommand>(json!({
        "command": { "RefreshClient": {} }
    }))
    .expect("sparse command DTO");
    assert_eq!(command.endpoint, RmuxEndpoint::Default);
    assert!(matches!(
        command.into_request(),
        rmux_proto::Request::RefreshClient(rmux_proto::RefreshClientRequest {
            target_client: None,
            adjustment: None,
            clear_pan: false,
            pan_left: false,
            pan_right: false,
            pan_up: false,
            pan_down: false,
            status_only: false,
            clipboard_query: false,
            flags: None,
            flags_alias: None,
            subscriptions,
            subscriptions_format,
            control_size: None,
            colour_report: None,
        }) if subscriptions.is_empty() && subscriptions_format.is_empty()
    ));
}

#[test]
fn explicit_empty_options_are_not_normalized_away_in_proto_mappings() {
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
    let request = rmux_proto::NewSessionExtRequest::from(new_session);
    assert_eq!(request.environment, Some(Vec::<String>::new()));
    assert_eq!(request.flags, Some(Vec::<String>::new()));
    assert_eq!(request.command, Some(Vec::<String>::new()));

    let attach = AttachSessionSpec {
        target_spec: Some(String::new()),
        reuse: AttachSessionReuse {
            flags: Some(Vec::new()),
            ..AttachSessionReuse::default()
        },
        working_directory: Some(String::new()),
        ..AttachSessionSpec::default()
    };
    let request = rmux_proto::AttachSessionExt2Request::from(attach);
    assert_eq!(request.target_spec, Some(String::new()));
    assert_eq!(request.flags, Some(Vec::<String>::new()));
    assert_eq!(request.working_directory, Some(String::new()));

    let refresh = RefreshClientSpec {
        flags: Some(String::new()),
        flags_alias: Some(String::new()),
        control_size: Some(String::new()),
        colour_report: Some(String::new()),
        ..RefreshClientSpec::default()
    };
    let request = rmux_proto::RefreshClientRequest::from(refresh);
    assert_eq!(request.flags, Some(String::new()));
    assert_eq!(request.flags_alias, Some(String::new()));
    assert_eq!(request.control_size, Some(String::new()));
    assert_eq!(request.colour_report, Some(String::new()));

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
    let request = rmux_proto::SplitWindowExtRequest::from(split);
    assert_eq!(request.environment, Some(Vec::<String>::new()));
    assert_eq!(request.command, Some(Vec::<String>::new()));
}

#[test]
fn command_value_object_round_trip_and_materializes_proto_request() {
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

    let request = command.into_request();
    assert!(matches!(
        request,
        rmux_proto::Request::SplitWindowExt(rmux_proto::SplitWindowExtRequest {
            target: rmux_proto::SplitWindowTarget::Session(_),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
            command: None,
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: None,
            detached: false,
            size: None,
            preserve_zoom: false,
        })
    ));
}
