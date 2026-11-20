//! Clap argument model for the public RMUX command surface.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::{ArgAction, Args, CommandFactory, FromArgMatches, Parser};
use rmux_core::{
    command_inventory::RMUX_EXTENSION_COMMANDS,
    command_parser::{CommandEntry, ParsedCommands, COMMAND_TABLE},
    tmux_precedence,
};

#[cfg(test)]
use rmux_core::command_parser::CommandParser as TmuxCommandParser;

mod buffer;
pub(crate) use buffer::{
    DeleteBufferArgs, ListBuffersArgs, LoadBufferArgs, PasteBufferArgs, SaveBufferArgs,
    SetBufferArgs, ShowBufferArgs,
};
mod client;
pub(crate) use client::{
    DetachClientArgs, ListClientsArgs, RefreshClientArgs, SuspendClientArgs, SwitchClientArgs,
};
mod config;
#[cfg(test)]
pub(crate) use config::build_scope;
pub(crate) use config::{
    SetEnvironmentArgs, SetHookArgs, SetOptionArgs, SetOptionCommandKind, SetWindowOptionArgs,
    ShowEnvironmentArgs, ShowHooksArgs, ShowOptionsArgs, ShowOptionsCommandKind,
    ShowWindowOptionsArgs,
};
mod control;
use control::initial_control_command_lines;
mod automation;
pub(crate) use automation::{
    BroadcastKeysArgs, CollectPaneOutputArgs, ExpectPaneArgs, FindPanesArgs, FindSessionsArgs,
    LocatorArgs, PaneSnapshotArgs, SendKeysWaitMode, SnapshotRegion, StreamPaneArgs, WaitPaneArgs,
    WithSessionArgs,
};
mod keys;
pub(crate) use keys::{BindKeyArgs, ListKeysArgs, SendKeysArgs, SendPrefixArgs, UnbindKeyArgs};
mod history;
pub(crate) use history::{CapturePaneArgs, ClearHistoryArgs};
mod inventory;
pub(crate) use inventory::ListCommandsArgs;
mod mode_tree;
pub(crate) use mode_tree::{ChooseBufferArgs, ChooseClientArgs, ChooseTreeArgs, CustomizeModeArgs};
mod message;
pub(crate) use message::DisplayMessageArgs;
mod prompt;
pub(crate) use prompt::{ConfirmBeforeArgs, PromptArgs, PromptHistoryArgs};
mod queue;
#[allow(unused_imports)]
pub(crate) use queue::command_arguments_for_clap;
pub(crate) use queue::parse_command_queue;
use queue::{command_from_parsed, parse_runtime_command_groups};
mod script;
use script::parse_source_file_args;
pub(crate) use script::{IfShellArgs, RunShellArgs, SourceFileArgs, WaitForArgs};
mod overlay;
pub(crate) use overlay::{DisplayMenuArgs, DisplayPopupArgs};
mod targets;
use targets::{parse_session_name, parse_session_target};
pub(crate) use targets::{parse_target_spec, TargetSpec};
mod pane;
use pane::{
    parse_join_pane_args, parse_resize_pane_args, parse_select_layout_args, parse_select_pane_args,
    parse_split_window_args,
};
pub(crate) use pane::{
    BreakPaneArgs, ClockModeArgs, CopyModeArgs, DisplayPanesArgs, JoinPaneArgs, LastPaneArgs,
    ListPanesArgs, PaneTargetArgs, PipePaneArgs, ResizePaneArgs, ResizePaneSize, RespawnPaneArgs,
    SelectLayoutArgs, SelectLayoutMode, SelectPaneArgs, SplitWindowArgs, SwapPaneArgs,
};
mod session;
pub(crate) use session::{
    AlertSessionTargetArgs, AttachSessionArgs, ClientTargetArgs, HasSessionArgs, KillSessionArgs,
    ListSessionsArgs, NewSessionArgs, RenameSessionArgs, ServerAccessArgs, SessionTargetArgs,
    ShowMessagesArgs,
};
mod negotiated_pane;
mod window;
use window::{parse_rename_window_args, parse_select_window_args, parse_swap_window_args};
pub(crate) use window::{
    ExportWindowSnapshotArgs, FindWindowArgs, KillWindowArgs, LinkWindowArgs, ListWindowsArgs,
    MoveWindowArgs, NewWindowArgs, RenameWindowArgs, ResizeWindowArgs, RespawnWindowArgs,
    RestoreWindowSnapshotArgs, RotateWindowArgs, SelectWindowArgs, SwapWindowArgs,
    UnlinkWindowArgs, WindowTargetArgs,
};
mod web;
pub(crate) use web::{WebShareArgs, WebShareTerminalThemeArg, WEB_SHARE_TUNNEL_PROVIDERS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DocumentedCliAlias {
    pub(crate) alias: &'static str,
    pub(crate) expansion: &'static str,
}

pub(crate) use queue::schema;
pub(crate) use schema::Command;
use schema::DOCUMENTED_CLI_ALIASES;
pub(crate) use schema::{parse_diagnose_args, DiagnoseArgumentError};

pub(crate) fn parse<I, T>(args: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let raw = parse_raw_cli(args)?;
    if raw.control_mode != 0 {
        return Cli::from_raw_control(raw);
    }
    let parsed_commands = parse_command_queue(&raw.command)?;
    reject_parse_time_assignments_without_runtime_bridge(&parsed_commands)?;
    Cli::from_raw(raw, parsed_commands, false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeCommandGroup {
    Canonical(String),
}

fn reject_parse_time_assignments_without_runtime_bridge(
    parsed_commands: &ParsedCommands,
) -> Result<(), clap::Error> {
    let Some(assignment) = parsed_commands.assignments().first() else {
        return Ok(());
    };
    let token = format!("{}={}", assignment.name(), assignment.value());
    let token = token
        .chars()
        .flat_map(char::escape_default)
        .collect::<String>();
    Err(clap::Error::raw(
        clap::error::ErrorKind::InvalidSubcommand,
        format!("unknown command: {token}"),
    ))
}

pub(crate) fn parse_with_runtime_command_groups<I, T>(
    args: I,
    runtime_groups: &[RuntimeCommandGroup],
) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let raw = parse_raw_cli(args)?;
    let parsed_commands = parse_runtime_command_groups(runtime_groups)?;
    Cli::from_raw(raw, parsed_commands, true)
}

fn parse_raw_cli<I, T>(args: I) -> Result<RawCli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args = normalize_top_level_attached_short_values(args.into_iter().map(Into::into));
    let mut command = RawCli::command();
    command = command.after_help(implemented_command_help());
    let matches = command.try_get_matches_from(args)?;
    RawCli::from_arg_matches(&matches)
}

/// Result of parsing only the clap-owned top-level prefix and opaque command
/// tail. Extensions use this before command-queue parsing so they share the
/// exact same short-option cluster and value-boundary rules as the main CLI.
#[derive(Debug)]
pub(crate) struct TopLevelCommandScan {
    pub(crate) assume_256_colors: bool,
    pub(crate) control_mode: u8,
    pub(crate) no_fork: bool,
    pub(crate) shell_command: Option<String>,
    pub(crate) config_files: Vec<PathBuf>,
    pub(crate) login_shell: bool,
    pub(crate) socket_name: Option<OsString>,
    pub(crate) no_start_server: bool,
    pub(crate) socket_path: Option<OsString>,
    pub(crate) terminal_features: Vec<String>,
    pub(crate) utf8: bool,
    pub(crate) verbose: u8,
    pub(crate) command: Vec<OsString>,
}

pub(crate) fn scan_top_level_command(
    arguments: &[OsString],
) -> Result<TopLevelCommandScan, clap::Error> {
    let args = std::iter::once(OsString::from("rmux"))
        .chain(arguments.iter().cloned())
        .collect::<Vec<_>>();
    let args = normalize_top_level_attached_short_values(args);
    let matches = RawCli::command().try_get_matches_from(args)?;
    let raw = RawCli::from_arg_matches(&matches)?;
    Ok(TopLevelCommandScan {
        assume_256_colors: raw.assume_256_colors,
        control_mode: raw.control_mode,
        no_fork: raw.no_fork,
        shell_command: raw.shell_command,
        config_files: raw.config_files,
        login_shell: raw.login_shell,
        socket_name: raw.socket_name,
        no_start_server: raw.no_start_server,
        socket_path: raw.socket_path,
        terminal_features: raw.terminal_features,
        utf8: raw.utf8,
        verbose: raw.verbose,
        command: raw.command,
    })
}

fn normalize_top_level_attached_short_values<I>(args: I) -> Vec<OsString>
where
    I: IntoIterator<Item = OsString>,
{
    let mut normalized = Vec::new();
    let mut args = crate::cli_top_level::normalize_legacy_q(args.into_iter().collect()).into_iter();
    let Some(binary) = args.next() else {
        return normalized;
    };
    normalized.push(binary);

    let mut passthrough = false;
    let mut preserve_hyphen_value = false;
    for argument in args {
        if passthrough {
            normalized.push(argument);
            continue;
        }
        if preserve_hyphen_value {
            preserve_hyphen_value = false;
            normalized.push(argument);
            continue;
        }
        let Some(value) = argument.to_str() else {
            normalized.push(argument);
            continue;
        };
        if value == "--" {
            passthrough = true;
            normalized.push(argument);
            continue;
        }
        if !value.starts_with('-') || value == "-" {
            passthrough = true;
            normalized.push(argument);
            continue;
        }
        if matches!(value, "-L" | "-S" | "-T") {
            preserve_hyphen_value = true;
            normalized.push(argument);
            continue;
        }
        if let Some((flag, attached)) = top_level_attached_short_value(value) {
            normalized.push(OsString::from(format!("-{flag}")));
            normalized.push(OsString::from(attached));
        } else {
            normalized.push(argument);
        }
    }

    normalized
}

fn top_level_attached_short_value(argument: &str) -> Option<(char, &str)> {
    let mut chars = argument.chars();
    if chars.next()? != '-' || chars.as_str().starts_with('-') {
        return None;
    }
    let flag = chars.next()?;
    let value = chars.as_str();
    (!value.is_empty() && matches!(flag, 'c' | 'f' | 'L' | 'S' | 'T')).then_some((flag, value))
}

use inventory::implemented_command_help;
pub(crate) use inventory::{documented_cli_aliases, implemented_command_surface};

#[allow(dead_code)]
mod completion;
#[allow(unused_imports)]
pub(crate) use completion::completion_command;

#[derive(Debug)]
pub(crate) struct Cli {
    pub(crate) assume_256_colors: bool,
    pub(crate) control_mode: u8,
    pub(crate) no_fork: bool,
    pub(crate) shell_command: Option<String>,
    config_files: Vec<PathBuf>,
    pub(crate) login_shell: bool,
    socket_name: Option<OsString>,
    pub(crate) no_start_server: bool,
    socket_path: Option<PathBuf>,
    terminal_features: Vec<String>,
    pub(crate) utf8: bool,
    pub(crate) verbose: u8,
    pub(crate) command: Option<Command>,
    command_queue: Vec<Command>,
    control_command_lines: Vec<String>,
}

#[derive(Debug, Parser)]
#[command(disable_help_subcommand = true, version)]
struct RawCli {
    #[arg(
        short = '2',
        action = ArgAction::SetTrue,
        overrides_with = "assume_256_colors"
    )]
    assume_256_colors: bool,
    #[arg(short = 'C', action = ArgAction::Count)]
    control_mode: u8,
    #[arg(short = 'D', action = ArgAction::SetTrue, overrides_with = "no_fork")]
    no_fork: bool,
    #[arg(short = 'c', value_name = "shell-command")]
    shell_command: Option<String>,
    #[arg(short = 'f', value_name = "file")]
    config_files: Vec<PathBuf>,
    #[arg(
        short = 'l',
        action = ArgAction::SetTrue,
        overrides_with = "login_shell"
    )]
    login_shell: bool,
    #[arg(short = 'L', value_name = "socket-name", allow_hyphen_values = true)]
    socket_name: Option<OsString>,
    #[arg(
        short = 'N',
        action = ArgAction::SetTrue,
        overrides_with = "no_start_server"
    )]
    no_start_server: bool,
    #[arg(short = 'S', value_name = "socket-path", allow_hyphen_values = true)]
    socket_path: Option<OsString>,
    #[arg(short = 'T', value_name = "features", allow_hyphen_values = true)]
    terminal_features: Vec<String>,
    #[arg(short = 'u', action = ArgAction::SetTrue, overrides_with = "utf8")]
    utf8: bool,
    #[arg(short = 'v', action = ArgAction::Count)]
    verbose: u8,
    #[arg(
        value_name = "command",
        allow_hyphen_values = true,
        trailing_var_arg = true
    )]
    command: Vec<OsString>,
}

impl Cli {
    fn from_raw_control(raw: RawCli) -> Result<Self, clap::Error> {
        let explicit_command_args = !raw.command.is_empty();
        let control_command_lines = initial_control_command_lines(&raw.command)?;
        let command_queue = explicit_command_args
            .then_some(Command::Noop)
            .into_iter()
            .collect::<Vec<_>>();
        let primary_command = command_queue.first().cloned();
        Ok(Self::from_raw_queue(
            raw,
            primary_command,
            command_queue,
            control_command_lines,
        ))
    }

    fn from_raw(
        raw: RawCli,
        parsed_commands: ParsedCommands,
        apply_runtime_assignments: bool,
    ) -> Result<Self, clap::Error> {
        let explicit_command_args = !raw.command.is_empty();
        let control_command_lines = if raw.control_mode == 0 {
            Vec::new()
        } else {
            initial_control_command_lines(&raw.command)?
        };
        let runtime_assignments = apply_runtime_assignments
            .then(|| parsed_commands.assignments_to_tmux_reparse_string())
            .filter(|assignments| !assignments.is_empty());
        let mut command_queue = parsed_commands
            .into_commands()
            .into_iter()
            .map(command_from_parsed)
            .collect::<Result<Vec<_>, _>>()?;
        if command_queue.is_empty() && explicit_command_args {
            command_queue.push(Command::Noop);
        }
        let primary_command = command_queue.first().cloned();
        if raw.control_mode == 0 {
            if let Some(assignments) = runtime_assignments {
                command_queue.insert(0, Command::ApplyParseTimeAssignments(assignments));
            }
        }
        Ok(Self::from_raw_queue(
            raw,
            primary_command,
            command_queue,
            control_command_lines,
        ))
    }

    fn from_raw_queue(
        raw: RawCli,
        primary_command: Option<Command>,
        command_queue: Vec<Command>,
        control_command_lines: Vec<String>,
    ) -> Self {
        Self {
            assume_256_colors: raw.assume_256_colors,
            control_mode: raw.control_mode,
            no_fork: raw.no_fork,
            shell_command: raw.shell_command,
            config_files: raw.config_files,
            login_shell: raw.login_shell,
            socket_name: raw.socket_name,
            no_start_server: raw.no_start_server,
            socket_path: raw.socket_path.map(PathBuf::from),
            terminal_features: raw.terminal_features,
            utf8: raw.utf8,
            verbose: raw.verbose,
            command: primary_command,
            command_queue,
            control_command_lines,
        }
    }

    pub(crate) fn socket_name(&self) -> Option<&std::ffi::OsStr> {
        self.socket_name.as_deref()
    }

    pub(crate) fn socket_path(&self) -> Option<&Path> {
        self.socket_path.as_deref()
    }

    pub(crate) fn config_file_selection(&self) -> ConfigFileSelection<'_> {
        match self.config_files.as_slice() {
            [] => ConfigFileSelection::Default,
            files => ConfigFileSelection::Custom(files),
        }
    }

    pub(crate) fn terminal_features(&self) -> &[String] {
        &self.terminal_features
    }

    pub(crate) fn command_queue(&self) -> &[Command] {
        &self.command_queue
    }

    pub(crate) fn into_command_queue(self) -> Vec<Command> {
        self.command_queue
    }

    pub(crate) fn control_command_lines(&self) -> &[String] {
        &self.control_command_lines
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigFileSelection<'a> {
    Default,
    Custom(&'a [PathBuf]),
}

fn parse_command_args<T>(
    command_name: &'static str,
    arguments: Vec<String>,
) -> Result<T, clap::Error>
where
    T: Args + FromArgMatches,
{
    parse_command_args_with_policy(
        command_name,
        arguments,
        PositionalOptionPolicy::StopAtFirstPositional,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PositionalOptionPolicy {
    StopAtFirstPositional,
    InterspersedBeforeSeparator,
}

fn parse_command_args_with_policy<T>(
    command_name: &'static str,
    arguments: Vec<String>,
    positional_option_policy: PositionalOptionPolicy,
) -> Result<T, clap::Error>
where
    T: Args + FromArgMatches,
{
    let command = schema::typed_command::<T>(command_name);
    let arguments = normalize_attached_short_values(&command, arguments);
    let arguments = tmux_precedence::normalize_tmux_precedence(command_name, arguments);
    let arguments = match positional_option_policy {
        PositionalOptionPolicy::StopAtFirstPositional => {
            delimit_options_before_positionals(command_name, &command, arguments)?
        }
        PositionalOptionPolicy::InterspersedBeforeSeparator => arguments,
    };
    let matches = command.try_get_matches_from(arguments)?;
    T::from_arg_matches(&matches)
}

use queue::normalization::{
    argument_requires_value, delimit_options_before_positionals, normalize_attached_short_values,
};

#[derive(Debug, Clone)]
pub(crate) struct UnsupportedCommandArgs {
    pub(crate) name: String,
    pub(crate) arguments: Vec<String>,
}

#[derive(Debug, Clone, Default, Args)]
pub(crate) struct StartServerArgs {
    #[arg(long = "web-port", value_name = "port", value_parser = clap::value_parser!(u16).range(1..))]
    pub(crate) web_port: Option<u16>,
    #[arg(long = "frontend-url", alias = "web-frontend", value_name = "url")]
    pub(crate) web_frontend: Option<String>,
}

trait QueuedCommand {
    fn set_queue_command(&mut self, queue_command: String);
}

#[cfg(test)]
#[path = "../cli_args_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "../cli_args_config_tests.rs"]
mod config_tests;
#[cfg(test)]
#[path = "../cli_args_layout_tests.rs"]
mod layout_tests;
#[cfg(test)]
#[path = "../cli_args_zoom_tests.rs"]
mod zoom_tests;
