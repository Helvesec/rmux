use crate::copy_mode::ModeKeys;

use super::super::prompt_support::PromptInputEvent;
use super::search::AttachedCopyModeSearchDirection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AttachedCopyModeInputAction {
    Command(&'static str),
    Search(AttachedCopyModeSearchDirection),
    Ignore,
}

pub(super) fn attached_copy_mode_input_action(
    mode_keys: ModeKeys,
    event: &PromptInputEvent,
) -> AttachedCopyModeInputAction {
    if mode_keys == ModeKeys::Vi {
        match event {
            PromptInputEvent::Char('/') => {
                return AttachedCopyModeInputAction::Search(
                    AttachedCopyModeSearchDirection::Forward,
                );
            }
            PromptInputEvent::KeyName(name) if name == "/" => {
                return AttachedCopyModeInputAction::Search(
                    AttachedCopyModeSearchDirection::Forward,
                );
            }
            PromptInputEvent::Char('?') => {
                return AttachedCopyModeInputAction::Search(
                    AttachedCopyModeSearchDirection::Backward,
                );
            }
            PromptInputEvent::KeyName(name) if name == "?" => {
                return AttachedCopyModeInputAction::Search(
                    AttachedCopyModeSearchDirection::Backward,
                );
            }
            _ => {}
        }
    }

    let command = match event {
        PromptInputEvent::Char(' ') => "begin-selection",
        PromptInputEvent::KeyName(name) if name == "Space" || name == " " => "begin-selection",
        PromptInputEvent::Enter => "copy-selection-no-clear",
        PromptInputEvent::Right => "cursor-right",
        PromptInputEvent::Left => "cursor-left",
        PromptInputEvent::Down => "cursor-down",
        PromptInputEvent::Up => "cursor-up",
        PromptInputEvent::Char('q') | PromptInputEvent::Escape => "cancel",
        PromptInputEvent::KeyName(name) if name == "q" => "cancel",
        _ => return AttachedCopyModeInputAction::Ignore,
    };
    AttachedCopyModeInputAction::Command(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emacs_arrow_keys_route_to_copy_mode_motion_commands() {
        assert_eq!(
            attached_copy_mode_input_action(ModeKeys::Emacs, &PromptInputEvent::Right),
            AttachedCopyModeInputAction::Command("cursor-right")
        );
        assert_eq!(
            attached_copy_mode_input_action(ModeKeys::Emacs, &PromptInputEvent::Left),
            AttachedCopyModeInputAction::Command("cursor-left")
        );
        assert_eq!(
            attached_copy_mode_input_action(ModeKeys::Emacs, &PromptInputEvent::Down),
            AttachedCopyModeInputAction::Command("cursor-down")
        );
        assert_eq!(
            attached_copy_mode_input_action(ModeKeys::Emacs, &PromptInputEvent::Up),
            AttachedCopyModeInputAction::Command("cursor-up")
        );
    }

    #[test]
    fn vi_search_keys_still_start_copy_mode_search() {
        assert_eq!(
            attached_copy_mode_input_action(ModeKeys::Vi, &PromptInputEvent::Char('/')),
            AttachedCopyModeInputAction::Search(AttachedCopyModeSearchDirection::Forward)
        );
        assert_eq!(
            attached_copy_mode_input_action(ModeKeys::Vi, &PromptInputEvent::Char('?')),
            AttachedCopyModeInputAction::Search(AttachedCopyModeSearchDirection::Backward)
        );
    }
}
