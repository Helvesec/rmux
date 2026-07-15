use std::ffi::{OsStr, OsString};

use clap::{ArgAction, Args, FromArgMatches};
use rmux_core::command_parser::{
    CommandArgument, CommandParseError, CommandParser as TmuxCommandParser, ParsedCommand,
    ParsedCommands,
};

use super::*;

pub(super) fn parse_command_queue(arguments: &[OsString]) -> Result<ParsedCommands, clap::Error> {
    if arguments.is_empty() {
        return Ok(ParsedCommands::default());
    }

    let arguments = arguments
        .iter()
        .map(|argument| command_argument_to_string(argument))
        .collect::<Result<Vec<_>, _>>()?;
    let arguments = expand_cli_argument_aliases(arguments);
    TmuxCommandParser::new()
        .with_exact_commands(super::RMUX_EXTENSION_COMMANDS)
        .parse_arguments(&arguments)
        .map_err(command_parse_error_to_clap)
}

pub(super) fn parse_runtime_command_groups(
    groups: &[RuntimeCommandGroup],
) -> Result<ParsedCommands, clap::Error> {
    let parser = TmuxCommandParser::new()
        .with_command_aliases(std::iter::empty::<String>())
        .with_exact_commands(super::RMUX_EXTENSION_COMMANDS);
    let mut parsed = ParsedCommands::default();

    for group in groups {
        let RuntimeCommandGroup::Canonical(rendered) = group;
        if rendered.is_empty() {
            continue;
        }
        let commands = parser
            .parse_one_group(rendered)
            .map_err(command_parse_error_to_clap)?;
        parsed.append(commands);
    }

    Ok(parsed)
}

fn expand_cli_argument_aliases(arguments: Vec<String>) -> Vec<String> {
    let mut expanded = Vec::with_capacity(arguments.len() + 2);
    let mut command_start = true;

    for argument in arguments {
        let (base, ends_command) = split_cli_command_terminator(&argument);
        if command_start {
            match base {
                "choose-window" => {
                    expanded.push("choose-tree".to_owned());
                    expanded.push(if ends_command {
                        "-w;".to_owned()
                    } else {
                        "-w".to_owned()
                    });
                    command_start = ends_command;
                    continue;
                }
                "choose-session" => {
                    expanded.push("choose-tree".to_owned());
                    expanded.push(if ends_command {
                        "-s;".to_owned()
                    } else {
                        "-s".to_owned()
                    });
                    command_start = ends_command;
                    continue;
                }
                _ => {}
            }
        }

        expanded.push(argument);
        command_start = ends_command;
    }

    expanded
}

fn split_cli_command_terminator(argument: &str) -> (&str, bool) {
    if let Some(stripped) = argument.strip_suffix(';') {
        if stripped.ends_with('\\') {
            (argument, false)
        } else {
            (stripped, true)
        }
    } else {
        (argument, false)
    }
}

fn command_argument_to_string(argument: &OsStr) -> Result<String, clap::Error> {
    argument.to_str().map(str::to_owned).ok_or_else(|| {
        clap::Error::raw(
            clap::error::ErrorKind::InvalidUtf8,
            "invalid UTF-8 in command argument",
        )
    })
}

fn command_parse_error_to_clap(error: CommandParseError) -> clap::Error {
    let message = cli_command_error_message(error.message());

    let kind =
        if message.starts_with("unknown command: ") || message.starts_with("ambiguous command: ") {
            clap::error::ErrorKind::InvalidSubcommand
        } else {
            clap::error::ErrorKind::ValueValidation
        };
    clap::Error::raw(kind, message.to_owned())
}

fn cli_command_error_message(message: &str) -> &str {
    let original = message;
    let Some(rest) = original.strip_prefix("-:") else {
        return message;
    };
    let Some((line, stripped)) = rest.split_once(": ") else {
        return original;
    };
    if line.bytes().all(|byte| byte.is_ascii_digit()) {
        stripped
    } else {
        original
    }
}

pub(super) fn command_from_parsed(command: ParsedCommand) -> Result<Command, clap::Error> {
    let name = command.name().to_owned();
    let error_command_name = name.clone();
    let queue_command = command.to_tmux_reparse_string();
    let arguments = command_arguments_for_clap(command.arguments());
    let parsed = match name.as_str() {
        "new-session" => parse_command_args("new-session", arguments).map(Command::NewSession),
        "start-server" => parse_command_args("start-server", arguments).map(Command::StartServer),
        "kill-server" => parse_no_args("kill-server", arguments).map(|()| Command::KillServer),
        "has-session" => parse_command_args("has-session", arguments).map(Command::HasSession),
        "kill-session" => parse_command_args("kill-session", arguments).map(Command::KillSession),
        "rename-session" => {
            parse_command_args("rename-session", arguments).map(Command::RenameSession)
        }
        "server-access" => parse_server_access_args(arguments).map(Command::ServerAccess),
        "lock-server" => parse_no_args("lock-server", arguments).map(|()| Command::LockServer),
        "lock-session" => parse_command_args("lock-session", arguments).map(Command::LockSession),
        "lock-client" => parse_command_args("lock-client", arguments).map(Command::LockClient),
        "new-window" => parse_command_args("new-window", arguments).map(Command::NewWindow),
        "kill-window" => parse_command_args("kill-window", arguments).map(Command::KillWindow),
        "select-window" => parse_select_window_args(arguments).map(Command::SelectWindow),
        "rename-window" => parse_rename_window_args(arguments).map(Command::RenameWindow),
        "next-window" => parse_command_args("next-window", arguments).map(Command::NextWindow),
        "previous-window" => {
            parse_command_args("previous-window", arguments).map(Command::PreviousWindow)
        }
        "last-window" => parse_command_args("last-window", arguments).map(Command::LastWindow),
        "list-sessions" => parse_command_args::<ListSessionsArgs>("list-sessions", arguments)
            .and_then(ListSessionsArgs::validate)
            .map(Command::ListSessions),
        "list-windows" => parse_command_args("list-windows", arguments).map(Command::ListWindows),
        "move-window" => parse_command_args("move-window", arguments).map(Command::MoveWindow),
        "swap-window" => parse_swap_window_args(arguments).map(Command::SwapWindow),
        "rotate-window" => {
            parse_command_args("rotate-window", arguments).map(Command::RotateWindow)
        }
        "resize-window" => {
            parse_command_args("resize-window", arguments).map(Command::ResizeWindow)
        }
        "respawn-window" => {
            parse_command_args("respawn-window", arguments).map(Command::RespawnWindow)
        }
        "split-window" => parse_split_window_args(arguments).map(Command::SplitWindow),
        "swap-pane" => parse_command_args("swap-pane", arguments).map(Command::SwapPane),
        "last-pane" => parse_command_args("last-pane", arguments).map(Command::LastPane),
        "join-pane" => parse_join_pane_args("join-pane", arguments).map(Command::JoinPane),
        "move-pane" => parse_join_pane_args("move-pane", arguments).map(Command::MovePane),
        "break-pane" => parse_command_args("break-pane", arguments).map(Command::BreakPane),
        "pipe-pane" => parse_command_args("pipe-pane", arguments).map(Command::PipePane),
        "respawn-pane" => parse_command_args("respawn-pane", arguments).map(Command::RespawnPane),
        "kill-pane" => parse_command_args("kill-pane", arguments).map(Command::KillPane),
        "select-layout" => parse_select_layout_args(arguments).map(Command::SelectLayout),
        "next-layout" => parse_command_args("next-layout", arguments).map(Command::NextLayout),
        "previous-layout" => {
            parse_command_args("previous-layout", arguments).map(Command::PreviousLayout)
        }
        "resize-pane" => parse_resize_pane_args(arguments).map(Command::ResizePane),
        "display-panes" => {
            parse_command_args("display-panes", arguments).map(Command::DisplayPanes)
        }
        "list-panes" => parse_command_args("list-panes", arguments).map(Command::ListPanes),
        "select-pane" => parse_select_pane_args(arguments).map(Command::SelectPane),
        "copy-mode" => parse_command_args::<CopyModeArgs>("copy-mode", arguments)
            .and_then(CopyModeArgs::validate)
            .map(Command::CopyMode),
        "clock-mode" => parse_command_args("clock-mode", arguments).map(Command::ClockMode),
        "wait-pane" => parse_command_args::<WaitPaneArgs>("wait-pane", arguments)
            .and_then(WaitPaneArgs::validate)
            .map(Command::WaitPane),
        "pane-snapshot" => {
            parse_command_args("pane-snapshot", arguments).map(Command::PaneSnapshot)
        }
        "stream-pane" => parse_command_args::<StreamPaneArgs>("stream-pane", arguments)
            .and_then(StreamPaneArgs::validate)
            .map(Command::StreamPane),
        "collect-pane-output" => {
            parse_command_args::<CollectPaneOutputArgs>("collect-pane-output", arguments)
                .and_then(CollectPaneOutputArgs::validate)
                .map(Command::CollectPaneOutput)
        }
        "locator" => parse_command_args::<LocatorArgs>("locator", arguments)
            .and_then(LocatorArgs::validate)
            .map(Command::Locator),
        "expect-pane" => parse_command_args::<ExpectPaneArgs>("expect-pane", arguments)
            .and_then(ExpectPaneArgs::validate)
            .map(Command::ExpectPane),
        "find-panes" => parse_command_args("find-panes", arguments).map(Command::FindPanes),
        "find-sessions" => {
            parse_command_args("find-sessions", arguments).map(Command::FindSessions)
        }
        "broadcast-keys" => parse_command_args::<BroadcastKeysArgs>("broadcast-keys", arguments)
            .and_then(BroadcastKeysArgs::validate)
            .map(Command::BroadcastKeys),
        "with-session" => parse_command_args::<WithSessionArgs>("with-session", arguments)
            .and_then(WithSessionArgs::validate)
            .map(Command::WithSession),
        "send-keys" => parse_send_keys_args(arguments).map(Command::SendKeys),
        "bind-key" => parse_command_args("bind-key", arguments).map(Command::BindKey),
        "unbind-key" => parse_command_args("unbind-key", arguments).map(Command::UnbindKey),
        "list-commands" => {
            parse_command_args("list-commands", arguments).map(Command::ListCommands)
        }
        "list-keys" => parse_command_args::<ListKeysArgs>("list-keys", arguments)
            .and_then(ListKeysArgs::validate)
            .map(Command::ListKeys),
        "send-prefix" => parse_command_args("send-prefix", arguments).map(Command::SendPrefix),
        "attach-session" => {
            parse_command_args("attach-session", arguments).map(Command::AttachSession)
        }
        "refresh-client" => {
            parse_command_args("refresh-client", arguments).map(Command::RefreshClient)
        }
        "list-clients" => parse_command_args("list-clients", arguments).map(Command::ListClients),
        "switch-client" => {
            parse_command_args("switch-client", arguments).map(Command::SwitchClient)
        }
        "detach-client" => {
            parse_command_args("detach-client", arguments).map(Command::DetachClient)
        }
        "suspend-client" => {
            parse_command_args("suspend-client", arguments).map(Command::SuspendClient)
        }
        "set-option" => parse_set_option_args("set-option", arguments).map(Command::SetOption),
        "set-window-option" => {
            parse_set_option_args("set-window-option", arguments).map(Command::SetWindowOption)
        }
        "set-environment" => {
            parse_command_args("set-environment", arguments).map(Command::SetEnvironment)
        }
        "show-options" => {
            parse_show_options_args("show-options", arguments).map(Command::ShowOptions)
        }
        "show-window-options" => parse_show_options_args("show-window-options", arguments)
            .map(Command::ShowWindowOptions),
        "show-environment" => {
            parse_command_args("show-environment", arguments).map(Command::ShowEnvironment)
        }
        "set-hook" => parse_command_args("set-hook", arguments).map(Command::SetHook),
        "show-hooks" => parse_command_args("show-hooks", arguments).map(Command::ShowHooks),
        "set-buffer" => parse_set_buffer_args(arguments).map(Command::SetBuffer),
        "show-buffer" => parse_command_args("show-buffer", arguments).map(Command::ShowBuffer),
        "paste-buffer" => parse_command_args("paste-buffer", arguments).map(Command::PasteBuffer),
        "list-buffers" => parse_command_args("list-buffers", arguments).map(Command::ListBuffers),
        "delete-buffer" => {
            parse_command_args("delete-buffer", arguments).map(Command::DeleteBuffer)
        }
        "load-buffer" => parse_command_args("load-buffer", arguments).map(Command::LoadBuffer),
        "save-buffer" => parse_command_args("save-buffer", arguments).map(Command::SaveBuffer),
        "capture-pane" => parse_command_args::<CapturePaneArgs>("capture-pane", arguments)
            .and_then(CapturePaneArgs::validate)
            .map(Command::CapturePane),
        "clear-history" => {
            parse_command_args("clear-history", arguments).map(Command::ClearHistory)
        }
        "display-message" => {
            parse_queue_command_args::<DisplayMessageArgs>("display-message", arguments)
                .and_then(DisplayMessageArgs::validate)
                .map(|args| Command::DisplayMessage(with_queue_command(args, queue_command)))
        }
        "show-messages" => {
            parse_command_args("show-messages", arguments).map(Command::ShowMessages)
        }
        "run-shell" => parse_command_args::<RunShellArgs>("run-shell", arguments)
            .and_then(RunShellArgs::validate)
            .map(Command::RunShell),
        "source-file" => parse_source_file_args(arguments).map(Command::SourceFile),
        "if-shell" => parse_queue_command_args::<IfShellArgs>("if-shell", arguments)
            .map(|args| Command::IfShell(with_queue_command(args, queue_command))),
        "wait-for" => parse_command_args("wait-for", arguments).map(Command::WaitFor),
        "web-share" => super::web::parse_web_share_args(arguments).map(Command::WebShare),
        "command-prompt" => parse_queue_command_args::<PromptArgs>("command-prompt", arguments)
            .and_then(PromptArgs::validate)
            .map(|args| Command::Prompt(with_queue_command(args, queue_command))),
        "confirm-before" => {
            parse_queue_command_args::<ConfirmBeforeArgs>("confirm-before", arguments)
                .map(|args| Command::ConfirmBefore(with_queue_command(args, queue_command)))
        }
        "find-window" => parse_queue_command_args::<FindWindowArgs>("find-window", arguments)
            .map(|args| Command::FindWindow(with_queue_command(args, queue_command))),
        "link-window" => parse_command_args("link-window", arguments).map(Command::LinkWindow),
        "unlink-window" => {
            parse_command_args("unlink-window", arguments).map(Command::UnlinkWindow)
        }
        "choose-tree" => parse_queue_command_args::<ChooseTreeArgs>("choose-tree", arguments)
            .and_then(ChooseTreeArgs::validate)
            .map(|args| Command::ChooseTree(with_queue_command(args, queue_command))),
        "choose-buffer" => parse_queue_command_args::<ChooseBufferArgs>("choose-buffer", arguments)
            .and_then(ChooseBufferArgs::validate)
            .map(|args| Command::ChooseBuffer(with_queue_command(args, queue_command))),
        "choose-client" => parse_queue_command_args::<ChooseClientArgs>("choose-client", arguments)
            .and_then(ChooseClientArgs::validate)
            .map(|args| Command::ChooseClient(with_queue_command(args, queue_command))),
        "customize-mode" => {
            parse_queue_command_args::<CustomizeModeArgs>("customize-mode", arguments)
                .map(|args| Command::CustomizeMode(with_queue_command(args, queue_command)))
        }
        "display-menu" | "menu" => {
            parse_queue_command_args::<DisplayMenuArgs>("display-menu", arguments)
                .map(|args| Command::DisplayMenu(with_queue_command(args, queue_command)))
        }
        "display-popup" | "popup" => {
            parse_queue_command_args::<DisplayPopupArgs>("display-popup", arguments)
                .map(|args| Command::DisplayPopup(with_queue_command(args, queue_command)))
        }
        "clear-prompt-history" | "clearphist" => {
            parse_queue_command_args::<PromptHistoryArgs>("clear-prompt-history", arguments)
                .map(|args| Command::ClearPromptHistory(with_queue_command(args, queue_command)))
        }
        "show-prompt-history" | "showphist" => {
            parse_queue_command_args::<PromptHistoryArgs>("show-prompt-history", arguments)
                .map(|args| Command::ShowPromptHistory(with_queue_command(args, queue_command)))
        }
        "capabilities" => {
            let is_help = arguments.iter().any(|arg| arg == "--help");
            if is_help {
                Err(clap::Error::raw(
                    clap::error::ErrorKind::DisplayHelp,
                    "usage: rmux capabilities [--human|--json]\n",
                ))
            } else {
                Ok(Command::Unsupported(UnsupportedCommandArgs {
                    name,
                    arguments,
                }))
            }
        }
        _ => Ok(Command::Unsupported(UnsupportedCommandArgs {
            name,
            arguments,
        })),
    };

    parsed.map_err(|mut error| {
        error.insert(
            clap::error::ContextKind::Custom,
            clap::error::ContextValue::String(error_command_name),
        );
        error
    })
}

fn command_arguments_for_clap(arguments: &[CommandArgument]) -> Vec<String> {
    arguments
        .iter()
        .map(|argument| match argument {
            CommandArgument::String(value) => value.clone(),
            CommandArgument::Commands(_) => argument.to_tmux_reparse_string(),
        })
        .collect()
}

fn parse_no_args(command_name: &'static str, arguments: Vec<String>) -> Result<(), clap::Error> {
    clap::Command::new(command_name)
        .no_binary_name(true)
        .disable_help_flag(true)
        .arg(
            clap::Arg::new("help")
                .long("help")
                .action(ArgAction::Help)
                .help("Print help"),
        )
        .try_get_matches_from(arguments)
        .map(|_| ())
}

fn parse_send_keys_args(arguments: Vec<String>) -> Result<SendKeysArgs, clap::Error> {
    let has_wait = arguments.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "--wait"
                | "--wait-text"
                | "--wait-visible-text"
                | "--wait-next-text"
                | "--wait-pane-exit"
        ) || argument.starts_with("--wait=")
            || argument.starts_with("--wait-text=")
            || argument.starts_with("--wait-visible-text=")
            || argument.starts_with("--wait-next-text=")
    });
    if has_wait && !arguments.iter().any(|argument| argument == "--") {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            "command send-keys: -- is required before payload when using --wait options",
        ));
    }
    parse_command_args::<SendKeysArgs>("send-keys", arguments).and_then(SendKeysArgs::validate)
}

fn parse_server_access_args(arguments: Vec<String>) -> Result<ServerAccessArgs, clap::Error> {
    for argument in &arguments {
        if argument == "--" {
            break;
        }
        if argument == "--help" {
            continue;
        }
        if argument.starts_with("--") {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::UnknownArgument,
                "command server-access: invalid flag --",
            ));
        }
        if argument == "-" {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::UnknownArgument,
                "command server-access: invalid flag -",
            ));
        }
        let Some(flags) = argument.strip_prefix('-') else {
            continue;
        };
        if flags.is_empty() {
            continue;
        }
        for flag in flags.chars() {
            if !matches!(flag, 'a' | 'd' | 'l' | 'r' | 't' | 'w') {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::UnknownArgument,
                    format!("command server-access: unknown flag -{flag}"),
                ));
            }
        }
    }

    let args = parse_command_args::<ServerAccessArgs>("server-access", arguments)?;
    args.validate()
}

fn parse_set_option_args(
    command_name: &'static str,
    mut arguments: Vec<String>,
) -> Result<SetOptionArgs, clap::Error> {
    let trailing_literal_separator = normalize_set_option_separator(command_name, &mut arguments)?;
    let explicit_scope = set_option_scope(command_name, &arguments);
    let kind = match command_name {
        "set-option" => SetOptionCommandKind::SetOption,
        "set-window-option" => SetOptionCommandKind::SetWindowOption,
        _ => unreachable!("unexpected set-option command name"),
    };
    let mut args = match kind {
        SetOptionCommandKind::SetOption => {
            let mut args = parse_command_args::<SetOptionArgs>(command_name, arguments)?;
            apply_set_option_scope(&mut args, explicit_scope);
            args
        }
        SetOptionCommandKind::SetWindowOption => {
            parse_command_args::<SetWindowOptionArgs>(command_name, arguments)?.into()
        }
    };
    if trailing_literal_separator {
        if args.value.is_some() {
            return Err(set_option_too_many_arguments(command_name));
        }
        args.value = Some("--".to_owned());
    }
    args.validate(kind)
}

#[derive(Debug, Clone, Copy)]
enum SetOptionScopeFlag {
    Server,
    Window,
    Pane,
}

#[derive(Debug, Default, Clone, Copy)]
struct SetOptionScopeFlags {
    server: bool,
    window: bool,
    pane: bool,
}

impl SetOptionScopeFlags {
    const fn selected(self) -> Option<SetOptionScopeFlag> {
        if self.server {
            Some(SetOptionScopeFlag::Server)
        } else if self.pane {
            Some(SetOptionScopeFlag::Pane)
        } else if self.window {
            Some(SetOptionScopeFlag::Window)
        } else {
            None
        }
    }
}

fn set_option_scope(
    command_name: &'static str,
    arguments: &[String],
) -> Option<SetOptionScopeFlag> {
    if command_name != "set-option" {
        return None;
    }

    let mut scopes = SetOptionScopeFlags::default();
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if argument == "--" {
            break;
        }
        if !argument.starts_with('-') || argument == "-" {
            break;
        }
        if argument.starts_with("-t") && argument.len() > 2 {
            index += 1;
            continue;
        }

        let mut chars = argument[1..].chars().peekable();
        while let Some(flag) = chars.next() {
            match flag {
                's' => scopes.server = true,
                // -U is an unset modifier, not a scope selector: plain
                // `set -U` targets the session copy (oracle 2026-07-09) and
                // -p/-w keep their own precedence when combined with it.
                'w' => scopes.window = true,
                'p' => scopes.pane = true,
                't' => {
                    if chars.peek().is_none() {
                        index += 1;
                    }
                    break;
                }
                _ => {}
            }
        }
        index += 1;
    }

    scopes.selected()
}

fn apply_set_option_scope(args: &mut SetOptionArgs, scope: Option<SetOptionScopeFlag>) {
    let Some(scope) = scope else {
        return;
    };
    args.server = false;
    args.window = false;
    args.pane = false;
    match scope {
        SetOptionScopeFlag::Server => args.server = true,
        SetOptionScopeFlag::Window => args.window = true,
        SetOptionScopeFlag::Pane => args.pane = true,
    }
}

fn normalize_set_option_separator(
    command_name: &'static str,
    arguments: &mut Vec<String>,
) -> Result<bool, clap::Error> {
    let Some(index) = arguments.iter().position(|argument| argument == "--") else {
        return Ok(false);
    };
    if index + 1 == arguments.len() {
        let _ = arguments.pop();
        return Ok(true);
    }
    if set_option_positionals_before_separator(&arguments[..index]) > 0 {
        return Err(set_option_too_many_arguments(command_name));
    }
    let _ = arguments.remove(index);
    Ok(false)
}

fn set_option_positionals_before_separator(arguments: &[String]) -> usize {
    let mut positionals = 0;
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "-t" {
            index += 2;
            continue;
        }
        if argument.starts_with("-t") && argument.len() > 2 {
            index += 1;
            continue;
        }
        if argument.starts_with('-') && argument.len() > 1 {
            let flags = &argument[1..];
            if let Some((offset, flag)) = flags.char_indices().find(|(_, flag)| *flag == 't') {
                index += if offset + flag.len_utf8() == flags.len() {
                    2
                } else {
                    1
                };
                continue;
            }
            index += 1;
            continue;
        }
        positionals += 1;
        index += 1;
    }
    positionals
}

fn set_option_too_many_arguments(command_name: &'static str) -> clap::Error {
    clap::Error::raw(
        clap::error::ErrorKind::TooManyValues,
        format!("command {command_name}: too many arguments (need at most 2)"),
    )
}

fn parse_show_options_args(
    command_name: &'static str,
    arguments: Vec<String>,
) -> Result<ShowOptionsArgs, clap::Error> {
    match command_name {
        "show-options" => parse_command_args::<ShowOptionsArgs>(command_name, arguments),
        "show-window-options" => {
            parse_command_args::<ShowWindowOptionsArgs>(command_name, arguments).map(Into::into)
        }
        _ => unreachable!("unexpected show-options command name"),
    }
}

fn parse_set_buffer_args(arguments: Vec<String>) -> Result<SetBufferArgs, clap::Error> {
    let args = parse_command_args::<SetBufferArgs>("set-buffer", arguments)?;
    args.validate()
}

fn parse_queue_command_args<T>(
    command_name: &'static str,
    arguments: Vec<String>,
) -> Result<T, clap::Error>
where
    T: Args + FromArgMatches,
{
    parse_command_args(command_name, arguments)
}

fn with_queue_command<T: QueuedCommand>(mut args: T, queue_command: String) -> T {
    args.set_queue_command(queue_command);
    args
}
