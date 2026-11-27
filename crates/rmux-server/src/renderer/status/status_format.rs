use rmux_core::{style::Style, OptionStore, Utf8Config};
use rmux_proto::{OptionName, SessionName};

use crate::format_runtime::RuntimeFormatContext;

use super::super::{format_draw_line, FormattedLine};
use super::status_job_cache_ttl;

pub(super) fn render_explicit_status_format_line(
    session_name: &SessionName,
    options: &OptionStore,
    runtime: &RuntimeFormatContext<'_>,
    base_style: &Style,
    width: usize,
    utf8_config: &Utf8Config,
) -> Option<FormattedLine> {
    if !has_explicit_status_format(options, session_name) {
        return None;
    }

    Some(
        render_status_format_line_at(
            session_name,
            options,
            runtime,
            base_style,
            width,
            utf8_config,
            0,
        )
        .unwrap_or_else(|| format_draw_line("", base_style, width, utf8_config)),
    )
}

pub(super) fn render_status_format_line_at(
    session_name: &SessionName,
    options: &OptionStore,
    runtime: &RuntimeFormatContext<'_>,
    base_style: &Style,
    width: usize,
    utf8_config: &Utf8Config,
    index: usize,
) -> Option<FormattedLine> {
    let index = u32::try_from(index).ok()?;
    let template = options
        .resolve_array_entries(Some(session_name), OptionName::StatusFormat)
        .into_iter()
        .find_map(|(key, value)| (key == index).then_some(value))?;
    if template.is_empty() {
        return None;
    }

    let ttl = status_job_cache_ttl(options, session_name);
    let expanded = render_bar_template(&template, runtime, ttl, false);
    Some(format_draw_line(
        &draw_text(&expanded),
        base_style,
        width,
        utf8_config,
    ))
}

fn has_explicit_status_format(options: &OptionStore, session_name: &SessionName) -> bool {
    options
        .session_value(session_name, OptionName::StatusFormat)
        .is_some()
        || options.global_value(OptionName::StatusFormat).is_some()
}

pub(super) fn render_bar_template(
    template: &str,
    runtime: &RuntimeFormatContext<'_>,
    ttl: std::time::Duration,
    component: bool,
) -> String {
    use rmux_core::formats::{expand_time_tokens, render_status_template};
    let expanded = render_status_template(
        &expand_time_tokens(template),
        &runtime.clone().with_status_jobs(ttl),
    );
    if component {
        expanded
            .chars()
            .map(|c| if c != '\0' && c.is_control() { ' ' } else { c })
            .collect()
    } else {
        expanded
    }
}

/// The draw pass skips control characters. Isolate each literal hash on both
/// sides so it neither opens a style nor combines with a neighbouring hash.
pub(super) fn draw_text(expanded: &str) -> std::borrow::Cow<'_, str> {
    if expanded.contains('\0') {
        expanded.replace('\0', "\0#\0").into()
    } else {
        expanded.into()
    }
}
