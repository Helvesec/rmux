use super::{CommandTargetMetadata, CommandTargetSpec, TargetFindFlags, TargetFindType};

/// Returns command target metadata for commands currently routed through the parsed queue.
#[must_use]
pub fn command_target_metadata(command_name: &str) -> Option<CommandTargetMetadata> {
    use TargetFindFlags as Flags;
    use TargetFindType as Type;

    match command_name {
        "break-pane" => Some(metadata(
            Some(spec('s', Type::Pane, Flags::NONE)),
            Some(spec('t', Type::Window, Flags::WINDOW_INDEX)),
        )),
        "capture-pane" | "kill-pane" | "paste-buffer" | "pipe-pane" | "resize-pane"
        | "respawn-pane" | "select-pane" | "send-keys" | "split-window" => {
            Some(metadata(None, Some(spec('t', Type::Pane, Flags::NONE))))
        }
        "copy-mode" => Some(metadata(
            Some(spec('s', Type::Pane, Flags::NONE)),
            Some(spec('t', Type::Pane, Flags::NONE)),
        )),
        "display-menu" | "display-message" | "display-popup" | "if-shell" | "show-hooks"
        | "show-options" | "run-shell" | "source-file" => {
            Some(metadata(None, Some(spec('t', Type::Pane, Flags::CANFAIL))))
        }
        "join-pane" | "move-pane" | "swap-pane" => Some(metadata(
            Some(spec('s', Type::Pane, Flags::DEFAULT_MARKED)),
            Some(spec('t', Type::Pane, Flags::NONE)),
        )),
        "kill-window" | "last-pane" | "list-panes" | "next-layout" | "previous-layout"
        | "rename-window" | "respawn-window" | "rotate-window" | "select-layout"
        | "select-window" => Some(metadata(None, Some(spec('t', Type::Window, Flags::NONE)))),
        "link-window" | "move-window" => Some(metadata(
            Some(spec('s', Type::Window, Flags::NONE)),
            Some(spec('t', Type::Window, Flags::WINDOW_INDEX)),
        )),
        "new-window" => Some(metadata(
            None,
            Some(spec('t', Type::Window, Flags::WINDOW_INDEX)),
        )),
        "swap-window" => Some(metadata(
            Some(spec('s', Type::Window, Flags::DEFAULT_MARKED)),
            Some(spec('t', Type::Window, Flags::NONE)),
        )),
        "attach-session" | "display-panes" | "has-session" | "kill-session" | "last-window"
        | "list-windows" | "next-window" | "previous-window" | "rename-session" => {
            Some(metadata(None, Some(spec('t', Type::Session, Flags::NONE))))
        }
        "set-hook" => Some(metadata(None, Some(spec('t', Type::Pane, Flags::CANFAIL)))),
        "set-environment" | "show-environment" => Some(metadata(
            None,
            Some(spec('t', Type::Session, Flags::CANFAIL)),
        )),
        "switch-client" => Some(metadata(
            None,
            Some(spec('t', Type::Session, Flags::PREFER_UNATTACHED)),
        )),
        "set-option" => Some(metadata(
            None,
            Some(spec('t', Type::Session, Flags::CANFAIL)),
        )),
        _ => None,
    }
}

const fn metadata(
    source: Option<CommandTargetSpec>,
    target: Option<CommandTargetSpec>,
) -> CommandTargetMetadata {
    CommandTargetMetadata { source, target }
}

const fn spec(flag: char, find_type: TargetFindType, flags: TargetFindFlags) -> CommandTargetSpec {
    CommandTargetSpec {
        flag,
        find_type,
        flags,
    }
}
