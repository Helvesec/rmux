use rmux_core::{
    text_width as tmux_text_width, truncate_right_to_width as tmux_truncate_right_to_width,
    truncate_to_width as tmux_truncate_to_width, OptionStore, Session, Utf8Config,
};
use rmux_proto::OptionName;

use super::super::{apply_style_overlay, RenderedPrompt};
use super::resolved_status_style;
use super::runs::{
    push_spaces, push_status_run, sanitize_status_text, status_runs_width, StatusRun, StatusStyle,
};

pub(super) struct PromptStatusLayout {
    pub(super) runs: Vec<StatusRun>,
    pub(super) cursor_x: u16,
}

pub(in crate::renderer) fn prompt_status_runs(
    session: &Session,
    options: &OptionStore,
    columns: u16,
    prompt: &RenderedPrompt,
) -> Vec<StatusRun> {
    prompt_status_layout(session, options, columns, prompt).runs
}

pub(super) fn prompt_status_layout(
    session: &Session,
    options: &OptionStore,
    columns: u16,
    prompt: &RenderedPrompt,
) -> PromptStatusLayout {
    let width = usize::from(columns);
    let utf8_config = Utf8Config::from_options(options);
    let style = prompt_style(session, options, prompt.command_prompt);
    let prompt_text =
        sanitize_status_text(tmux_truncate_to_width(&prompt.prompt, width, &utf8_config));
    let prompt_width = tmux_text_width(&prompt_text, &utf8_config);
    let available = width.saturating_sub(prompt_width);
    let input = prompt_visible_input(&prompt.input, prompt.cursor, available, &utf8_config);
    let input_text = sanitize_status_text(input.text);

    let mut runs = Vec::new();
    push_status_run(&mut runs, prompt_text, style.clone());
    push_status_run(&mut runs, input_text, style.clone());
    let rendered = status_runs_width(&runs, &utf8_config);
    push_spaces(&mut runs, width.saturating_sub(rendered), style);

    let cursor_x = prompt_width.saturating_add(input.cursor_x);
    PromptStatusLayout {
        runs,
        cursor_x: u16::try_from(cursor_x.min(width.saturating_sub(1))).unwrap_or(u16::MAX),
    }
}

fn prompt_style(session: &Session, options: &OptionStore, command_prompt: bool) -> StatusStyle {
    let style_option = if command_prompt {
        OptionName::MessageCommandStyle
    } else {
        OptionName::MessageStyle
    };
    apply_style_overlay(
        &resolved_status_style(options, session.name()),
        options.resolve(Some(session.name()), style_option),
    )
}

struct PromptVisibleInput {
    text: String,
    cursor_x: usize,
}

fn prompt_visible_input(
    input: &str,
    cursor: usize,
    width: usize,
    utf8_config: &Utf8Config,
) -> PromptVisibleInput {
    if width == 0 {
        return PromptVisibleInput {
            text: String::new(),
            cursor_x: 0,
        };
    }

    let input_width = width.saturating_sub(1);
    let cursor = cursor.min(input.chars().count());
    let cursor_byte = byte_index_for_char(input, cursor);
    let visible_prefix =
        tmux_truncate_right_to_width(&input[..cursor_byte], input_width, utf8_config);
    let cursor_x = tmux_text_width(&visible_prefix, utf8_config).min(input_width);
    let mut visible_input = visible_prefix;
    visible_input.push_str(&input[cursor_byte..]);
    let text = tmux_truncate_to_width(&visible_input, input_width, utf8_config);
    PromptVisibleInput { text, cursor_x }
}

fn byte_index_for_char(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map_or(value.len(), |(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_visible_input_reserves_a_cursor_cell_at_the_tail() {
        let visible = prompt_visible_input("0123456789", 10, 6, &Utf8Config::default());

        assert_eq!(visible.text, "56789");
        assert_eq!(visible.cursor_x, 5);
    }

    #[test]
    fn prompt_visible_input_keeps_cursor_column_for_middle_edits() {
        let visible = prompt_visible_input("abcdef", 3, 8, &Utf8Config::default());

        assert_eq!(visible.text, "abcdef");
        assert_eq!(visible.cursor_x, 3);
    }

    #[test]
    fn prompt_visible_input_keeps_wide_tail_cells_whole() {
        let visible = prompt_visible_input("A表B", 3, 4, &Utf8Config::default());

        assert_eq!(visible.text, "表B");
        assert_eq!(visible.cursor_x, 3);
    }

    #[test]
    fn prompt_visible_input_keeps_zwj_tail_cells_whole() {
        let visible = prompt_visible_input("A👨\u{200D}👩B", 5, 4, &Utf8Config::default());

        assert_eq!(visible.text, "👨\u{200D}👩B");
        assert_eq!(visible.cursor_x, 3);
    }
}
