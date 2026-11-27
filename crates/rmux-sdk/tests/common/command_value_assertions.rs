//! Compare supported SDK values with the independently written, pre-cutoff
//! request expectations. This is a value/serde migration check, not a transport
//! mapper: no request is constructed from an SDK value here. Fields formerly
//! filled with constants have no SDK counterpart; keep their historical checks.
use rmux_sdk::*;

pub fn assert_command_value(actual: impl Into<RmuxCommandKind>, expected: rmux_proto::Request) {
    let actual = actual.into();
    let binary = bincode::serialize(&actual).unwrap();
    assert_eq!(
        bincode::deserialize::<RmuxCommandKind>(&binary).unwrap(),
        actual
    );
    let json = serde_json::to_string(&actual).unwrap();
    assert_eq!(
        serde_json::from_str::<RmuxCommandKind>(&json).unwrap(),
        actual
    );
    match (actual, expected) {
        (RmuxCommandKind::NewSession(s), rmux_proto::Request::NewSessionExt(p)) => {
            assert_eq!(s.session_name, p.session_name);
            assert_eq!(s.working_directory, p.working_directory);
            assert_eq!(s.detached, p.detached);
            assert_eq!(
                serde_json::to_value(s.size).unwrap(),
                serde_json::to_value(p.size).unwrap()
            );
            assert_process(s.process, p.command, p.process_command, p.environment);
            assert_eq!(s.group_target, p.group_target);
            assert_eq!(s.reuse.attach_if_exists, p.attach_if_exists);
            assert_eq!(s.reuse.detach_other_clients, p.detach_other_clients);
            assert_eq!(s.reuse.kill_other_clients, p.kill_other_clients);
            assert_eq!(s.reuse.skip_environment_update, p.skip_environment_update);
            assert_eq!(s.reuse.flags, p.flags);
            assert_eq!(s.window_name, p.window_name);
            assert_eq!(s.print_session_info, p.print_session_info);
            assert_eq!(s.print_format, p.print_format);
            assert_eq!(p.client_environment, None);
        }
        (RmuxCommandKind::AttachSession(s), rmux_proto::Request::AttachSessionExt2(p)) => {
            assert_eq!(s.target, p.target);
            assert_eq!(s.target_spec, p.target_spec);
            assert_eq!(s.reuse.detach_other_clients, p.detach_other_clients);
            assert_eq!(s.reuse.kill_other_clients, p.kill_other_clients);
            assert_eq!(s.reuse.read_only, p.read_only);
            assert_eq!(s.reuse.skip_environment_update, p.skip_environment_update);
            assert_eq!(s.reuse.flags, p.flags);
            assert_eq!(s.working_directory, p.working_directory);
            assert_eq!(
                s.client_terminal.terminal_features,
                p.client_terminal.terminal_features
            );
            assert_eq!(s.client_terminal.utf8, p.client_terminal.utf8);
            assert_eq!(
                serde_json::to_value(s.client_size).unwrap(),
                serde_json::to_value(p.client_size).unwrap()
            );
        }
        (RmuxCommandKind::SplitWindow(s), rmux_proto::Request::SplitWindowExt(p)) => {
            assert_eq!(
                serde_json::to_value(s.target).unwrap(),
                serde_json::to_value(p.target).unwrap()
            );
            assert_eq!(
                serde_json::to_value(s.direction).unwrap(),
                serde_json::to_value(p.direction).unwrap()
            );
            assert_eq!(s.before, p.before);
            assert_process(s.process, p.command, p.process_command, p.environment);
            assert_eq!(p.start_directory, None);
            assert_eq!(p.keep_alive_on_exit, None);
            assert!(!p.detached);
            assert_eq!(p.size, None);
            assert!(!p.preserve_zoom);
            assert!(!p.full_size);
            assert_eq!(p.stdin_payload, None);
        }
        (RmuxCommandKind::RefreshClient(s), rmux_proto::Request::RefreshClient(p)) => {
            assert_eq!(s.target_client, p.target_client);
            assert_eq!(s.adjustment, p.adjustment);
            assert_eq!(s.clear_pan, p.clear_pan);
            assert_eq!(s.pan_left, p.pan_left);
            assert_eq!(s.pan_right, p.pan_right);
            assert_eq!(s.pan_up, p.pan_up);
            assert_eq!(s.pan_down, p.pan_down);
            assert_eq!(s.status_only, p.status_only);
            assert_eq!(s.clipboard_query, p.clipboard_query);
            assert_eq!(s.flags, p.flags);
            assert_eq!(s.flags_alias, p.flags_alias);
            assert_eq!(s.subscriptions.subscriptions, p.subscriptions);
            assert_eq!(s.subscriptions.subscriptions_format, p.subscriptions_format);
            assert_eq!(s.control_size, p.control_size);
            assert_eq!(s.colour_report, p.colour_report);
        }
        pair => panic!("mismatched SDK value / historical request: {pair:?}"),
    }
}

fn assert_process(
    s: ProcessSpec,
    command: Option<Vec<String>>,
    mode: Option<rmux_proto::ProcessCommand>,
    environment: Option<Vec<String>>,
) {
    assert_eq!(s.command, command);
    assert_eq!(
        serde_json::to_value(s.process_command).unwrap(),
        serde_json::to_value(mode).unwrap()
    );
    assert_eq!(s.environment, environment);
}
