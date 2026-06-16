use rmux_core::{
    format_skip_delimiter, formats::FormatVariables, OptionStore, Session, Utf8Config,
};
use rmux_proto::OptionName;

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};

use super::super::{apply_runtime_style_overlay, format_draw_line, FormattedLine};
use super::{active_format_context, resolved_status_style, sanitize_status_text};

pub(in crate::renderer) fn format_status_message_line(
    session: &Session,
    options: &OptionStore,
    width: usize,
    message: &str,
    command_prompt: bool,
) -> FormattedLine {
    let mut runtime = RuntimeFormatContext::new(active_format_context(session, 0, None, None))
        .with_options(options)
        .with_session(session)
        .with_window(session.active_window_index(), session.window())
        .with_named_value("message", sanitize_status_text(message.to_owned()))
        .with_named_value("command_prompt", if command_prompt { "1" } else { "0" });
    if let Some(pane) = session.window().active_pane() {
        runtime = runtime.with_pane(pane);
    }

    let template = options
        .resolve(Some(session.name()), OptionName::MessageFormat)
        .unwrap_or("#{message}");
    let expanded = render_runtime_template(template, &runtime, true);
    let expanded = expand_format_draw_style_clauses(&expanded, &runtime);
    let style_option = if command_prompt {
        OptionName::MessageCommandStyle
    } else {
        OptionName::MessageStyle
    };
    let base_style = apply_runtime_style_overlay(
        &resolved_status_style(options, session.name()),
        options.resolve(Some(session.name()), style_option),
        &runtime,
    );
    format_draw_line(
        &expanded,
        &base_style,
        width,
        &Utf8Config::from_options(options),
    )
}

fn expand_format_draw_style_clauses<V>(line: &str, variables: &V) -> String
where
    V: FormatVariables + ?Sized,
{
    let bytes = line.as_bytes();
    let mut expanded = String::with_capacity(line.len());
    let mut index = 0_usize;

    while index < bytes.len() {
        if bytes[index] == b'#' && bytes.get(index + 1) == Some(&b'[') {
            if let Some(end_offset) = format_skip_delimiter(&line[index + 2..], b"]") {
                let clause_start = index + 2;
                let clause_end = clause_start + end_offset;
                expanded.push_str("#[");
                expanded.push_str(&render_runtime_template(
                    &line[clause_start..clause_end],
                    variables,
                    false,
                ));
                expanded.push(']');
                index = clause_end + 1;
                continue;
            }
        }

        let Some(ch) = line[index..].chars().next() else {
            break;
        };
        expanded.push(ch);
        index += ch.len_utf8();
    }

    expanded
}
