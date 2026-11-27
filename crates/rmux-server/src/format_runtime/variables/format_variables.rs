//! The `FormatVariables` surface and the tiered by-name lookup.

use rmux_core::formats::{
    format_variable_descriptor, FormatVariable, FormatVariableDescriptor, FormatVariables,
};

use crate::hook_runtime::current_hook_format_value;

use super::super::render_runtime_template;
use super::{RuntimeFormatContext, RuntimeValue};

impl FormatVariables for RuntimeFormatContext<'_> {
    fn format_job(&self, source: &str, command: &str) -> Option<String> {
        self.job_output(source, command)
    }

    fn format_value(&self, variable: FormatVariable) -> Option<String> {
        if let Some(value) = self.runtime_format_value(variable) {
            return Some(value);
        }
        self.base.format_value(variable)
    }

    fn format_loop(
        &self,
        scope: char,
        body: &str,
        current_body: Option<&str>,
        count_only: bool,
    ) -> Option<String> {
        match scope {
            'S' => self.render_session_loop(body, current_body, count_only),
            // No argument reached this path, so the loop renders in the
            // session's own order, which is what no argument means.
            'W' => self.render_window_loop(&[], body, current_body, count_only),
            'P' => self.render_pane_loop(body, current_body, count_only),
            _ => None,
        }
    }

    fn format_loop_with_args(
        &self,
        scope: char,
        arguments: &[String],
        body: &str,
        current_body: Option<&str>,
        count_only: bool,
    ) -> Option<String> {
        match scope {
            'L' => {
                if count_only {
                    self.client_loop_snapshots.map(|clients| {
                        clients
                            .iter()
                            .filter(|client| client.is_attached())
                            .count()
                            .to_string()
                    })
                } else {
                    let mut expand = |template: &str, variables: &dyn FormatVariables| {
                        render_runtime_template(template, variables, false)
                    };
                    self.render_client_loop(arguments, body, &mut expand)
                }
            }
            // The window loop reads its own argument; every other scope keeps
            // the path it had.
            'W' => self.render_window_loop(arguments, body, current_body, count_only),
            _ => self.format_loop(scope, body, current_body, count_only),
        }
    }

    fn format_loop_with_args_and_expand(
        &self,
        scope: char,
        arguments: &[String],
        body: &str,
        current_body: Option<&str>,
        count_only: bool,
        expand: &mut dyn FnMut(&str, &dyn FormatVariables) -> String,
    ) -> Option<String> {
        match scope {
            'L' => {
                if count_only {
                    self.client_loop_snapshots.map(|clients| {
                        clients
                            .iter()
                            .filter(|client| client.is_attached())
                            .count()
                            .to_string()
                    })
                } else {
                    self.render_client_loop(arguments, body, expand)
                }
            }
            'S' => self.render_session_loop_with_expand(body, current_body, count_only, expand),
            'W' => self.render_window_loop_with_expand(
                arguments,
                body,
                current_body,
                count_only,
                expand,
            ),
            'P' => self.render_pane_loop_with_expand(body, current_body, count_only, expand),
            _ => self.format_loop_with_args(scope, arguments, body, current_body, count_only),
        }
    }

    fn format_name_exists(&self, scope: Option<char>, name: &str) -> Option<bool> {
        match scope {
            Some('s') => Some(
                rmux_proto::SessionName::from_canonical(name)
                    .ok()
                    .and_then(|session_name| {
                        self.session_store
                            .map(|sessions| sessions.contains_session(&session_name))
                    })
                    .unwrap_or(false),
            ),
            None | Some('w') => Some(
                self.session
                    .map(|session| {
                        session.windows().any(|(window_index, window)| {
                            self.rendered_window_name(window_index, window).as_deref() == Some(name)
                        })
                    })
                    .unwrap_or(false),
            ),
            Some(_) => None,
        }
    }

    fn format_search(&self, options: &str, pattern: &str) -> Option<String> {
        self.pane_content_search(options, pattern)
    }

    fn format_value_by_name(&self, name: &str) -> Option<String> {
        self.resolve_value_by_name(name)
            .map(RuntimeValue::into_public)
    }

    fn format_status_field(&self, name: &str) -> (String, bool) {
        match self.resolve_value_by_name(name) {
            Some(RuntimeValue::ReportedText(text)) => (text, true),
            other => (
                other.and_then(RuntimeValue::into_found).unwrap_or_default(),
                false,
            ),
        }
    }

    fn format_path_value_by_name(&self, name: &str) -> Option<String> {
        self.resolve_value_by_name(name)
            .and_then(RuntimeValue::into_found)
    }
}

impl RuntimeFormatContext<'_> {
    /// The one lookup both readings go through, in the order it always had:
    /// the floating values, the enum runtime, the pre-base registered names,
    /// the base context, the current hook, the registered runtime names, the
    /// options, the environment, and finally the historical empty answer for a
    /// known name nobody supplied.
    ///
    /// What changes is only what it says about each answer: a value it really
    /// has, or the empty answer it has always given where it has none, which
    /// stops the search exactly where it stopped before.
    fn resolve_value_by_name(&self, name: &str) -> Option<RuntimeValue> {
        let descriptor = format_variable_descriptor(name);
        if let Some(value) =
            descriptor.and_then(|descriptor| self.floating_format_value(descriptor.key()))
        {
            return Some(RuntimeValue::Found(value));
        }
        if let Some(variable) = descriptor.and_then(FormatVariableDescriptor::format_variable) {
            if let Some(value) = self.runtime_format_value(variable) {
                return Some(RuntimeValue::Found(value));
            }
        }
        if let Some(descriptor) = descriptor {
            if let Some(value) = self.pre_base_registered_format_value(descriptor.key()) {
                return Some(value);
            }
        }
        if let Some(value) = self.base.format_value_by_name(name) {
            return Some(RuntimeValue::Found(value));
        }
        if let Some(value) = current_hook_format_value(name) {
            return Some(RuntimeValue::Found(value));
        }
        if let Some(descriptor) = descriptor {
            if let Some(value) = self.registered_runtime_format_value(descriptor.key()) {
                return Some(value);
            }
        }
        if let Some(value) = self.option_value_by_name(name) {
            return Some(RuntimeValue::Found(value));
        }
        if let Some(value) = self.environment_value_by_name(name) {
            return Some(RuntimeValue::Found(value));
        }
        descriptor
            .filter(|descriptor| descriptor.has_known_empty_fallback())
            .map(|_| RuntimeValue::CompatibilityEmpty)
    }
}
