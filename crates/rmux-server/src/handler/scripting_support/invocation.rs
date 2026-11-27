//! Typed entries shared by all command producers.
use super::list_commands_runtime::ParsedListCommandsCommand;
use super::list_parse::{ParsedListPanesAllCommand, ParsedListWindowsAllCommand};
use super::pane_parse::ParsedSplitWindowCommand;
use super::prompt_parse::{
    ParsedCommandPromptCommand, ParsedConfirmBeforeCommand, ParsedPromptHistoryCommand,
};
use super::queue_parse::{ParsedIfShellCommand, ParsedNewWindowCommand};
use super::shell_parse::ParsedRunShellCommand;
use super::source_files::ParsedSourceFileCommand;
use rmux_proto::Request;

#[derive(Debug, Clone)]
pub(super) enum QueueInvocation {
    Request(Request),
    QuietOnlyIfUnset(Request),
    RestoreWindowSnapshot(super::window_snapshot_parse::ParsedRestoreWindowSnapshot),
    RunShell(ParsedRunShellCommand),
    NoOp,
    StartServer,
    ListCommands(ParsedListCommandsCommand),
    NewWindow(ParsedNewWindowCommand),
    NewPane(super::new_pane::ParsedNewPane),
    MoveFloatingPane(super::floating_move::ParsedFloatingPaneMove),
    SetPanePlacement(super::pane_placement::ParsedPanePlacement),
    IfShell(ParsedIfShellCommand),
    SourceFile(ParsedSourceFileCommand),
    ListPanesAll(ParsedListPanesAllCommand),
    ListWindowsAll(ParsedListWindowsAllCommand),
    SplitWindow(ParsedSplitWindowCommand),
    MouseResizePane(rmux_proto::PaneTarget),
    CommandPrompt(ParsedCommandPromptCommand),
    ConfirmBefore(ParsedConfirmBeforeCommand),
    ModeTree(super::super::mode_tree_support::ParsedModeTreeCommand),
    Overlay(super::super::overlay_support::ParsedOverlayCommand),
    PromptHistory(ParsedPromptHistoryCommand),
}

impl QueueInvocation {
    pub(super) fn uses_request_hooks(&self) -> bool {
        match self {
            Self::RunShell(command) => !command.request.as_commands || command.request.background,
            Self::Request(_)
            | Self::RestoreWindowSnapshot(_)
            | Self::QuietOnlyIfUnset(_)
            | Self::NewWindow(_)
            | Self::SplitWindow(_) => true,
            _ => false,
        }
    }
}
