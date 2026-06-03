use rmux_core::{style::Style, OptionStore, Utf8Config};
use rmux_proto::{OptionName, SessionName};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};

use super::super::{format_draw_line, FormattedLine};

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

    let template = options
        .resolve_array_values(Some(session_name), OptionName::StatusFormat)
        .into_iter()
        .next()?;
    if template.is_empty() {
        return None;
    }

    let expanded = render_runtime_template(&template, runtime, true);
    Some(format_draw_line(&expanded, base_style, width, utf8_config))
}

fn has_explicit_status_format(options: &OptionStore, session_name: &SessionName) -> bool {
    options
        .session_value(session_name, OptionName::StatusFormat)
        .is_some()
        || options.global_value(OptionName::StatusFormat).is_some()
}
