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
pub(crate) use config::{
    build_scope, SetEnvironmentArgs, SetHookArgs, SetOptionArgs, SetOptionCommandKind,
    ShowEnvironmentArgs, ShowHooksArgs, ShowOptionsArgs, ShowOptionsCommandKind,
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
use pane::{parse_resize_pane_args, parse_select_pane_args};
pub(crate) use pane::{
    BreakPaneArgs, ClockModeArgs, CopyModeArgs, DisplayPanesArgs, JoinPaneArgs, ListPanesArgs,
    PaneTargetArgs, PipePaneArgs, ResizePaneArgs, RespawnPaneArgs, SelectLayoutArgs,
    SelectPaneArgs, SplitWindowArgs, SwapPaneArgs,
};
#[path = "cli_args/session.rs"]
mod session;
pub(crate) use session::{
    AlertSessionTargetArgs, AttachSessionArgs, ClientTargetArgs, KillSessionArgs, ListSessionsArgs,
    NewSessionArgs, RenameSessionArgs, ServerAccessArgs, SessionTargetArgs, ShowMessagesArgs,
};
#[path = "cli_args/window.rs"]
mod window;
pub(crate) use window::{
    FindWindowArgs, KillWindowArgs, LinkWindowArgs, ListWindowsArgs, MoveWindowArgs, NewWindowArgs,
    RenameWindowArgs, ResizeWindowArgs, RespawnWindowArgs, RotateWindowArgs, SwapWindowArgs,
    UnlinkWindowArgs, WindowTargetArgs,
};
#[path = "cli_args/web.rs"]
mod web;
pub(crate) use web::WebShareArgs;

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

pub(crate) fn parse<I, T>(args: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let mut command = RawCli::command();
    command = command.after_help(implemented_command_help());
    let matches = command.try_get_matches_from(args)?;
    let raw = RawCli::from_arg_matches(&matches)?;
    let parsed_commands = parse_command_queue(&raw.command)?;
    Cli::from_raw(raw, parsed_commands)
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
        .get_or_init(|| COMMAND_TABLE.iter().collect())
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
    socket_path: Option<PathBuf>,
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
            socket_path: raw.socket_path,
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
    .disable_help_subcommand(true)
    .arg(
        clap::Arg::new("help")
            .long("help")
            .action(ArgAction::Help)
            .help("Print help"),
    );
    let matches = command.try_get_matches_from(arguments)?;
    T::from_arg_matches(&matches)
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
    SelectWindow(WindowTargetArgs),
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
    LastPane(WindowTargetArgs),
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
    #[arg(long = "web-port", value_name = "port")]
    pub(crate) web_port: Option<u16>,
    #[arg(long = "web-frontend", value_name = "url")]
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
