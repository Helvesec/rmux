#[path = "common/command_value_assertions.rs"]
mod command_value_assertions;
use command_value_assertions::assert_command_value;

use rmux_sdk::{AttachSessionSpec, NewSessionSpec, RmuxCommand, TerminalSizeSpec};

const SIZES: [(u16, u16); 6] = [
    (0, 0),
    (0, 37),
    (91, 0),
    (123, 45),
    (65535, 1),
    (65535, 65535),
];

#[test]
fn literal_size_fields_const_default_derives_and_serde_are_unchanged() {
    fn derives<T: Copy + Clone + std::fmt::Debug + Default + Eq + std::hash::Hash>() {}
    derives::<TerminalSizeSpec>();
    const SIZE: TerminalSizeSpec = TerminalSizeSpec::new(65535, 0);
    assert_eq!(
        SIZE,
        TerminalSizeSpec {
            cols: 65535,
            rows: 0
        }
    );
    assert_eq!(
        TerminalSizeSpec::default(),
        TerminalSizeSpec { cols: 0, rows: 0 }
    );
    for (cols, rows) in SIZES {
        let size = TerminalSizeSpec::new(cols, rows);
        assert_eq!(size, TerminalSizeSpec { cols, rows });
        let json = format!("{{\"cols\":{cols},\"rows\":{rows}}}");
        assert_eq!(serde_json::to_string(&size).unwrap(), json);
        assert_eq!(
            serde_json::from_str::<TerminalSizeSpec>(&json).unwrap(),
            size
        );
        // Base bincode 1.x contract: declaration-order, little-endian u16 fields.
        let bytes = [cols.to_le_bytes(), rows.to_le_bytes()].concat();
        assert_eq!(bincode::serialize(&size).unwrap(), bytes);
        assert_eq!(
            bincode::deserialize::<TerminalSizeSpec>(&bytes).unwrap(),
            size
        );
    }
}

#[test]
fn new_and_attach_command_entry_paths_preserve_sizes_none_and_adjacent_defaults() {
    for dimensions in std::iter::once(None).chain(SIZES.map(Some)) {
        let size = dimensions.map(|(cols, rows)| TerminalSizeSpec::new(cols, rows));
        let expected = dimensions.map(|(cols, rows)| rmux_proto::TerminalSize { cols, rows });
        let command = RmuxCommand::new(NewSessionSpec {
            size,
            ..Default::default()
        });
        assert_command_value(
            command.command,
            rmux_proto::Request::NewSessionExt(Box::new(rmux_proto::NewSessionExtRequest {
                size: expected,
                session_name: None,
                working_directory: None,
                detached: false,
                environment: None,
                group_target: None,
                attach_if_exists: false,
                detach_other_clients: false,
                kill_other_clients: false,
                flags: None,
                window_name: None,
                print_session_info: false,
                print_format: None,
                command: None,
                process_command: None,
                client_environment: None,
                skip_environment_update: false,
            })),
        );
        let command = RmuxCommand::new(AttachSessionSpec {
            client_size: size,
            ..Default::default()
        });
        assert_command_value(
            command.command,
            rmux_proto::Request::AttachSessionExt2(Box::new(
                rmux_proto::AttachSessionExt2Request {
                    client_size: expected,
                    target: None,
                    target_spec: None,
                    detach_other_clients: false,
                    kill_other_clients: false,
                    read_only: false,
                    skip_environment_update: false,
                    flags: None,
                    working_directory: None,
                    client_terminal: rmux_proto::ClientTerminalContext {
                        terminal_features: Vec::new(),
                        utf8: false,
                    },
                },
            )),
        );
    }
}
