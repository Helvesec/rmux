use clap::{ArgAction, Args};
use rmux_core::command_parser::CommandEntry;

use super::*;

pub(crate) fn completion_command() -> clap::Command {
    let mut command = clap::Command::new("rmux")
        .disable_help_subcommand(true)
        .version(env!("CARGO_PKG_VERSION"))
        .arg(
            clap::Arg::new("256-colors")
                .short('2')
                .action(ArgAction::SetTrue),
        )
        .arg(
            clap::Arg::new("control-mode")
                .short('C')
                .action(ArgAction::Count),
        )
        .arg(
            clap::Arg::new("no-fork")
                .short('D')
                .action(ArgAction::SetTrue),
        )
        .arg(
            clap::Arg::new("shell-command")
                .short('c')
                .value_name("shell-command"),
        )
        .arg(
            clap::Arg::new("file")
                .short('f')
                .value_name("file")
                .action(ArgAction::Append),
        )
        .arg(
            clap::Arg::new("login-shell")
                .short('l')
                .action(ArgAction::SetTrue),
        )
        .arg(
            clap::Arg::new("socket-name")
                .short('L')
                .value_name("socket-name")
                .allow_hyphen_values(true),
        )
        .arg(
            clap::Arg::new("no-start-server")
                .short('N')
                .action(ArgAction::SetTrue),
        )
        .arg(
            clap::Arg::new("socket-path")
                .short('S')
                .value_name("socket-path")
                .allow_hyphen_values(true),
        )
        .arg(
            clap::Arg::new("features")
                .short('T')
                .value_name("features")
                .allow_hyphen_values(true)
                .action(ArgAction::Append),
        )
        .arg(clap::Arg::new("utf8").short('u').action(ArgAction::SetTrue))
        .arg(
            clap::Arg::new("verbose")
                .short('v')
                .action(ArgAction::Count),
        );

    for entry in implemented_command_surface() {
        command = command.subcommand(completion_subcommand(entry));
    }
    for alias in documented_cli_aliases() {
        command = command.subcommand(
            completion_typed_subcommand::<ChooseTreeArgs>(alias.alias)
                .about(format!("Alias for {}", alias.expansion)),
        );
    }

    command
}

fn completion_subcommand(entry: &'static CommandEntry) -> clap::Command {
    let mut command = match entry.name {
        "new-session" => completion_typed_subcommand::<NewSessionArgs>(entry.name),
        "start-server" => completion_typed_subcommand::<StartServerArgs>(entry.name),
        "kill-server" => completion_empty_subcommand(entry.name),
        "has-session" => completion_typed_subcommand::<SessionTargetArgs>(entry.name),
        "kill-session" => completion_typed_subcommand::<KillSessionArgs>(entry.name),
        "rename-session" => completion_typed_subcommand::<RenameSessionArgs>(entry.name),
        "server-access" => completion_typed_subcommand::<ServerAccessArgs>(entry.name),
        "lock-server" => completion_empty_subcommand(entry.name),
        "lock-session" => completion_typed_subcommand::<SessionTargetArgs>(entry.name),
        "lock-client" => completion_typed_subcommand::<ClientTargetArgs>(entry.name),
        "new-window" => completion_typed_subcommand::<NewWindowArgs>(entry.name),
        "kill-window" => completion_typed_subcommand::<KillWindowArgs>(entry.name),
        "select-window" => completion_typed_subcommand::<SelectWindowArgs>(entry.name),
        "rename-window" => completion_typed_subcommand::<RenameWindowArgs>(entry.name),
        "next-window" | "previous-window" => {
            completion_typed_subcommand::<AlertSessionTargetArgs>(entry.name)
        }
        "last-window" => completion_typed_subcommand::<SessionTargetArgs>(entry.name),
        "list-sessions" => completion_typed_subcommand::<ListSessionsArgs>(entry.name),
        "list-windows" => completion_typed_subcommand::<ListWindowsArgs>(entry.name),
        "move-window" => completion_typed_subcommand::<MoveWindowArgs>(entry.name),
        "swap-window" => completion_typed_subcommand::<SwapWindowArgs>(entry.name),
        "rotate-window" => completion_typed_subcommand::<RotateWindowArgs>(entry.name),
        "resize-window" => completion_typed_subcommand::<ResizeWindowArgs>(entry.name),
        "respawn-window" => completion_typed_subcommand::<RespawnWindowArgs>(entry.name),
        "split-window" => completion_typed_subcommand::<SplitWindowArgs>(entry.name),
        "swap-pane" => completion_typed_subcommand::<SwapPaneArgs>(entry.name),
        "last-pane" => completion_typed_subcommand::<LastPaneArgs>(entry.name),
        "join-pane" | "move-pane" => completion_typed_subcommand::<JoinPaneArgs>(entry.name),
        "break-pane" => completion_typed_subcommand::<BreakPaneArgs>(entry.name),
        "pipe-pane" => completion_typed_subcommand::<PipePaneArgs>(entry.name),
        "respawn-pane" => completion_typed_subcommand::<RespawnPaneArgs>(entry.name),
        "kill-pane" => completion_typed_subcommand::<PaneTargetArgs>(entry.name),
        "select-layout" => completion_typed_subcommand::<SelectLayoutArgs>(entry.name),
        "next-layout" | "previous-layout" => {
            completion_typed_subcommand::<WindowTargetArgs>(entry.name)
        }
        "resize-pane" => completion_typed_subcommand::<ResizePaneArgs>(entry.name),
        "display-panes" => completion_typed_subcommand::<DisplayPanesArgs>(entry.name),
        "list-panes" => completion_typed_subcommand::<ListPanesArgs>(entry.name),
        "select-pane" => completion_typed_subcommand::<SelectPaneArgs>(entry.name),
        "copy-mode" => completion_typed_subcommand::<CopyModeArgs>(entry.name),
        "clock-mode" => completion_typed_subcommand::<ClockModeArgs>(entry.name),
        "wait-pane" => completion_typed_subcommand::<WaitPaneArgs>(entry.name),
        "pane-snapshot" => completion_typed_subcommand::<PaneSnapshotArgs>(entry.name),
        "stream-pane" => completion_typed_subcommand::<StreamPaneArgs>(entry.name),
        "collect-pane-output" => completion_typed_subcommand::<CollectPaneOutputArgs>(entry.name),
        "locator" => completion_typed_subcommand::<LocatorArgs>(entry.name),
        "expect-pane" => completion_typed_subcommand::<ExpectPaneArgs>(entry.name),
        "find-panes" => completion_typed_subcommand::<FindPanesArgs>(entry.name),
        "find-sessions" => completion_typed_subcommand::<FindSessionsArgs>(entry.name),
        "broadcast-keys" => completion_typed_subcommand::<BroadcastKeysArgs>(entry.name),
        "with-session" => completion_typed_subcommand::<WithSessionArgs>(entry.name),
        "send-keys" => completion_typed_subcommand::<SendKeysArgs>(entry.name),
        "bind-key" => completion_typed_subcommand::<BindKeyArgs>(entry.name),
        "unbind-key" => completion_typed_subcommand::<UnbindKeyArgs>(entry.name),
        "list-commands" => completion_typed_subcommand::<ListCommandsArgs>(entry.name),
        "list-keys" => completion_typed_subcommand::<ListKeysArgs>(entry.name),
        "send-prefix" => completion_typed_subcommand::<SendPrefixArgs>(entry.name),
        "attach-session" => completion_typed_subcommand::<AttachSessionArgs>(entry.name),
        "refresh-client" => completion_typed_subcommand::<RefreshClientArgs>(entry.name),
        "list-clients" => completion_typed_subcommand::<ListClientsArgs>(entry.name),
        "switch-client" => completion_typed_subcommand::<SwitchClientArgs>(entry.name),
        "detach-client" => completion_typed_subcommand::<DetachClientArgs>(entry.name),
        "suspend-client" => completion_typed_subcommand::<SuspendClientArgs>(entry.name),
        "set-option" | "set-window-option" => {
            completion_typed_subcommand::<SetOptionArgs>(entry.name)
        }
        "set-environment" => completion_typed_subcommand::<SetEnvironmentArgs>(entry.name),
        "show-options" | "show-window-options" => {
            completion_typed_subcommand::<ShowOptionsArgs>(entry.name)
        }
        "show-environment" => completion_typed_subcommand::<ShowEnvironmentArgs>(entry.name),
        "set-hook" => completion_typed_subcommand::<SetHookArgs>(entry.name),
        "show-hooks" => completion_typed_subcommand::<ShowHooksArgs>(entry.name),
        "set-buffer" => completion_typed_subcommand::<SetBufferArgs>(entry.name),
        "show-buffer" => completion_typed_subcommand::<ShowBufferArgs>(entry.name),
        "paste-buffer" => completion_typed_subcommand::<PasteBufferArgs>(entry.name),
        "list-buffers" => completion_typed_subcommand::<ListBuffersArgs>(entry.name),
        "delete-buffer" => completion_typed_subcommand::<DeleteBufferArgs>(entry.name),
        "load-buffer" => completion_typed_subcommand::<LoadBufferArgs>(entry.name),
        "save-buffer" => completion_typed_subcommand::<SaveBufferArgs>(entry.name),
        "capture-pane" => completion_typed_subcommand::<CapturePaneArgs>(entry.name),
        "clear-history" => completion_typed_subcommand::<ClearHistoryArgs>(entry.name),
        "display-message" => completion_typed_subcommand::<DisplayMessageArgs>(entry.name),
        "show-messages" => completion_typed_subcommand::<ShowMessagesArgs>(entry.name),
        "run-shell" => completion_typed_subcommand::<RunShellArgs>(entry.name),
        "source-file" => completion_typed_subcommand::<SourceFileArgs>(entry.name),
        "if-shell" => completion_typed_subcommand::<IfShellArgs>(entry.name),
        "wait-for" => completion_typed_subcommand::<WaitForArgs>(entry.name),
        "web-share" => completion_typed_subcommand::<WebShareArgs>(entry.name),
        "command-prompt" => completion_typed_subcommand::<PromptArgs>(entry.name),
        "confirm-before" => completion_typed_subcommand::<ConfirmBeforeArgs>(entry.name),
        "find-window" => completion_typed_subcommand::<FindWindowArgs>(entry.name),
        "link-window" => completion_typed_subcommand::<LinkWindowArgs>(entry.name),
        "unlink-window" => completion_typed_subcommand::<UnlinkWindowArgs>(entry.name),
        "choose-tree" => completion_typed_subcommand::<ChooseTreeArgs>(entry.name),
        "choose-buffer" => completion_typed_subcommand::<ChooseBufferArgs>(entry.name),
        "choose-client" => completion_typed_subcommand::<ChooseClientArgs>(entry.name),
        "customize-mode" => completion_typed_subcommand::<CustomizeModeArgs>(entry.name),
        "display-menu" => completion_typed_subcommand::<DisplayMenuArgs>(entry.name),
        "display-popup" => completion_typed_subcommand::<DisplayPopupArgs>(entry.name),
        "clear-prompt-history" | "show-prompt-history" => {
            completion_typed_subcommand::<PromptHistoryArgs>(entry.name)
        }
        "capabilities" => completion_empty_subcommand(entry.name)
            .arg(
                clap::Arg::new("human")
                    .long("human")
                    .action(ArgAction::SetTrue),
            )
            .arg(
                clap::Arg::new("json")
                    .long("json")
                    .action(ArgAction::SetTrue),
            ),
        "claude" => completion_empty_subcommand(entry.name).arg(
            clap::Arg::new("claude-args")
                .num_args(0..)
                .allow_hyphen_values(true)
                .trailing_var_arg(true),
        ),
        "doctor" => completion_empty_subcommand(entry.name).arg(
            clap::Arg::new("check")
                .value_name("tmux-dropin")
                .value_parser(["tmux-dropin"]),
        ),
        "setup" => completion_empty_subcommand(entry.name).arg(
            clap::Arg::new("action")
                .value_name("tmux-shim")
                .value_parser(["tmux-shim"]),
        ),
        missing => panic!("completion tree missing command mapping for {missing}"),
    };
    if let Some(alias) = entry.alias {
        command = command.visible_alias(alias);
    }
    command
}

fn completion_typed_subcommand<T>(name: &'static str) -> clap::Command
where
    T: Args,
{
    T::augment_args(completion_empty_subcommand(name))
}

fn completion_empty_subcommand(name: &'static str) -> clap::Command {
    clap::Command::new(name)
        .no_binary_name(true)
        .disable_help_flag(true)
        .disable_help_subcommand(true)
        .arg(
            clap::Arg::new("help")
                .long("help")
                .action(ArgAction::Help)
                .help("Print help"),
        )
}
