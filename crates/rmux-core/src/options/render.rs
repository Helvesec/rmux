use super::mutation::split_array_assignment;
use super::registry::{DefaultValue, OptionQuery, OptionValueType};
use super::storage::{OptionEntry, OptionEntryValue, StoredOptionValue};
use super::OptionStore;
use crate::command_parser::escape_argument;
use rmux_proto::{PaneTarget, WindowTarget};

pub(super) fn render_show_line(name: &str, value: &str, value_only: bool) -> String {
    if value_only {
        value.to_owned()
    } else if value.is_empty() && name.starts_with('@') {
        format!("{name} ''")
    } else if value.is_empty() {
        name.to_owned()
    } else {
        format!("{name} {}", render_show_value(value))
    }
}

pub(super) fn render_known_show_line(
    query: &OptionQuery,
    name: &str,
    value: &str,
    value_only: bool,
) -> String {
    if query.value_type() == OptionValueType::Command {
        return render_command_show_line(name, value, value_only);
    }
    if !value_only && value.is_empty() && matches!(query.value_type(), OptionValueType::String) {
        return format!("{name} ''");
    }
    render_show_line(name, value, value_only)
}

fn render_command_show_line(name: &str, value: &str, value_only: bool) -> String {
    if value_only {
        value.to_owned()
    } else {
        format!("{name} {value}")
    }
}

fn render_show_value(value: &str) -> String {
    escape_argument(value)
}

pub(super) fn show_option_name(name: &str, index: Option<u32>) -> String {
    match index {
        Some(index) => format!("{name}[{index}]"),
        None => name.to_owned(),
    }
}

pub(super) fn render_entry_show_lines(entry: &OptionEntry, value_only: bool) -> Vec<String> {
    match &entry.value {
        OptionEntryValue::Scalar(StoredOptionValue::Command(_)) => {
            vec![render_command_show_line(
                &entry.name,
                entry.rendered(),
                value_only,
            )]
        }
        OptionEntryValue::Scalar(_) => {
            vec![render_show_line(&entry.name, entry.rendered(), value_only)]
        }
        OptionEntryValue::Array(_) => entry
            .array_entries()
            .into_iter()
            .map(|(index, value)| {
                render_show_line(
                    &show_option_name(&entry.name, Some(index)),
                    &value,
                    value_only,
                )
            })
            .collect(),
    }
}

pub(super) fn default_array_show_values(query: &OptionQuery) -> Vec<(u32, String)> {
    match query.default_value() {
        Some(DefaultValue::Scalar(value)) => split_array_assignment(value, query.separator())
            .into_iter()
            .enumerate()
            .map(|(index, value)| (index as u32, value))
            .collect(),
        Some(DefaultValue::Array(values)) => values
            .iter()
            .enumerate()
            .map(|(index, value)| (index as u32, (*value).to_owned()))
            .collect(),
        None => Vec::new(),
    }
}

pub(super) fn format_rendered_option_value(query: &OptionQuery, value: String) -> String {
    match query.value_type() {
        OptionValueType::Flag => {
            if value == "on" {
                "1".to_owned()
            } else {
                "0".to_owned()
            }
        }
        _ => value,
    }
}

impl OptionStore {
    /// Copies the inheritance tree used by one selected window's presentation.
    /// Other sessions, windows and panes are not retained by the renderer.
    #[must_use]
    pub fn snapshot_for_selected_window(&self, session: crate::ManagedSessionView<'_>) -> Self {
        let mut snapshot = Self {
            server_global: self.server_global.clone(),
            session_global: self.session_global.clone(),
            window_global: self.window_global.clone(),
            ..Self::default()
        };
        if let Some(options) = self.sessions.get(session.name()) {
            snapshot
                .sessions
                .insert(session.name().clone(), options.clone());
        }
        let window =
            WindowTarget::with_window(session.name().clone(), session.active_window_index());
        if let Some(options) = self.windows.get(&window) {
            snapshot.windows.insert(window, options.clone());
        }
        for pane in session.window().panes() {
            let target = PaneTarget::with_window(
                session.name().clone(),
                session.active_window_index(),
                pane.index(),
            );
            if let Some(options) = self.panes.get(&target) {
                snapshot.panes.insert(target, options.clone());
            }
        }
        snapshot
    }
}
