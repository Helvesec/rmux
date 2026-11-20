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
        // Only socket selection belongs to these direct transports. The typed
        // top-level parser owns the values, duplicates and opaque argv handling.
        let mut index = 0;
        while let Some(argument) = arguments.get(index) {
            match argument.to_str()? {
                "-L" | "-S" => index += 2,
                value if value.starts_with('-') => return None,
                _ => break,
            }
        }
        let prefix = cli_args::scan_top_level_command(arguments).ok()?;
        let (name, args) = prefix.command.split_first()?;
        let name = rmux_core::command_parser::lookup_command(name.to_str()?)
            .ok()?
            .name;
        let socket_path = tiny_socket(
            tmux_compatible,
            prefix.socket_name.as_deref(),
            prefix.socket_path.as_deref().map(Path::new),
        )?;
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
