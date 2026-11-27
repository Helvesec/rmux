use tokio::sync::oneshot;

use super::scripting_support::QueueExecutionContext;
use super::RequesterOrigin;
use crate::renderer::RenderedPrompt;

mod events;
mod history;
mod history_file;
mod input;
mod lifecycle;
mod name_completion;
use name_completion::CommandCompletionMenu;
mod render;
mod substitution;
mod types;
mod vi;

pub(super) use self::history::PromptHistoryStore;
pub(super) use self::input::{decode_prompt_key, PromptInputEvent};
pub(super) use self::substitution::substitute_prompt_template;
pub(super) use self::types::{
    CommandPromptPlan, ConfirmBeforePlan, PromptField, PromptQueueResult, PromptStartOutcome,
    PromptType,
};
use self::vi::{PromptEditMode, StatusKeys, ViWordMotion};

pub(super) const PROMPT_FLAG_SINGLE: u8 = 0x01;
pub(super) const PROMPT_FLAG_NUMERIC: u8 = 0x02;
pub(super) const PROMPT_FLAG_INCREMENTAL: u8 = 0x04;
pub(super) const PROMPT_FLAG_KEY: u8 = 0x10;
pub(super) const PROMPT_FLAG_BSPACE_EXIT: u8 = 0x80;

#[derive(Debug)]
enum PromptCompletion {
    Foreground(oneshot::Sender<PromptQueueResult>),
    Background,
}

#[derive(Debug)]
enum PromptKind {
    Command {
        template: String,
        format_values: Vec<(String, String)>,
    },
    Confirm {
        template: String,
        format_values: Vec<(String, String)>,
        confirm_key: char,
        default_yes: bool,
    },
}

#[derive(Debug)]
pub(super) struct ClientPromptState {
    fields: Vec<PromptField>,
    current: usize,
    responses: Vec<String>,
    prompt: String,
    buffer: String,
    cursor: usize,
    last_input: String,
    saved: String,
    history_index: usize,
    pre_history_buffer: Option<String>,
    flags: u8,
    prompt_type: PromptType,
    origin: RequesterOrigin,
    context: QueueExecutionContext,
    kind: PromptKind,
    completion: PromptCompletion,
    /// Entry or command half of the vi prompt; only ever left `Entry` when
    /// `status-keys` is emacs, since nothing else enters command mode.
    edit_mode: PromptEditMode,
    completion_menu: Option<CommandCompletionMenu>,
}

impl ClientPromptState {
    pub(in crate::handler) fn rename_session_targets(
        &mut self,
        old_name: &rmux_proto::SessionName,
        new_name: &rmux_proto::SessionName,
    ) {
        self.context.rename_session_targets(old_name, new_name);
    }

    fn new_command(plan: CommandPromptPlan, completion: PromptCompletion) -> Self {
        let first = plan.fields.first().cloned().unwrap_or(PromptField {
            prompt: ":".to_owned(),
            input: String::new(),
        });
        let buffer = if (plan.flags & PROMPT_FLAG_INCREMENTAL) != 0 {
            String::new()
        } else {
            first.input.clone()
        };

        Self {
            fields: plan.fields,
            current: 0,
            responses: Vec::new(),
            prompt: first.prompt,
            buffer,
            cursor: if (plan.flags & PROMPT_FLAG_INCREMENTAL) != 0 {
                0
            } else {
                first.input.chars().count()
            },
            last_input: first.input,
            saved: String::new(),
            history_index: 0,
            pre_history_buffer: None,
            flags: plan.flags,
            prompt_type: plan.prompt_type,
            origin: plan.origin,
            context: plan.context,
            kind: PromptKind::Command {
                template: plan.template,
                format_values: plan.format_values,
            },
            completion,
            edit_mode: PromptEditMode::Entry,
            completion_menu: None,
        }
    }

    fn new_confirm(plan: ConfirmBeforePlan, completion: PromptCompletion) -> Self {
        Self {
            fields: vec![PromptField {
                prompt: plan.prompt.clone(),
                input: String::new(),
            }],
            current: 0,
            responses: Vec::new(),
            prompt: plan.prompt,
            buffer: String::new(),
            cursor: 0,
            last_input: String::new(),
            saved: String::new(),
            history_index: 0,
            pre_history_buffer: None,
            flags: PROMPT_FLAG_SINGLE,
            prompt_type: PromptType::Command,
            origin: plan.origin,
            context: plan.context,
            kind: PromptKind::Confirm {
                template: plan.template,
                format_values: plan.format_values,
                confirm_key: plan.confirm_key,
                default_yes: plan.default_yes,
            },
            completion,
            edit_mode: PromptEditMode::Entry,
            completion_menu: None,
        }
    }

    fn apply_field(&mut self, field: &PromptField) {
        self.prompt = field.prompt.clone();
        self.last_input = field.input.clone();
        if (self.flags & PROMPT_FLAG_INCREMENTAL) != 0 {
            self.buffer.clear();
            self.cursor = 0;
        } else {
            self.buffer = field.input.clone();
            self.cursor = self.buffer.chars().count();
        }
        self.history_index = 0;
        self.pre_history_buffer = None;
        self.edit_mode = PromptEditMode::Entry;
        self.completion_menu = None;
    }

    fn submit_response(&mut self, response: String) -> Option<PromptFinalizeKind> {
        self.responses.push(response);
        self.current += 1;
        if let Some(field) = self.fields.get(self.current).cloned() {
            self.apply_field(&field);
            None
        } else {
            Some(PromptFinalizeKind::Command {
                responses: self.responses.clone(),
            })
        }
    }

    fn current_command_dispatch(&self, responses: Vec<String>) -> Option<PromptDispatch> {
        let PromptKind::Command {
            template,
            format_values,
        } = &self.kind
        else {
            return None;
        };

        Some(PromptDispatch {
            origin: self.origin.clone(),
            context: self.context.clone(),
            template: template.clone(),
            format_values: format_values.clone(),
            responses,
        })
    }

    fn initial_incremental_dispatch(&self) -> Option<PromptDispatch> {
        ((self.flags & PROMPT_FLAG_INCREMENTAL) != 0)
            .then(|| self.current_command_dispatch(vec!["=".to_owned()]))
            .flatten()
    }

    pub(super) fn rendered_prompt(&self) -> RenderedPrompt {
        RenderedPrompt {
            prompt: self.prompt.clone(),
            input: self.buffer.clone(),
            cursor: self.cursor,
            // tmux opens command-prompt in PROMPT_ENTRY mode and flips to
            // PROMPT_COMMAND after Escape in vi-style editing, and publishes
            // that mode to its prompt style, its prompt cursor style and its
            // `#{command_prompt}` format (status.c:882-911). Here the field
            // drives the two RMUX readers of it: `message-command-style` in
            // renderer/status/prompt.rs and CursorScope::CommandPrompt in
            // handler_attach.rs. RMUX still feeds `#{command_prompt}` from the
            // transient-message path, which this field does not reach.
            command_prompt: self.edit_mode == PromptEditMode::Command,
            completion_menu: self
                .completion_menu
                .as_ref()
                .map(|menu| menu.rendered_items())
                .unwrap_or_default(),
        }
    }

    fn edit_mode(&self) -> PromptEditMode {
        self.edit_mode
    }

    fn edit_mode_mut(&mut self) -> &mut PromptEditMode {
        &mut self.edit_mode
    }

    /// Moves the cursor for one vi word motion. tmux reaches `changed:` for
    /// every one of them, so the prompt always repaints afterwards.
    fn apply_vi_word_motion(&mut self, motion: ViWordMotion, word_separators: &str) {
        self.cursor = motion.apply(&self.buffer, self.cursor, word_separators);
    }

    /// tmux steps the cursor back one character when Escape enters command
    /// mode, leaving it on the last character typed (status.c:1071-1073).
    fn enter_command_mode_cursor(&mut self) {
        if self.cursor != 0 {
            self.cursor -= 1;
        }
    }

    fn buffer_string(&self) -> String {
        self.buffer.clone()
    }

    fn push_char(&mut self, ch: char) {
        let byte = byte_index_for_char(&self.buffer, self.cursor);
        self.buffer.insert(byte, ch);
        self.cursor += 1;
        self.history_index = 0;
    }

    fn insert_batched_text(&mut self, text: &str, status_keys: StatusKeys) -> bool {
        const PER_EVENT_FLAGS: u8 =
            PROMPT_FLAG_SINGLE | PROMPT_FLAG_NUMERIC | PROMPT_FLAG_INCREMENTAL | PROMPT_FLAG_KEY;
        if text.is_empty() || self.flags & PER_EVENT_FLAGS != 0 || self.completion_menu.is_some() {
            return false;
        }
        // In vi command mode a run of printable bytes is a run of editing
        // commands, not literal text: refuse the fast path so the caller
        // replays the same bytes as individual prompt events.
        if status_keys == StatusKeys::Vi && self.edit_mode == PromptEditMode::Command {
            return false;
        }

        let byte = byte_index_for_char(&self.buffer, self.cursor);
        self.buffer.insert_str(byte, text);
        self.cursor += text.chars().count();
        self.history_index = 0;
        true
    }

    fn delete_left(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let end = byte_index_for_char(&self.buffer, self.cursor);
        self.cursor -= 1;
        let start = byte_index_for_char(&self.buffer, self.cursor);
        self.buffer.drain(start..end);
        self.history_index = 0;
        true
    }

    fn delete_at_cursor(&mut self) -> bool {
        let start = byte_index_for_char(&self.buffer, self.cursor);
        if start == self.buffer.len() {
            return false;
        }
        let end = next_char_boundary(&self.buffer, start);
        self.buffer.drain(start..end);
        self.history_index = 0;
        true
    }

    fn move_left(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        true
    }

    fn move_right(&mut self) -> bool {
        if self.cursor >= self.buffer.chars().count() {
            return false;
        }
        self.cursor += 1;
        true
    }

    fn move_home(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor = 0;
        true
    }

    fn move_end(&mut self) -> bool {
        let end = self.buffer.chars().count();
        if self.cursor == end {
            return false;
        }
        self.cursor = end;
        true
    }

    fn clear_buffer(&mut self) -> bool {
        if self.buffer.is_empty() {
            return false;
        }
        self.buffer.clear();
        self.cursor = 0;
        self.history_index = 0;
        true
    }

    fn delete_to_end(&mut self) -> bool {
        let start = byte_index_for_char(&self.buffer, self.cursor);
        if start == self.buffer.len() {
            return false;
        }
        self.buffer.truncate(start);
        self.history_index = 0;
        true
    }

    fn delete_word_left(&mut self, separators: &str) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let chars = self.buffer.chars().collect::<Vec<_>>();
        let mut index = self.cursor;

        while index > 0 && chars[index - 1].is_whitespace() {
            index -= 1;
        }
        let separator_word = index > 0 && separators.contains(chars[index - 1]);
        while index > 0 {
            let ch = chars[index - 1];
            if ch.is_whitespace() || separator_word != separators.contains(ch) {
                break;
            }
            index -= 1;
        }

        let start = byte_index_for_char(&self.buffer, index);
        let end = byte_index_for_char(&self.buffer, self.cursor);
        self.saved = self.buffer[start..end].to_owned();
        self.buffer.drain(start..end);
        self.cursor = index;
        self.history_index = 0;
        true
    }

    fn paste_saved(&mut self) -> bool {
        if self.saved.is_empty() {
            return false;
        }
        let byte = byte_index_for_char(&self.buffer, self.cursor);
        self.buffer.insert_str(byte, &self.saved);
        self.cursor += self.saved.chars().count();
        self.history_index = 0;
        true
    }

    fn set_history(&mut self, value: String) {
        self.buffer = value;
        self.cursor = self.buffer.chars().count();
    }

    fn into_finished(self, kind: PromptFinalizeKind) -> FinishedPrompt {
        let kind = match (self.kind, kind) {
            (_, PromptFinalizeKind::Cancel) => FinishedPromptKind::Cancel,
            (
                PromptKind::Command {
                    template,
                    format_values,
                },
                PromptFinalizeKind::Command { responses },
            ) => FinishedPromptKind::Command {
                template,
                format_values,
                responses,
            },
            (
                PromptKind::Confirm {
                    template,
                    format_values,
                    ..
                },
                PromptFinalizeKind::Confirm { accepted: true },
            ) => FinishedPromptKind::Command {
                template,
                format_values,
                responses: Vec::new(),
            },
            (PromptKind::Confirm { .. }, PromptFinalizeKind::Confirm { accepted: false }) => {
                FinishedPromptKind::Cancel
            }
            _ => FinishedPromptKind::Cancel,
        };

        FinishedPrompt {
            origin: self.origin,
            context: self.context,
            completion: self.completion,
            kind,
        }
    }
}

#[derive(Debug, Clone)]
enum PromptFinalizeKind {
    Cancel,
    Command { responses: Vec<String> },
    Confirm { accepted: bool },
}

#[derive(Debug)]
struct PromptAction {
    refresh: bool,
    dispatch: Option<PromptDispatch>,
    finalize: Option<PromptFinalizeKind>,
}

impl PromptAction {
    const fn none() -> Self {
        Self {
            refresh: false,
            dispatch: None,
            finalize: None,
        }
    }
}

#[derive(Debug)]
enum FinishedPromptKind {
    Cancel,
    Command {
        template: String,
        format_values: Vec<(String, String)>,
        responses: Vec<String>,
    },
}

#[derive(Debug)]
struct FinishedPrompt {
    origin: RequesterOrigin,
    context: QueueExecutionContext,
    completion: PromptCompletion,
    kind: FinishedPromptKind,
}

fn prompt_accept_should_dismiss_mode_tree(finished: &FinishedPrompt) -> bool {
    matches!(
        &finished.kind,
        FinishedPromptKind::Command { template, .. }
            if template.trim_start().starts_with("kill-pane")
    )
}

#[derive(Debug, Clone)]
struct PromptDispatch {
    origin: RequesterOrigin,
    context: QueueExecutionContext,
    template: String,
    format_values: Vec<(String, String)>,
    responses: Vec<String>,
}

fn byte_index_for_char(value: &str, index: usize) -> usize {
    value
        .char_indices()
        .nth(index)
        .map(|(byte, _)| byte)
        .unwrap_or(value.len())
}

fn next_char_boundary(value: &str, index: usize) -> usize {
    value[index..]
        .char_indices()
        .nth(1)
        .map(|(next, _)| index + next)
        .unwrap_or(value.len())
}
