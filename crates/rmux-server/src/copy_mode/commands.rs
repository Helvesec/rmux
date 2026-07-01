//! Copy-mode command dispatch and clear-policy handling.

use rmux_proto::RmuxError;

use super::args::{is_readonly_command, strip_leading_separator};
use super::types::{
    ClearPolicy, CopyModeCommandContext, CopyModeCommandOutcome, JumpKind, ModeKeys,
    SearchDirection,
};
use super::CopyModeState;

impl CopyModeState {
    pub(crate) fn execute_command(
        &mut self,
        command: &str,
        args: &[String],
        context: &CopyModeCommandContext,
    ) -> Result<CopyModeCommandOutcome, RmuxError> {
        let args = strip_leading_separator(args);
        self.mode_keys = context.mode_keys;
        self.wrap_search = context.wrap_search;
        self.word_separators = context.word_separators.clone();
        if self.view_mode && !is_readonly_command(command) {
            return Ok(CopyModeCommandOutcome::nothing());
        }

        let outcome = match command {
            "append-selection" => self.transfer_selection(args, true, false, true),
            "append-selection-and-cancel" => self.transfer_selection(args, true, true, true),
            "back-to-indentation" => {
                self.readonly(Self::cmd_back_to_indentation, ClearPolicy::Always)
            }
            "begin-selection" => {
                if let Some(mouse) = context.mouse {
                    self.move_cursor_to_mouse(mouse.content_x, mouse.content_y);
                    if self
                        .selection
                        .as_ref()
                        .is_some_and(|selection| selection.active)
                    {
                        return Ok(self.finish_policy(
                            CopyModeCommandOutcome::nothing(),
                            ClearPolicy::Always,
                        ));
                    }
                }
                self.begin_selection();
                Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always))
            }
            "bottom-line" => self.readonly(Self::cmd_bottom_line, ClearPolicy::EmacsOnly),
            "cancel" => {
                Ok(self.finish_policy(CopyModeCommandOutcome::cancel(), ClearPolicy::Always))
            }
            "cancel-or-clear-selection" => {
                if self.selection.is_some() {
                    self.selection = None;
                    Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always))
                } else {
                    Ok(self.finish_policy(CopyModeCommandOutcome::cancel(), ClearPolicy::Always))
                }
            }
            "clear-selection" => {
                self.selection = None;
                Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always))
            }
            "copy-end-of-line" => self.transfer_end_of_line(args, false, false),
            "copy-end-of-line-and-cancel" => self.transfer_end_of_line(args, false, true),
            "copy-pipe-end-of-line" => self.transfer_end_of_line(args, true, false),
            "copy-pipe-end-of-line-and-cancel" => self.transfer_end_of_line(args, true, true),
            "copy-line" => self.transfer_line(args, false, false),
            "copy-line-and-cancel" => self.transfer_line(args, false, true),
            "copy-pipe-line" => self.transfer_line(args, true, false),
            "copy-pipe-line-and-cancel" => self.transfer_line(args, true, true),
            "copy-pipe-no-clear" => self.transfer_copy_pipe(args, false, ClearPolicy::Never),
            "copy-pipe" => self.transfer_copy_pipe(args, false, ClearPolicy::Always),
            "copy-pipe-and-cancel" => self.transfer_copy_pipe(args, true, ClearPolicy::Always),
            "copy-selection-no-clear" => {
                self.transfer_copy_selection(args, false, ClearPolicy::Never)
            }
            "copy-selection" => self.transfer_copy_selection(args, false, ClearPolicy::Always),
            "copy-selection-and-cancel" => {
                self.transfer_copy_selection(args, true, ClearPolicy::Always)
            }
            "cursor-down" => self.readonly(Self::cmd_cursor_down, ClearPolicy::EmacsOnly),
            "cursor-down-and-cancel" => {
                let old_y = self.cursor.y;
                self.cmd_cursor_down()?;
                let cancel = self.cursor.y == old_y && self.at_bottom();
                Ok(self.finish_policy(
                    CopyModeCommandOutcome {
                        cancel,
                        transfer: None,
                    },
                    ClearPolicy::Always,
                ))
            }
            "cursor-left" => self.readonly(Self::cmd_cursor_left, ClearPolicy::EmacsOnly),
            "cursor-right" => self.readonly(Self::cmd_cursor_right, ClearPolicy::EmacsOnly),
            "cursor-up" => self.readonly(Self::cmd_cursor_up, ClearPolicy::EmacsOnly),
            "cursor-centre-vertical" => {
                self.readonly(Self::cmd_cursor_centre_vertical, ClearPolicy::EmacsOnly)
            }
            "cursor-centre-horizontal" => {
                self.readonly(Self::cmd_cursor_centre_horizontal, ClearPolicy::EmacsOnly)
            }
            "end-of-line" => self.readonly(Self::cmd_end_of_line, ClearPolicy::EmacsOnly),
            "goto-line" => self.readonly_args(args, ClearPolicy::EmacsOnly, |state, args| {
                let line = args
                    .first()
                    .ok_or_else(|| RmuxError::Server("goto-line expects a line".to_owned()))?;
                state.cmd_goto_line(line)
            }),
            "halfpage-down" => {
                self.readonly_exit_on_scroll(Self::cmd_halfpage_down, ClearPolicy::EmacsOnly)
            }
            "halfpage-down-and-cancel" => {
                self.cmd_halfpage_down()?;
                let cancel = self.at_bottom();
                Ok(self.finish_policy(
                    CopyModeCommandOutcome {
                        cancel,
                        transfer: None,
                    },
                    ClearPolicy::Always,
                ))
            }
            "halfpage-up" => self.readonly(Self::cmd_halfpage_up, ClearPolicy::EmacsOnly),
            "end-of-buffer" | "history-bottom" => {
                self.readonly(Self::cmd_history_bottom, ClearPolicy::EmacsOnly)
            }
            "history-top" | "start-of-buffer" => {
                self.readonly(Self::cmd_history_top, ClearPolicy::EmacsOnly)
            }
            "jump-again" => self.jump_again(false),
            "jump-backward" => self.jump_with_arg(args, JumpKind::Backward),
            "jump-forward" => self.jump_with_arg(args, JumpKind::Forward),
            "jump-reverse" => self.jump_again(true),
            "jump-to-backward" => self.jump_with_arg(args, JumpKind::ToBackward),
            "jump-to-forward" => self.jump_with_arg(args, JumpKind::ToForward),
            "jump-to-mark" => self.readonly(Self::cmd_jump_to_mark, ClearPolicy::Always),
            "next-prompt" => self.readonly_args(args, ClearPolicy::Always, |state, args| {
                let only_after = args.iter().any(|value| value == "-o");
                state.cmd_next_prompt(only_after)
            }),
            "previous-prompt" => self.readonly_args(args, ClearPolicy::Always, |state, args| {
                let only_before = args.iter().any(|value| value == "-o");
                state.cmd_previous_prompt(only_before)
            }),
            "middle-line" => self.readonly(Self::cmd_middle_line, ClearPolicy::EmacsOnly),
            "next-matching-bracket" => {
                self.readonly(Self::cmd_next_matching_bracket, ClearPolicy::Always)
            }
            "next-paragraph" => self.readonly(Self::cmd_next_paragraph, ClearPolicy::EmacsOnly),
            "next-space" => self.readonly(Self::cmd_next_space, ClearPolicy::EmacsOnly),
            "next-space-end" => self.readonly(Self::cmd_next_space_end, ClearPolicy::EmacsOnly),
            "next-word" => self.readonly(Self::cmd_next_word, ClearPolicy::EmacsOnly),
            "next-word-end" => self.readonly(Self::cmd_next_word_end, ClearPolicy::EmacsOnly),
            "other-end" => self.other_end(),
            "page-down" => {
                self.readonly_exit_on_scroll(Self::cmd_page_down, ClearPolicy::EmacsOnly)
            }
            "page-down-and-cancel" => {
                self.cmd_page_down()?;
                let cancel = self.at_bottom();
                Ok(self.finish_policy(
                    CopyModeCommandOutcome {
                        cancel,
                        transfer: None,
                    },
                    ClearPolicy::Always,
                ))
            }
            "page-up" => self.readonly(Self::cmd_page_up, ClearPolicy::EmacsOnly),
            "pipe-no-clear" => self.transfer_pipe(args, false, ClearPolicy::Never),
            "pipe" => self.transfer_pipe(args, false, ClearPolicy::Always),
            "pipe-and-cancel" => self.transfer_pipe(args, true, ClearPolicy::Always),
            "previous-matching-bracket" => {
                self.readonly(Self::cmd_previous_matching_bracket, ClearPolicy::Always)
            }
            "previous-paragraph" => {
                self.readonly(Self::cmd_previous_paragraph, ClearPolicy::EmacsOnly)
            }
            "previous-space" => self.readonly(Self::cmd_previous_space, ClearPolicy::EmacsOnly),
            "previous-word" => self.readonly(Self::cmd_previous_word, ClearPolicy::EmacsOnly),
            "rectangle-on" => {
                self.rectangle = true;
                Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always))
            }
            "rectangle-off" => {
                self.rectangle = false;
                Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always))
            }
            "rectangle-toggle" => {
                self.rectangle = !self.rectangle;
                Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always))
            }
            "refresh-from-pane" => self.readonly_args(args, ClearPolicy::Always, |state, _args| {
                if let Some(screen) = context.refresh_screen.clone() {
                    state.refresh_from_screen(screen);
                }
                Ok(())
            }),
            "scroll-bottom" => self.readonly(Self::cmd_scroll_bottom, ClearPolicy::Always),
            "scroll-down" => {
                self.readonly_exit_on_scroll(Self::cmd_scroll_down, ClearPolicy::EmacsOnly)
            }
            "scroll-down-and-cancel" => {
                self.cmd_scroll_down()?;
                let cancel = self.at_bottom();
                Ok(self.finish_policy(
                    CopyModeCommandOutcome {
                        cancel,
                        transfer: None,
                    },
                    ClearPolicy::Always,
                ))
            }
            "scroll-exit-on" => {
                self.exit_on_scroll = true;
                Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always))
            }
            "scroll-exit-off" => {
                self.exit_on_scroll = false;
                Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always))
            }
            "scroll-exit-toggle" => {
                self.exit_on_scroll = !self.exit_on_scroll;
                Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always))
            }
            "scroll-middle" => self.readonly(Self::cmd_scroll_middle, ClearPolicy::Always),
            "scroll-to-mouse" => {
                self.readonly_args(args, ClearPolicy::EmacsOnly, |state, _args| {
                    if let Some(mouse) = context.mouse {
                        state.scroll_to_mouse(mouse.slider_mpos, mouse.scroll_y);
                    }
                    Ok(())
                })
            }
            "scroll-top" => self.readonly(Self::cmd_scroll_top, ClearPolicy::Always),
            "scroll-up" => self.readonly(Self::cmd_scroll_up, ClearPolicy::EmacsOnly),
            "search-again" => self.search_again(),
            "search-backward" => self.search_with_arg(args, SearchDirection::Backward, false),
            "search-backward-text" => self.search_with_arg(args, SearchDirection::Backward, true),
            "search-backward-incremental" => {
                self.incremental_search(args, SearchDirection::Backward)
            }
            "search-forward" => self.search_with_arg(args, SearchDirection::Forward, false),
            "search-forward-text" => self.search_with_arg(args, SearchDirection::Forward, true),
            "search-forward-incremental" => self.incremental_search(args, SearchDirection::Forward),
            "search-reverse" => self.search_reverse(),
            "select-line" => {
                if let Some(mouse) = context.mouse {
                    self.move_cursor_to_mouse(mouse.content_x, mouse.content_y);
                }
                self.select_line();
                Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always))
            }
            "select-word" => {
                if let Some(mouse) = context.mouse {
                    self.move_cursor_to_mouse(mouse.content_x, mouse.content_y);
                }
                self.select_word();
                Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always))
            }
            "selection-mode" => self.selection_mode(args),
            "set-mark" => self.readonly(Self::cmd_set_mark, ClearPolicy::Always),
            "start-of-line" => self.readonly(Self::cmd_start_of_line, ClearPolicy::EmacsOnly),
            "stop-selection" => {
                if let Some(selection) = &mut self.selection {
                    selection.end = self.cursor;
                    selection.active = false;
                }
                Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Always))
            }
            "toggle-position" => {
                self.show_position = !self.show_position;
                Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), ClearPolicy::Never))
            }
            "top-line" => self.readonly(Self::cmd_top_line, ClearPolicy::EmacsOnly),
            other => Err(RmuxError::Server(format!(
                "unknown copy-mode command: {other}"
            ))),
        }?;

        Ok(outcome)
    }

    fn readonly(
        &mut self,
        command: fn(&mut Self) -> Result<(), RmuxError>,
        clear: ClearPolicy,
    ) -> Result<CopyModeCommandOutcome, RmuxError> {
        command(self)?;
        Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), clear))
    }

    fn readonly_exit_on_scroll(
        &mut self,
        command: fn(&mut Self) -> Result<(), RmuxError>,
        clear: ClearPolicy,
    ) -> Result<CopyModeCommandOutcome, RmuxError> {
        command(self)?;
        let cancel = self.exit_on_scroll && self.at_bottom();
        Ok(self.finish_policy(
            CopyModeCommandOutcome {
                cancel,
                transfer: None,
            },
            clear,
        ))
    }

    fn readonly_args<F>(
        &mut self,
        args: &[String],
        clear: ClearPolicy,
        command: F,
    ) -> Result<CopyModeCommandOutcome, RmuxError>
    where
        F: FnOnce(&mut Self, &[String]) -> Result<(), RmuxError>,
    {
        command(self, args)?;
        Ok(self.finish_policy(CopyModeCommandOutcome::nothing(), clear))
    }

    pub(super) fn finish_policy(
        &mut self,
        outcome: CopyModeCommandOutcome,
        clear: ClearPolicy,
    ) -> CopyModeCommandOutcome {
        match clear {
            ClearPolicy::Always => self.search_highlighted = false,
            ClearPolicy::Never => {}
            ClearPolicy::EmacsOnly if self.mode_keys == ModeKeys::Emacs => {
                self.search_highlighted = false;
            }
            ClearPolicy::EmacsOnly => {}
        }
        outcome
    }
}
