use super::*;
use crate::cli_args::{
    self,
    schema::{self, CommandSpec},
};

impl TinyInvocation {
    pub(super) fn parse(args: &[OsString]) -> Self {
        if env_var_nonempty("RMUX_DISABLE_TINY_CLI") {
            return Self::Fallback;
        }
        let tmux_compatible = invoked_as_tmux(args);
        let arguments = args.get(1..).unwrap_or(&[]);
        match top_level::top_level_decision(arguments, tmux_compatible) {
            Some(top_level::TopLevelDecision::Version) => return Self::Version,
            Some(_) => return Self::Fallback,
            None => {}
        }
        Self::direct(arguments, tmux_compatible)
            .map(|command| Self::Direct(Box::new(command)))
            .unwrap_or(Self::Fallback)
    }

    fn direct(arguments: &[OsString], tmux_compatible: bool) -> Option<TinyCommand> {
        let (command, socket_name, socket_path) = socket_prefix(arguments)?;
        let (name, args) = command.split_first()?;
        let name = rmux_core::command_parser::lookup_command(name.to_str()?)
            .ok()?
            .name;
        let socket_path = tiny_socket(tmux_compatible, socket_name, socket_path)?;
        if env_var_nonempty("RMUX_DISABLE_CLI_TARGET_ACTIONS")
            && matches!(
                name,
                schema::CapturePane::NAME | schema::SplitWindow::NAME | schema::ResizePane::NAME
            )
        {
            return None;
        }
        Some(match name {
            schema::StartServer::NAME if args.is_empty() => {
                TinyCommand::StartServer { socket_path }
            }
            schema::ListSessions::NAME if args.is_empty() => {
                TinyCommand::ListSessions { socket_path }
            }
            schema::HasSession::NAME => TinyCommand::HasSession {
                socket_path,
                request: parse_has_session(args)?,
            },
            schema::ListWindows::NAME => TinyCommand::ListWindows {
                socket_path,
                request: parse_list_windows(args)?,
            },
            schema::ListPanes::NAME => TinyCommand::ListPanes {
                socket_path,
                request: parse_list_panes(args)?,
            },
            schema::KillServer::NAME if args.is_empty() => TinyCommand::KillServer { socket_path },
            schema::CapturePane::NAME => TinyCommand::CapturePane {
                socket_path,
                request: parse_capture_pane(args)?,
            },
            schema::AttachSession::NAME => TinyCommand::AttachSession {
                socket_path,
                request: parse_attach_session(args)?,
            },
            schema::SplitWindow::NAME => TinyCommand::SplitWindow {
                socket_path,
                request: parse_split_window(args)?,
            },
            schema::NewWindow::NAME => TinyCommand::NewWindow {
                socket_path,
                request: parse_new_window(args)?,
            },
            schema::NewSession::NAME => TinyCommand::NewSession {
                socket_path,
                request: parse_new_session(args)?,
            },
            schema::KillSession::NAME => TinyCommand::KillSession {
                socket_path,
                request: parse_kill_session(args)?,
            },
            schema::ShowOptions::NAME => TinyCommand::ShowOptions {
                socket_path,
                request: parse_show_options(args, false)?,
                command_name: schema::ShowOptions::NAME,
            },
            schema::ShowWindowOptions::NAME => TinyCommand::ShowOptions {
                socket_path,
                request: parse_show_options(args, true)?,
                command_name: schema::ShowWindowOptions::NAME,
            },
            schema::RenameWindow::NAME => TinyCommand::RenameWindow {
                socket_path,
                request: parse_rename_window(args)?,
            },
            schema::SelectWindow::NAME => TinyCommand::SelectWindow {
                socket_path,
                request: parse_select_window(args)?,
            },
            schema::KillPane::NAME => TinyCommand::KillPane {
                socket_path,
                request: parse_kill_pane(args)?,
            },
            schema::JoinPane::NAME => TinyCommand::JoinPane {
                socket_path,
                request: parse_join_pane(args)?,
            },
            schema::SetOption::NAME => TinyCommand::SetOption {
                socket_path,
                request: parse_set_option(args, false)?,
                command_name: schema::SetOption::NAME,
            },
            schema::SetWindowOption::NAME => TinyCommand::SetOption {
                socket_path,
                request: parse_set_option(args, true)?,
                command_name: schema::SetWindowOption::NAME,
            },
            schema::ResizePane::NAME => TinyCommand::ResizePane {
                socket_path,
                request: parse_resize_pane(args)?,
            },
            schema::DisplayMessage::NAME => TinyCommand::DisplayMessage {
                socket_path,
                request: parse_display_message(args)?,
            },
            schema::SendKeys::NAME => TinyCommand::SendKeys {
                socket_path,
                request: parse_send_keys(args)?,
            },
            schema::SourceFile::NAME => TinyCommand::SourceFile {
                socket_path,
                request: parse_source_file(args)?,
            },
            _ => return None,
        })
    }
}

/// Only the separate, non-repeated socket selectors enter this transport.
/// Other spellings retain the full parser's diagnostics and precedence.
fn socket_prefix(arguments: &[OsString]) -> Option<(&[OsString], Option<&OsStr>, Option<&Path>)> {
    let mut socket_name = None;
    let mut socket_path = None;
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        match argument.to_str()? {
            "-L" if socket_name.is_none() => {
                let value = arguments.get(index + 1)?;
                if value.is_empty() {
                    return None;
                }
                socket_name = Some(value.as_os_str());
                index += 2;
            }
            "-S" if socket_path.is_none() => {
                let value = arguments.get(index + 1)?;
                if value.is_empty() {
                    return None;
                }
                socket_path = Some(Path::new(value));
                index += 2;
            }
            value if value.starts_with('-') => return None,
            _ => break,
        }
    }
    Some((&arguments[index..], socket_name, socket_path))
}

#[cfg(test)]
mod prefix_tests {
    use super::*;

    #[test]
    fn socket_projection_matches_canonical_prefix_and_opaque_tail() {
        for value in ["socket", "-S", "--", "--help", "a b", "é"] {
            for prefix in [
                vec![],
                vec!["-L", value],
                vec!["-S", value],
                vec!["-L", value, "-S", value],
                vec!["-S", value, "-L", value],
            ] {
                for tail in [
                    vec!["capture-pane", "-p", "-S", "-10000"],
                    vec!["display-message", "-p", "a; b", "--help"],
                    vec![],
                ] {
                    let args: Vec<_> = prefix
                        .iter()
                        .chain(&tail)
                        .map(|value| OsString::from(*value))
                        .collect();
                    let (command, name, path) = socket_prefix(&args).unwrap();
                    let canonical = cli_args::scan_top_level_command(&args).unwrap();
                    assert_eq!(command, canonical.command);
                    assert_eq!(name, canonical.socket_name.as_deref());
                    assert_eq!(path, canonical.socket_path.as_deref().map(Path::new));
                }
            }
        }
    }

    #[test]
    fn unsupported_prefixes_retain_full_parser_ownership() {
        for args in [
            vec!["-L"],
            vec!["-S"],
            vec!["-L", ""],
            vec!["-S", ""],
            vec!["-L", "one", "-L", "two"],
            vec!["-S", "one", "-S", "two"],
            vec!["-Lattached"],
            vec!["--"],
            vec!["-f", "file"],
            vec!["-q"],
            vec!["-V"],
        ] {
            let args: Vec<_> = args.into_iter().map(OsString::from).collect();
            assert!(socket_prefix(&args).is_none(), "{args:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_socket_bytes_are_preserved() {
        use std::os::unix::ffi::OsStringExt;
        let raw = OsString::from_vec(vec![b'/', 0xff]);
        for flag in ["-L", "-S"] {
            let args = vec![flag.into(), raw.clone(), "capture-pane".into(), "-p".into()];
            let (command, name, path) = socket_prefix(&args).unwrap();
            let canonical = cli_args::scan_top_level_command(&args).unwrap();
            assert_eq!(command, canonical.command);
            assert_eq!(name, canonical.socket_name.as_deref());
            assert_eq!(path, canonical.socket_path.as_deref().map(Path::new));
        }
    }
}
