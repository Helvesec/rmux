use super::mutation::split_array_assignment;
use super::registry::{DefaultValue, OptionQuery, OptionValueType};
use super::storage::{OptionEntry, OptionEntryValue};

pub(super) fn render_show_line(name: &str, value: &str, value_only: bool) -> String {
    if value_only {
        value.to_owned()
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
    if !value_only
        && value.is_empty()
        && matches!(
            query.value_type(),
            OptionValueType::String | OptionValueType::Command
        )
    {
        return format!("{name} ''");
    }
    render_show_line(name, value, value_only)
}

fn render_show_value(value: &str) -> String {
    if !show_value_needs_quotes(value) {
        return value.to_owned();
    }

    let escaped = value
        .chars()
        .flat_map(|character| match character {
            '"' | '$' | '\\' => ['\\', character].into_iter().collect::<Vec<_>>(),
            other => [other].into_iter().collect::<Vec<_>>(),
        })
        .collect::<String>();
    format!("\"{escaped}\"")
}

fn show_value_needs_quotes(value: &str) -> bool {
    value
        .chars()
        .any(|character| character.is_whitespace() || matches!(character, '#' | '$' | '"' | '\\'))
}

pub(super) fn show_option_name(name: &str, index: Option<u32>) -> String {
    match index {
        Some(index) => format!("{name}[{index}]"),
        None => name.to_owned(),
    }
}

pub(super) fn render_entry_show_lines(entry: &OptionEntry, value_only: bool) -> Vec<String> {
    match &entry.value {
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
