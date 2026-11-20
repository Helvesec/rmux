//! Expectations come from d9cbeb3830's field copies and unchanged protocol DTOs.
//! The retired mappings are now checked as SDK value/serde correspondences.
//! Runtime request assertions live in spec/request_tests.rs, unchanged.
use rmux_sdk::*;

#[path = "common/command_value_assertions.rs"]
mod command_value_assertions;
use command_value_assertions::assert_command_value;

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn name() -> SessionName {
    // Preserve already-valid escapes as well as unusual literal characters.
    serde_json::from_str(r#"" leading $@%[];=/é雪 line\\n ""#).unwrap()
}

fn modes() -> Vec<(
    Option<ProcessCommandSpec>,
    Option<rmux_proto::ProcessCommand>,
)> {
    use rmux_proto::ProcessCommand as Wire;
    vec![
        (None, None),
        (
            Some(ProcessCommandSpec::Argv(vec![])),
            Some(Wire::Argv(vec![])),
        ),
        (
            Some(ProcessCommandSpec::Argv(strings(&[""]))),
            Some(Wire::Argv(strings(&[""]))),
        ),
        (
            Some(ProcessCommandSpec::Argv(strings(&["one $literal"]))),
            Some(Wire::Argv(strings(&["one $literal"]))),
        ),
        (
            Some(ProcessCommandSpec::Argv(strings(&[
                "not-executed",
                "",
                " a b ",
                "é雪",
                "$X;=\\\n",
            ]))),
            Some(Wire::Argv(strings(&[
                "not-executed",
                "",
                " a b ",
                "é雪",
                "$X;=\\\n",
            ]))),
        ),
        (
            Some(ProcessCommandSpec::Shell(String::new())),
            Some(Wire::Shell(String::new())),
        ),
        (
            Some(ProcessCommandSpec::Shell("  $X; 'é雪'\n\\  ".into())),
            Some(Wire::Shell("  $X; 'é雪'\n\\  ".into())),
        ),
    ]
}

#[test]
fn new_session_preserves_every_process_mode_options_and_owned_storage() {
    for (sdk, wire) in modes() {
        for values in [
            None,
            Some(vec![]),
            Some(strings(&["", " A=B=C ", "é雪", "A=second"])),
        ] {
            let spec = NewSessionSpec {
                session_name: Some(name()),
                working_directory: Some(String::new()),
                detached: true,
                size: None,
                process: ProcessSpec {
                    command: values.clone(),
                    process_command: sdk.clone(),
                    environment: values.clone(),
                },
                group_target: Some(name()),
                reuse: NewSessionReuse {
                    attach_if_exists: true,
                    detach_other_clients: true,
                    kill_other_clients: true,
                    skip_environment_update: true,
                    flags: values.clone(),
                },
                window_name: Some(" window é ".into()),
                print_session_info: true,
                print_format: Some("#{session_name}\n".into()),
            };
            let expected = rmux_proto::NewSessionExtRequest {
                session_name: Some(name()),
                working_directory: Some(String::new()),
                detached: true,
                size: None,
                environment: values.clone(),
                group_target: Some(name()),
                attach_if_exists: true,
                detach_other_clients: true,
                kill_other_clients: true,
                flags: values.clone(),
                window_name: Some(" window é ".into()),
                print_session_info: true,
                print_format: Some("#{session_name}\n".into()),
                command: values.clone(),
                process_command: wire.clone(),
                client_environment: None,
                skip_environment_update: true,
            };
            assert_command_value(
                spec.clone(),
                rmux_proto::Request::NewSessionExt(Box::new(expected.clone())),
            );
            let ptr = spec.session_name.as_ref().unwrap().as_str().as_ptr();
            let process_ptr = sdk_storage(spec.process.process_command.as_ref());
            let RmuxCommandKind::NewSession(actual) = RmuxCommand::new(spec).command else {
                unreachable!()
            };
            assert_eq!(actual.session_name.as_ref().unwrap().as_str().as_ptr(), ptr);
            assert_eq!(
                sdk_storage(actual.process.process_command.as_ref()),
                process_ptr,
                "owned SDK process storage moves into command"
            );
            assert_command_value(
                actual,
                rmux_proto::Request::NewSessionExt(Box::new(expected)),
            );
        }
    }
}

fn sdk_storage(value: Option<&ProcessCommandSpec>) -> Option<usize> {
    match value {
        Some(ProcessCommandSpec::Argv(v)) if !v.is_empty() => Some(v.as_ptr() as usize),
        Some(ProcessCommandSpec::Shell(s)) if !s.is_empty() => Some(s.as_ptr() as usize),
        _ => None,
    }
}
#[test]
fn attach_preserves_terminal_defaults_features_order_and_owned_storage() {
    assert_eq!(
        serde_json::from_str::<ClientTerminalSpec>("{}").unwrap(),
        ClientTerminalSpec::default()
    );
    for features in [vec![], strings(&["", "RGB", "é雪", "RGB", " mouse "])] {
        for utf8 in [false, true] {
            for flags in [
                None,
                Some(vec![]),
                Some(strings(&["", "read-only", "active-pane"])),
            ] {
                let spec = AttachSessionSpec {
                    target: Some(name()),
                    target_spec: Some(String::new()),
                    reuse: AttachSessionReuse {
                        detach_other_clients: true,
                        kill_other_clients: false,
                        read_only: true,
                        skip_environment_update: true,
                        flags: flags.clone(),
                    },
                    working_directory: Some(" #{pane_current_path} ".into()),
                    client_terminal: ClientTerminalSpec {
                        terminal_features: features.clone(),
                        utf8,
                    },
                    client_size: None,
                };
                let expected = rmux_proto::AttachSessionExt2Request {
                    target: Some(name()),
                    target_spec: Some(String::new()),
                    detach_other_clients: true,
                    kill_other_clients: false,
                    read_only: true,
                    skip_environment_update: true,
                    flags,
                    working_directory: Some(" #{pane_current_path} ".into()),
                    client_terminal: rmux_proto::ClientTerminalContext {
                        terminal_features: features.clone(),
                        utf8,
                    },
                    client_size: None,
                };
                let mut bytes = bincode::serialize(&features).unwrap();
                bytes.push(u8::from(utf8));
                assert_eq!(bincode::serialize(&spec.client_terminal).unwrap(), bytes);
                assert_command_value(
                    spec.clone(),
                    rmux_proto::Request::AttachSessionExt2(Box::new(expected.clone())),
                );
                let ptr = spec.client_terminal.terminal_features.as_ptr();
                let RmuxCommandKind::AttachSession(actual) = RmuxCommand::new(spec).command else {
                    unreachable!()
                };
                assert_eq!(actual.client_terminal.terminal_features.as_ptr(), ptr);
                assert_command_value(
                    actual,
                    rmux_proto::Request::AttachSessionExt2(Box::new(expected)),
                );
            }
        }
    }
    assert_command_value(
        AttachSessionSpec::default(),
        rmux_proto::Request::AttachSessionExt2(Box::new(rmux_proto::AttachSessionExt2Request {
            target: None,
            target_spec: None,
            detach_other_clients: false,
            kill_other_clients: false,
            read_only: false,
            skip_environment_update: false,
            flags: None,
            working_directory: None,
            client_terminal: rmux_proto::ClientTerminalContext {
                terminal_features: vec![],
                utf8: false,
            },
            client_size: None,
        })),
    );
}

#[test]
fn split_preserves_both_directions_targets_indices_and_all_process_modes() {
    for (direction, wire_direction, ordinal) in [
        (
            SplitDirectionSpec::Vertical,
            rmux_proto::SplitDirection::Vertical,
            0_u32,
        ),
        (
            SplitDirectionSpec::Horizontal,
            rmux_proto::SplitDirection::Horizontal,
            1,
        ),
    ] {
        assert_eq!(
            bincode::serialize(&direction).unwrap(),
            ordinal.to_le_bytes()
        );
        for (w, p) in [(0, 0), (0, u32::MAX), (u32::MAX, 0), (u32::MAX, u32::MAX)] {
            for is_pane in [false, true] {
                for before in [false, true] {
                    for (sdk, wire) in modes() {
                        let target = if is_pane {
                            SplitTargetSpec::Pane(PaneRef::new(name(), w, p))
                        } else {
                            SplitTargetSpec::Session(name())
                        };
                        let expected_target = if is_pane {
                            rmux_proto::SplitWindowTarget::Pane(
                                rmux_proto::PaneTarget::with_window(name(), w, p),
                            )
                        } else {
                            rmux_proto::SplitWindowTarget::Session(name())
                        };
                        assert_eq!(
                            bincode::serialize(&target).unwrap(),
                            bincode::serialize(&expected_target).unwrap()
                        );
                        assert_eq!(
                            serde_json::to_string(&target).unwrap(),
                            serde_json::to_string(&expected_target).unwrap()
                        );
                        let ptr = match &target {
                            SplitTargetSpec::Session(s) => s.as_str().as_ptr(),
                            SplitTargetSpec::Pane(p) => p.session_name.as_str().as_ptr(),
                        };
                        let spec = SplitSpec {
                            target,
                            direction,
                            before,
                            process: ProcessSpec {
                                command: Some(vec![]),
                                process_command: sdk,
                                environment: Some(strings(&["", "A=B=C", "A=", "é=雪"])),
                            },
                        };
                        let expected = rmux_proto::SplitWindowExtRequest {
                            target: expected_target,
                            direction: wire_direction,
                            before,
                            command: Some(vec![]),
                            process_command: wire,
                            environment: Some(strings(&["", "A=B=C", "A=", "é=雪"])),
                            start_directory: None,
                            keep_alive_on_exit: None,
                            detached: false,
                            size: None,
                            preserve_zoom: false,
                            full_size: false,
                            stdin_payload: None,
                        };
                        assert_command_value(
                            spec.clone(),
                            rmux_proto::Request::SplitWindowExt(Box::new(expected.clone())),
                        );
                        let RmuxCommandKind::SplitWindow(actual) = RmuxCommand::new(spec).command
                        else {
                            unreachable!()
                        };
                        let moved = match &actual.target {
                            SplitTargetSpec::Session(s) => s.as_str().as_ptr(),
                            SplitTargetSpec::Pane(p) => p.session_name.as_str().as_ptr(),
                        };
                        assert_eq!(ptr, moved);
                        assert_command_value(
                            actual,
                            rmux_proto::Request::SplitWindowExt(Box::new(expected)),
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn process_values_keep_serde_names_binary_order_and_constructors() {
    for (sdk, wire) in modes() {
        if let (Some(sdk), Some(wire)) = (sdk, wire) {
            let bytes = bincode::serialize(&sdk).unwrap();
            assert_eq!(bytes, bincode::serialize(&wire).unwrap());
            assert_eq!(
                bincode::deserialize::<ProcessCommandSpec>(&bytes).unwrap(),
                sdk
            );
            let json = serde_json::to_string(&sdk).unwrap();
            assert_eq!(json, serde_json::to_string(&wire).unwrap());
            assert_eq!(
                serde_json::from_str::<ProcessCommandSpec>(&json).unwrap(),
                sdk
            );
        }
    }
    assert_eq!(
        ProcessSpec::argv(["a", ""]).process_command,
        Some(ProcessCommandSpec::Argv(strings(&["a", ""])))
    );
    assert_eq!(
        ProcessSpec::shell("").process_command,
        Some(ProcessCommandSpec::Shell(String::new()))
    );
    assert_eq!(SplitDirectionSpec::default(), SplitDirectionSpec::Vertical);
}

#[test]
fn former_raw_request_payload_is_explicitly_rejected() {
    let request =
        rmux_proto::Request::SplitWindowExt(Box::new(rmux_proto::SplitWindowExtRequest {
            target: rmux_proto::SplitWindowTarget::Session(name()),
            direction: rmux_proto::SplitDirection::Vertical,
            before: true,
            command: Some(strings(&[""])),
            process_command: Some(rmux_proto::ProcessCommand::Shell(String::new())),
            environment: Some(vec![]),
            start_directory: Some("".into()),
            keep_alive_on_exit: Some(false),
            detached: true,
            size: Some("0%".into()),
            preserve_zoom: true,
            full_size: true,
            stdin_payload: Some(vec![]),
        }));
    let json = serde_json::json!({"command": {"Request": request}});
    assert!(serde_json::from_value::<RmuxCommand>(json).is_err());
    let mut bytes = vec![0; 8]; // Default endpoint and retired raw SDK tag 0.
    bytes.extend(bincode::serialize(&request).unwrap());
    assert!(bincode::deserialize::<RmuxCommand>(&bytes).is_err());
}
