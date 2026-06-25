//! Clap argument model for the public RMUX command surface.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use clap::{ArgAction, Args, CommandFactory, FromArgMatches, Parser};
use rmux_core::command_parser::{CommandEntry, ParsedCommands, COMMAND_TABLE};

#[cfg(test)]
use rmux_core::command_parser::CommandParser as TmuxCommandParser;

#[path = "cli_args/buffer.rs"]
mod buffer;
pub(crate) use buffer::{
    DeleteBufferArgs, ListBuffersArgs, LoadBufferArgs, PasteBufferArgs, SaveBufferArgs,
    SetBufferArgs, ShowBufferArgs,
};
#[path = "cli_args/client.rs"]
mod client;
pub(crate) use client::{
    DetachClientArgs, ListClientsArgs, RefreshClientArgs, SuspendClientArgs, SwitchClientArgs,
};
#[path = "cli_args/config.rs"]
mod config;
#[cfg(test)]
pub(crate) use config::build_scope;
pub(crate) use config::{
    SetEnvironmentArgs, SetHookArgs, SetOptionArgs, SetOptionCommandKind, ShowEnvironmentArgs,
    ShowHooksArgs, ShowOptionsArgs, ShowOptionsCommandKind,
};
#[path = "cli_args/automation.rs"]
mod automation;
pub(crate) use automation::{
    BroadcastKeysArgs, CollectPaneOutputArgs, ExpectPaneArgs, FindPanesArgs, FindSessionsArgs,
    LocatorArgs, PaneSnapshotArgs, SendKeysWaitMode, SnapshotRegion, StreamPaneArgs, WaitPaneArgs,
    WithSessionArgs,
};
#[path = "cli_args/keys.rs"]
mod keys;
pub(crate) use keys::{BindKeyArgs, ListKeysArgs, SendKeysArgs, SendPrefixArgs, UnbindKeyArgs};
#[path = "cli_args/history.rs"]
mod history;
pub(crate) use history::{CapturePaneArgs, ClearHistoryArgs};
#[path = "cli_args/inventory.rs"]
mod inventory;
pub(crate) use inventory::ListCommandsArgs;
#[path = "cli_args/mode_tree.rs"]
mod mode_tree;
pub(crate) use mode_tree::{ChooseBufferArgs, ChooseClientArgs, ChooseTreeArgs, CustomizeModeArgs};
#[path = "cli_args/message.rs"]
mod message;
pub(crate) use message::DisplayMessageArgs;
#[path = "cli_args/prompt.rs"]
mod prompt;
pub(crate) use prompt::{ConfirmBeforeArgs, PromptArgs, PromptHistoryArgs};
#[path = "cli_args/queue.rs"]
mod queue;
use queue::{command_from_parsed, parse_command_queue};
#[path = "cli_args/script.rs"]
mod script;
use script::parse_source_file_args;
pub(crate) use script::{IfShellArgs, RunShellArgs, SourceFileArgs, WaitForArgs};
#[path = "cli_args/overlay.rs"]
mod overlay;
pub(crate) use overlay::{DisplayMenuArgs, DisplayPopupArgs};
#[path = "cli_args/targets.rs"]
mod targets;
use targets::{parse_session_name, parse_target};
pub(crate) use targets::{parse_target_spec, TargetSpec};
#[path = "cli_args/pane.rs"]
mod pane;
#[path = "cli_args/tmux_last_wins.rs"]
mod tmux_last_wins;
use pane::{
    parse_join_pane_args, parse_resize_pane_args, parse_select_layout_args, parse_select_pane_args,
    parse_split_window_args,
};
pub(crate) use pane::{
    BreakPaneArgs, ClockModeArgs, CopyModeArgs, DisplayPanesArgs, JoinPaneArgs, LastPaneArgs,
    ListPanesArgs, PaneTargetArgs, PipePaneArgs, ResizePaneArgs, ResizePaneSize, RespawnPaneArgs,
    SelectLayoutArgs, SelectPaneArgs, SplitWindowArgs, SwapPaneArgs,
};
#[path = "cli_args/session.rs"]
mod session;
pub(crate) use session::{
    AlertSessionTargetArgs, AttachSessionArgs, ClientTargetArgs, KillSessionArgs, ListSessionsArgs,
    NewSessionArgs, RenameSessionArgs, ServerAccessArgs, SessionTargetArgs, ShowMessagesArgs,
};
#[path = "cli_args/window.rs"]
mod window;
use window::{parse_rename_window_args, parse_select_window_args, parse_swap_window_args};
pub(crate) use window::{
    FindWindowArgs, KillWindowArgs, LinkWindowArgs, ListWindowsArgs, MoveWindowArgs, NewWindowArgs,
    RenameWindowArgs, ResizeWindowArgs, RespawnWindowArgs, RotateWindowArgs, SelectWindowArgs,
    SwapWindowArgs, UnlinkWindowArgs, WindowTargetArgs,
};
#[path = "cli_args/web.rs"]
mod web;
pub(crate) use web::{WebShareArgs, WebShareTerminalThemeArg, WEB_SHARE_TUNNEL_PROVIDERS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DocumentedCliAlias {
    pub(crate) alias: &'static str,
    pub(crate) expansion: &'static str,
}

const DOCUMENTED_CLI_ALIASES: &[DocumentedCliAlias] = &[
    DocumentedCliAlias {
        alias: "choose-session",
        expansion: "choose-tree -s",
    },
    DocumentedCliAlias {
        alias: "choose-window",
        expansion: "choose-tree -w",
    },
];

static IMPLEMENTED_COMMAND_SURFACE: OnceLock<Vec<&'static CommandEntry>> = OnceLock::new();

static IMPLEMENTED_COMMAND_HELP: OnceLock<String> = OnceLock::new();

const RMUX_EXTENSION_COMMANDS: &[CommandEntry] = &[
    CommandEntry {
        name: "capabilities",
        alias: None,
    },
    CommandEntry {
        name: "claude",
        alias: None,
    },
    CommandEntry {
        name: "doctor",
        alias: None,
    },
    CommandEntry {
        name: "setup",
        alias: None,
    },
    CommandEntry {
        name: "wait-pane",
        alias: None,
    },
    CommandEntry {
        name: "pane-snapshot",
        alias: None,
    },
    CommandEntry {
        name: "stream-pane",
        alias: None,
    },
    CommandEntry {
        name: "collect-pane-output",
        alias: None,
    },
    CommandEntry {
        name: "locator",
        alias: None,
    },
    CommandEntry {
        name: "expect-pane",
        alias: None,
    },
    CommandEntry {
        name: "find-panes",
        alias: None,
    },
    CommandEntry {
        name: "find-sessions",
        alias: None,
    },
    CommandEntry {
        name: "broadcast-keys",
        alias: None,
    },
    CommandEntry {
        name: "with-session",
        alias: None,
    },
    CommandEntry {
        name: "web-share",
        alias: None,
    },
];

pub(crate) fn parse<I, T>(args: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args = normalize_top_level_attached_short_values(args.into_iter().map(Into::into));
    let mut command = RawCli::command();
    command = command.after_help(implemented_command_help());
    let matches = command.try_get_matches_from(args)?;
    let raw = RawCli::from_arg_matches(&matches)?;
    let parsed_commands = parse_command_queue(&raw.command)?;
    Cli::from_raw(raw, parsed_commands)
}

fn normalize_top_level_attached_short_values<I>(args: I) -> Vec<OsString>
where
    I: IntoIterator<Item = OsString>,
{
    let mut normalized = Vec::new();
    let mut args = args.into_iter();
    let Some(binary) = args.next() else {
        return normalized;
    };
    normalized.push(binary);

    let mut passthrough = false;
    for argument in args {
        if passthrough {
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

fn build_implemented_command_help() -> String {
    let mut help = String::from("Commands:\n");
    for entry in implemented_command_surface() {
        help.push_str("  ");
        help.push_str(entry.name);
        if let Some(alias) = entry.alias {
            help.push_str(" (");
            help.push_str(alias);
            help.push(')');
        }
        help.push('\n');
    }

    help.push_str("\nBuilt-in command aliases:\n");
    for alias in documented_cli_aliases() {
        help.push_str("  ");
        help.push_str(alias.alias);
        help.push_str(" => ");
        help.push_str(alias.expansion);
        help.push('\n');
    }

    help.trim_end().to_owned()
}

pub(crate) fn implemented_command_surface() -> &'static [&'static CommandEntry] {
    IMPLEMENTED_COMMAND_SURFACE
        .get_or_init(|| {
            COMMAND_TABLE
                .iter()
                .chain(RMUX_EXTENSION_COMMANDS.iter())
                .collect()
        })
        .as_slice()
}

fn implemented_command_help() -> &'static str {
    IMPLEMENTED_COMMAND_HELP
        .get_or_init(build_implemented_command_help)
        .as_str()
}

pub(crate) fn documented_cli_aliases() -> &'static [DocumentedCliAlias] {
    DOCUMENTED_CLI_ALIASES
}

#[allow(dead_code)]
#[path = "cli_args/completion.rs"]
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
    #[arg(short = '2', action = ArgAction::SetTrue)]
    assume_256_colors: bool,
    #[arg(short = 'C', action = ArgAction::Count)]
    control_mode: u8,
    #[arg(short = 'D', action = ArgAction::SetTrue)]
    no_fork: bool,
    #[arg(short = 'c', value_name = "shell-command")]
    shell_command: Option<String>,
    #[arg(short = 'f', value_name = "file")]
    config_files: Vec<PathBuf>,
    #[arg(short = 'l', action = ArgAction::SetTrue)]
    login_shell: bool,
    #[arg(short = 'L', value_name = "socket-name", allow_hyphen_values = true)]
    socket_name: Option<OsString>,
    #[arg(short = 'N', action = ArgAction::SetTrue)]
    no_start_server: bool,
    #[arg(short = 'S', value_name = "socket-path", allow_hyphen_values = true)]
    socket_path: Option<OsString>,
    #[arg(short = 'T', value_name = "features", allow_hyphen_values = true)]
    terminal_features: Vec<String>,
    #[arg(short = 'u', action = ArgAction::SetTrue)]
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
    fn from_raw(raw: RawCli, parsed_commands: ParsedCommands) -> Result<Self, clap::Error> {
        let control_command_lines = if parsed_commands.is_empty() {
            Vec::new()
        } else {
            vec![parsed_commands.to_tmux_string()]
        };
        let command_queue = parsed_commands
            .into_commands()
            .into_iter()
            .map(command_from_parsed)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
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
            command: command_queue.first().cloned(),
            command_queue,
            control_command_lines,
        })
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
    let command = T::augment_args(
        clap::Command::new(command_name)
            .no_binary_name(true)
            .disable_help_flag(true),
    )
    .args_override_self(true)
    .disable_help_subcommand(true)
    .arg(
        clap::Arg::new("help")
            .long("help")
            .action(ArgAction::Help)
            .help("Print help"),
    );
    let arguments = normalize_attached_short_values(&command, arguments);
    let arguments = tmux_last_wins::normalize(command_name, arguments);
    validate_options_before_positionals(command_name, &command, &arguments)?;
    let matches = command.try_get_matches_from(arguments)?;
    T::from_arg_matches(&matches)
}

fn validate_options_before_positionals(
    command_name: &'static str,
    command: &clap::Command,
    arguments: &[String],
) -> Result<(), clap::Error> {
    let positionals_allow_hyphen = command
        .get_positionals()
        .any(|argument| argument.is_allow_hyphen_values_set());

    let short_flags = command
        .get_arguments()
        .filter_map(clap::Arg::get_short)
        .collect::<std::collections::BTreeSet<_>>();
    let value_flags = command
        .get_arguments()
        .filter(|argument| argument_requires_value(argument))
        .filter_map(clap::Arg::get_short)
        .collect::<std::collections::BTreeSet<_>>();
    let long_flags = command
        .get_arguments()
        .filter_map(clap::Arg::get_long)
        .collect::<std::collections::BTreeSet<_>>();
    let long_value_flags = command
        .get_arguments()
        .filter(|argument| argument_requires_value(argument))
        .filter_map(clap::Arg::get_long)
        .collect::<std::collections::BTreeSet<_>>();

    let mut expected_value_flag = None::<String>;
    for argument in arguments {
        if expected_value_flag.take().is_some() {
            continue;
        }
        if argument == "--" {
            break;
        }
        if !argument.starts_with('-') || argument == "-" {
            break;
        }
        if let Some(long) = argument.strip_prefix("--") {
            let name = long.split_once('=').map_or(long, |(name, _)| name);
            if !long_flags.contains(name) {
                if positionals_allow_hyphen {
                    return Err(unknown_flag_error(command_name, format!("--{name}")));
                }
                continue;
            }
            if long_value_flags.contains(name) && !long.contains('=') {
                expected_value_flag = Some(format!("--{name}"));
            }
            continue;
        }

        let mut chars = argument[1..].chars().peekable();
        while let Some(flag) = chars.next() {
            if !short_flags.contains(&flag) {
                if positionals_allow_hyphen {
                    return Err(unknown_flag_error(command_name, format!("-{flag}")));
                }
                continue;
            }
            if value_flags.contains(&flag) {
                if chars.peek().is_none() {
                    expected_value_flag = Some(format!("-{flag}"));
                }
                break;
            }
        }
    }

    if let Some(flag) = expected_value_flag {
        return Err(missing_value_error(command_name, flag));
    }

    Ok(())
}

fn argument_requires_value(argument: &clap::Arg) -> bool {
    matches!(argument.get_action(), ArgAction::Set | ArgAction::Append)
        && argument
            .get_num_args()
            .is_none_or(|range| range.min_values() > 0)
}

fn unknown_flag_error(command_name: &'static str, flag: String) -> clap::Error {
    clap::Error::raw(
        clap::error::ErrorKind::UnknownArgument,
        format!("command {command_name}: unknown flag {flag}"),
    )
}

fn missing_value_error(command_name: &'static str, flag: String) -> clap::Error {
    clap::Error::raw(
        clap::error::ErrorKind::ValueValidation,
        format!("command {command_name}: {flag} expects an argument"),
    )
}

fn normalize_attached_short_values(command: &clap::Command, arguments: Vec<String>) -> Vec<String> {
    let value_flags = command
        .get_arguments()
        .filter(|argument| argument_requires_value(argument))
        .filter_map(clap::Arg::get_short)
        .collect::<std::collections::BTreeSet<_>>();
    let has_trailing_position = command
        .get_positionals()
        .any(clap::Arg::is_trailing_var_arg_set);
    let repeat_last_wins_flags = if command.get_name() == "new-window" {
        command
            .get_arguments()
            .filter(|argument| matches!(argument.get_action(), ArgAction::Set))
            .filter_map(clap::Arg::get_short)
            .filter(|flag| *flag == 't')
            .collect::<std::collections::BTreeSet<_>>()
    } else {
        std::collections::BTreeSet::new()
    };
    if value_flags.is_empty() {
        return arguments;
    }

    let mut normalized_options = Vec::with_capacity(arguments.len());
    let mut trailing_values = Vec::new();
    let mut passthrough = false;
    let mut expect_next_value = false;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        if passthrough {
            trailing_values.push(argument);
            continue;
        }
        if expect_next_value {
            normalized_options.push(argument);
            expect_next_value = false;
            continue;
        }
        if argument == "--" {
            passthrough = true;
            trailing_values.push(argument);
            continue;
        }

        if has_trailing_position && is_trailing_position_start(&argument) {
            trailing_values.push(argument);
            trailing_values.extend(arguments);
            break;
        }

        if let Some((flag, value)) = attached_short_value(&argument, &value_flags) {
            normalized_options.push(format!("-{flag}"));
            normalized_options.push(value.to_owned());
        } else if exact_short_flag(&argument, &value_flags).is_some() {
            expect_next_value = true;
            normalized_options.push(argument);
        } else {
            normalized_options.push(argument);
        }
    }

    let mut normalized =
        collapse_repeated_short_values(normalized_options, &repeat_last_wins_flags);
    normalized.extend(trailing_values);
    normalized
}

fn is_trailing_position_start(argument: &str) -> bool {
    !argument.starts_with('-') || argument == "-"
}

fn attached_short_value<'a>(
    argument: &'a str,
    value_flags: &std::collections::BTreeSet<char>,
) -> Option<(char, &'a str)> {
    let mut chars = argument.chars();
    if chars.next()? != '-' || chars.as_str().starts_with('-') {
        return None;
    }

    let flag = chars.next()?;
    let value = chars.as_str();
    (!value.is_empty() && value_flags.contains(&flag)).then_some((flag, value))
}

fn collapse_repeated_short_values(
    arguments: Vec<String>,
    repeat_last_wins_flags: &std::collections::BTreeSet<char>,
) -> Vec<String> {
    if repeat_last_wins_flags.is_empty() {
        return arguments;
    }

    let mut occurrences = std::collections::BTreeMap::<char, Vec<(usize, usize)>>::new();
    let mut index = 0;
    while index + 1 < arguments.len() {
        if let Some(flag) = exact_short_flag(&arguments[index], repeat_last_wins_flags) {
            occurrences
                .entry(flag)
                .or_default()
                .push((index, index + 1));
            index += 2;
        } else {
            index += 1;
        }
    }

    let mut drop_indexes = std::collections::BTreeSet::new();
    for positions in occurrences.values() {
        for (flag_index, value_index) in positions
            .iter()
            .take(positions.len().saturating_sub(1))
            .copied()
        {
            drop_indexes.insert(flag_index);
            drop_indexes.insert(value_index);
        }
    }

    arguments
        .into_iter()
        .enumerate()
        .filter_map(|(index, argument)| (!drop_indexes.contains(&index)).then_some(argument))
        .collect()
}

fn exact_short_flag(argument: &str, flags: &std::collections::BTreeSet<char>) -> Option<char> {
    let mut chars = argument.chars();
    if chars.next()? != '-' || chars.as_str().starts_with('-') {
        return None;
    }
    let flag = chars.next()?;
    (chars.next().is_none() && flags.contains(&flag)).then_some(flag)
}

#[derive(Debug, Clone)]
pub(crate) enum Command {
    NewSession(NewSessionArgs),
    StartServer(StartServerArgs),
    KillServer,
    HasSession(SessionTargetArgs),
    KillSession(KillSessionArgs),
    RenameSession(RenameSessionArgs),
    ServerAccess(ServerAccessArgs),
    LockServer,
    LockSession(SessionTargetArgs),
    LockClient(ClientTargetArgs),
    NewWindow(NewWindowArgs),
    KillWindow(KillWindowArgs),
    SelectWindow(SelectWindowArgs),
    RenameWindow(RenameWindowArgs),
    NextWindow(AlertSessionTargetArgs),
    PreviousWindow(AlertSessionTargetArgs),
    LastWindow(SessionTargetArgs),
    ListSessions(ListSessionsArgs),
    ListWindows(ListWindowsArgs),
    MoveWindow(MoveWindowArgs),
    SwapWindow(SwapWindowArgs),
    RotateWindow(RotateWindowArgs),
    ResizeWindow(ResizeWindowArgs),
    RespawnWindow(RespawnWindowArgs),
    SplitWindow(SplitWindowArgs),
    SwapPane(SwapPaneArgs),
    LastPane(LastPaneArgs),
    JoinPane(JoinPaneArgs),
    MovePane(JoinPaneArgs),
    BreakPane(BreakPaneArgs),
    PipePane(PipePaneArgs),
    RespawnPane(RespawnPaneArgs),
    KillPane(PaneTargetArgs),
    SelectLayout(SelectLayoutArgs),
    NextLayout(WindowTargetArgs),
    PreviousLayout(WindowTargetArgs),
    ResizePane(ResizePaneArgs),
    DisplayPanes(DisplayPanesArgs),
    ListPanes(ListPanesArgs),
    SelectPane(SelectPaneArgs),
    CopyMode(CopyModeArgs),
    ClockMode(ClockModeArgs),
    WaitPane(WaitPaneArgs),
    PaneSnapshot(PaneSnapshotArgs),
    StreamPane(StreamPaneArgs),
    CollectPaneOutput(CollectPaneOutputArgs),
    Locator(LocatorArgs),
    ExpectPane(ExpectPaneArgs),
    FindPanes(FindPanesArgs),
    FindSessions(FindSessionsArgs),
    BroadcastKeys(BroadcastKeysArgs),
    WithSession(WithSessionArgs),
    SendKeys(SendKeysArgs),
    BindKey(BindKeyArgs),
    UnbindKey(UnbindKeyArgs),
    ListCommands(ListCommandsArgs),
    ListKeys(ListKeysArgs),
    SendPrefix(SendPrefixArgs),
    Prompt(PromptArgs),
    ConfirmBefore(ConfirmBeforeArgs),
    FindWindow(FindWindowArgs),
    LinkWindow(LinkWindowArgs),
    UnlinkWindow(UnlinkWindowArgs),
    ChooseTree(ChooseTreeArgs),
    ChooseBuffer(ChooseBufferArgs),
    ChooseClient(ChooseClientArgs),
    CustomizeMode(CustomizeModeArgs),
    AttachSession(AttachSessionArgs),
    RefreshClient(RefreshClientArgs),
    ListClients(ListClientsArgs),
    SwitchClient(SwitchClientArgs),
    DetachClient(DetachClientArgs),
    SuspendClient(SuspendClientArgs),
    SetOption(SetOptionArgs),
    SetWindowOption(SetOptionArgs),
    SetEnvironment(SetEnvironmentArgs),
    ShowOptions(ShowOptionsArgs),
    ShowWindowOptions(ShowOptionsArgs),
    ShowEnvironment(ShowEnvironmentArgs),
    SetHook(SetHookArgs),
    ShowHooks(ShowHooksArgs),
    SetBuffer(SetBufferArgs),
    ShowBuffer(ShowBufferArgs),
    PasteBuffer(PasteBufferArgs),
    ListBuffers(ListBuffersArgs),
    DeleteBuffer(DeleteBufferArgs),
    LoadBuffer(LoadBufferArgs),
    SaveBuffer(SaveBufferArgs),
    CapturePane(CapturePaneArgs),
    ClearHistory(ClearHistoryArgs),
    DisplayMessage(DisplayMessageArgs),
    ShowMessages(ShowMessagesArgs),
    RunShell(RunShellArgs),
    SourceFile(SourceFileArgs),
    IfShell(IfShellArgs),
    WaitFor(WaitForArgs),
    WebShare(WebShareArgs),
    DisplayMenu(DisplayMenuArgs),
    DisplayPopup(DisplayPopupArgs),
    ClearPromptHistory(PromptHistoryArgs),
    ShowPromptHistory(PromptHistoryArgs),
    Unsupported(UnsupportedCommandArgs),
}

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
#[path = "cli_args_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "cli_args_config_tests.rs"]
mod config_tests;
#[cfg(test)]
#[path = "cli_args_layout_tests.rs"]
mod layout_tests;
#[cfg(test)]
#[path = "cli_args_zoom_tests.rs"]
mod zoom_tests;
