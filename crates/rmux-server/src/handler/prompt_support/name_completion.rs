//! Command-name completion only; arguments have their own completion rules.

use std::ops::Range;

use rmux_core::command_parser::COMMAND_TABLE;

use super::{byte_index_for_char, ClientPromptState, PromptInputEvent, PromptType};
use crate::renderer::MenuRenderItem;

#[derive(Default)]
pub(super) struct CommandCompletionContext<'a> {
    pub(super) aliases: &'a [String],
    pub(super) menu_height: usize,
}

#[derive(Debug)]
pub(super) struct CommandCompletionMenu {
    range: Range<usize>,
    candidates: Vec<String>,
    // Keep all matches for a later grow; shortcuts and selection index this slice.
    visible: Range<usize>,
    selected: Option<usize>,
}

impl CommandCompletionMenu {
    fn visible_candidates(&self) -> &[String] {
        &self.candidates[self.visible.clone()]
    }

    fn reconcile(&mut self, height: usize) {
        let selected = self.selected.map(|index| self.visible.start + index);
        self.visible = self.candidates.len().saturating_sub(height)..self.candidates.len();
        self.selected = selected
            .filter(|index| self.visible.contains(index))
            .map(|index| index - self.visible.start)
            .or_else(|| (!self.visible.is_empty()).then_some(0));
    }

    pub(super) fn rendered_items(&self) -> Vec<MenuRenderItem> {
        self.visible_candidates()
            .iter()
            .enumerate()
            .map(|(index, name)| MenuRenderItem {
                label: name.clone(),
                shortcut: shortcut(index).map(|key| format!("({key})")),
                selected: self.selected == Some(index),
                separator: false,
            })
            .collect()
    }
}

impl ClientPromptState {
    pub(in crate::handler) fn reconcile_completion_menu(&mut self, height: usize) {
        if height == 0 {
            self.completion_menu = None;
        } else if let Some(menu) = self.completion_menu.as_mut() {
            menu.reconcile(height);
        }
    }

    pub(super) fn complete_command_name(&mut self, context: CommandCompletionContext<'_>) -> bool {
        if self.prompt_type != PromptType::Command {
            return false;
        }
        let start = self.buffer.len() - self.buffer.trim_start_matches(' ').len();
        let end = self.buffer[start..]
            .find(' ')
            .map_or(self.buffer.len(), |offset| start + offset);
        let cursor = byte_index_for_char(&self.buffer, self.cursor);
        if start == end || cursor > end || (start > 0 && cursor <= start) {
            return false;
        }
        let word = &self.buffer[start..end];
        let mut candidates: Vec<String> = COMMAND_TABLE
            .iter()
            .flat_map(|entry| std::iter::once(entry.name).chain(entry.alias))
            .chain(
                context
                    .aliases
                    .iter()
                    .filter_map(|alias| alias.split_once('=').map(|(name, _)| name)),
            )
            .filter(|name| name.starts_with(word))
            .map(str::to_owned)
            .collect();
        candidates.sort_unstable();
        candidates.dedup();
        let Some(first) = candidates.first() else {
            return false;
        };
        if candidates.len() == 1 {
            self.replace_completed_name(start..end, &format!("{first} "));
            return true;
        }
        let mut common = first.clone();
        for candidate in &candidates[1..] {
            let length = common
                .chars()
                .zip(candidate.chars())
                .take_while(|(left, right)| left == right)
                .map(|(ch, _)| ch.len_utf8())
                .sum();
            common.truncate(length);
        }
        if common.len() > word.len() {
            self.replace_completed_name(start..end, &common);
        } else {
            if context.menu_height == 0 {
                return false;
            }
            self.completion_menu = Some(CommandCompletionMenu {
                range: start..end,
                visible: 0..candidates.len(),
                candidates,
                selected: Some(0),
            });
            self.reconcile_completion_menu(context.menu_height);
        }
        true
    }

    fn replace_completed_name(&mut self, range: Range<usize>, replacement: &str) {
        self.cursor = self.buffer[..range.start].chars().count() + replacement.chars().count();
        self.buffer.replace_range(range, replacement);
        self.history_index = 0;
    }

    /// A completion menu owns keys until it closes, including in vi entry mode.
    /// Choosing an item inserts its spelling without submitting the prompt.
    pub(super) fn handle_completion_menu(&mut self, event: &PromptInputEvent) -> bool {
        let Some(mut menu) = self.completion_menu.take() else {
            return false;
        };
        let chosen = match event {
            PromptInputEvent::Char(ch) => menu
                .visible_candidates()
                .iter()
                .enumerate()
                .find_map(|(index, _)| (shortcut(index) == Some(*ch)).then_some(index)),
            PromptInputEvent::Enter => menu.selected,
            _ => None,
        };
        if let Some(index) = chosen {
            let replacement = &menu.visible_candidates()[index];
            self.replace_completed_name(menu.range.clone(), replacement);
            return true;
        }
        let count = menu.visible_candidates().len();
        match event {
            PromptInputEvent::Escape
            | PromptInputEvent::Ctrl('c' | 'g')
            | PromptInputEvent::Enter
            | PromptInputEvent::Char('q') => return true,
            PromptInputEvent::Down | PromptInputEvent::Tab | PromptInputEvent::Char('j') => {
                menu.selected = Some(menu.selected.map_or(0, |index| (index + 1) % count));
            }
            PromptInputEvent::Up | PromptInputEvent::Char('k') => {
                menu.selected = Some(
                    menu.selected
                        .map_or(count - 1, |index| (index + count - 1) % count),
                );
            }
            PromptInputEvent::Home | PromptInputEvent::Char('g') => menu.selected = Some(0),
            PromptInputEvent::End | PromptInputEvent::Char('G') => menu.selected = Some(count - 1),
            _ => {}
        }
        self.completion_menu = Some(menu);
        true
    }
}

fn shortcut(index: usize) -> Option<char> {
    u32::try_from(index)
        .ok()
        .and_then(|value| char::from_digit(value, 10))
}
