use std::cell::OnceCell;

use rmux_core::formats::{FormatVariable, FormatVariables};
use rmux_core::{style::Style, OptionStore, Utf8Config};
use rmux_proto::{OptionName, SessionName};

use crate::format_runtime::RuntimeFormatContext;

use super::super::{format_draw_line, FormattedLine};
use super::{render_status_template_jobs_with_profile, status_job_cache_ttl};

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

    let ttl = status_job_cache_ttl(options, session_name);
    let status_runtime = StatusFormatVariables::new(runtime, session_name, options, ttl);
    let expanded = render_status_template_jobs_with_profile(
        &template,
        &status_runtime,
        status_runtime.profile_for_template(&template),
        ttl,
    );
    Some(format_draw_line(&expanded, base_style, width, utf8_config))
}

fn has_explicit_status_format(options: &OptionStore, session_name: &SessionName) -> bool {
    options
        .session_value(session_name, OptionName::StatusFormat)
        .is_some()
        || options.global_value(OptionName::StatusFormat).is_some()
}

struct StatusFormatVariables<'a, 'runtime> {
    inner: &'a RuntimeFormatContext<'runtime>,
    session_name: &'a SessionName,
    options: &'a OptionStore,
    ttl: std::time::Duration,
    profile: OnceCell<Option<crate::terminal::TerminalProfile>>,
    status_left: OnceCell<Option<String>>,
    status_right: OnceCell<Option<String>>,
}

impl<'a, 'runtime> StatusFormatVariables<'a, 'runtime> {
    fn new(
        inner: &'a RuntimeFormatContext<'runtime>,
        session_name: &'a SessionName,
        options: &'a OptionStore,
        ttl: std::time::Duration,
    ) -> Self {
        Self {
            inner,
            session_name,
            options,
            ttl,
            profile: OnceCell::new(),
            status_left: OnceCell::new(),
            status_right: OnceCell::new(),
        }
    }

    fn profile_for_template(&self, template: &str) -> Option<&crate::terminal::TerminalProfile> {
        if !template.contains("#(") {
            return None;
        }
        self.profile
            .get_or_init(|| self.inner.status_job_profile())
            .as_ref()
    }

    fn status_left(&self) -> Option<String> {
        self.status_left
            .get_or_init(|| self.render_option(OptionName::StatusLeft))
            .clone()
    }

    fn status_right(&self) -> Option<String> {
        self.status_right
            .get_or_init(|| self.render_option(OptionName::StatusRight))
            .clone()
    }

    fn render_option(&self, option: OptionName) -> Option<String> {
        self.options
            .resolve(Some(self.session_name), option)
            .map(|template| {
                render_status_template_jobs_with_profile(
                    template,
                    self.inner,
                    self.profile_for_template(template),
                    self.ttl,
                )
            })
    }
}

impl FormatVariables for StatusFormatVariables<'_, '_> {
    fn format_value(&self, variable: FormatVariable) -> Option<String> {
        self.inner.format_value(variable)
    }

    fn format_loop(
        &self,
        scope: char,
        body: &str,
        current_body: Option<&str>,
        count_only: bool,
    ) -> Option<String> {
        self.inner
            .format_loop(scope, body, current_body, count_only)
    }

    fn format_name_exists(&self, scope: Option<char>, name: &str) -> Option<bool> {
        self.inner.format_name_exists(scope, name)
    }

    fn format_search(&self, options: &str, pattern: &str) -> Option<String> {
        self.inner.format_search(options, pattern)
    }

    fn format_value_by_name(&self, name: &str) -> Option<String> {
        match name {
            "status-left" => self.status_left(),
            "status-right" => self.status_right(),
            _ => self.inner.format_value_by_name(name),
        }
    }
}
