use std::collections::BTreeMap;

use crate::{
    colour_to_string, command_parser::parse_command_string, key_string_lookup_key,
    key_string_lookup_string, parse_colour, Style, KEYC_UNKNOWN,
};
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{OptionName, RmuxError, ScopeSelector, SetOptionMode};

use super::registry::{
    self, option_metadata, resolve_option_name, DefaultValue, GlobalRoot, OptionMetadata,
    OptionValueType,
};
use super::storage::{ArrayItem, OptionEntry, StoredOptionValue};
use super::{OptionMutationOutcome, OptionNotification, OptionQuery};

/// Validates an option mutation against the legacy known-option registry.
pub fn validate_option_mutation(
    option: OptionName,
    scope: &ScopeSelector,
    mode: SetOptionMode,
    value: &str,
) -> Result<(), RmuxError> {
    let query = OptionQuery::known(option);
    let explicit_scope = legacy_scope_for_option(option, scope);
    validate_query_mutation(&query, &explicit_scope, mode, Some(value), false)
}

/// Validates an option mutation against the open string-keyed registry.
pub fn validate_option_name_mutation(
    name: &str,
    scope: &OptionScopeSelector,
    mode: SetOptionMode,
    value: Option<&str>,
    unset: bool,
) -> Result<OptionQuery, RmuxError> {
    let query = resolve_option_name(name)?;
    validate_query_mutation(&query, scope, mode, value, unset)?;
    Ok(query)
}

fn validate_query_mutation(
    query: &OptionQuery,
    scope: &OptionScopeSelector,
    mode: SetOptionMode,
    value: Option<&str>,
    unset: bool,
) -> Result<(), RmuxError> {
    if let Some(metadata) = query.metadata() {
        if !metadata.supports_scope(scope) {
            return Err(RmuxError::InvalidSetOption(format!(
                "{} is only supported at {} scope",
                query.canonical_name(),
                allowed_scope_message(metadata),
            )));
        }
    }

    if mode == SetOptionMode::Append
        && !unset
        && !query.is_array()
        && !matches!(query.value_type(), OptionValueType::String)
    {
        return Err(RmuxError::InvalidSetOption(format!(
            "{} is not an array option",
            query.canonical_name()
        )));
    }

    if !unset && !query.is_array() {
        match query.value_type() {
            OptionValueType::Flag | OptionValueType::Choice(_) => {}
            _ if query.is_user() && value.is_none() => {
                return Err(RmuxError::InvalidSetOption("empty value".to_owned()))
            }
            OptionValueType::String
            | OptionValueType::Number { .. }
            | OptionValueType::Key
            | OptionValueType::Colour
            | OptionValueType::Command
                if value.is_none() =>
            {
                return Err(RmuxError::InvalidSetOption("empty value".to_owned()))
            }
            _ => {
                let _ = normalize_scalar_value(query, value, None)?;
            }
        }
    }

    Ok(())
}

pub(super) fn normalize_scalar_value(
    query: &OptionQuery,
    value: Option<&str>,
    current: Option<&str>,
) -> Result<StoredOptionValue, RmuxError> {
    match query.value_type() {
        OptionValueType::String => {
            let raw = value.ok_or_else(|| RmuxError::InvalidSetOption("empty value".to_owned()))?;
            let next = match current {
                Some(current) => format!("{current}{}{raw}", query.separator()),
                None => raw.to_owned(),
            };
            if query.canonical_name() == "default-size" && !matches_default_size_pattern(&next) {
                return Err(RmuxError::InvalidSetOption(format!(
                    "value is invalid: {next}"
                )));
            }
            if query.effects().contains(registry::EFFECT_STYLE_PARSE)
                && !next.contains("#{")
                && normalize_style_string(&next).is_err()
            {
                return Err(RmuxError::InvalidSetOption(format!(
                    "invalid style: {next}"
                )));
            }
            Ok(StoredOptionValue::String(next))
        }
        OptionValueType::Number { minimum } => {
            let raw = value.ok_or_else(|| RmuxError::InvalidSetOption("empty value".to_owned()))?;
            let parsed = raw
                .parse::<i64>()
                .map_err(|_| RmuxError::InvalidSetOption(format!("value is invalid: {raw}")))?;
            if parsed < i64::from(minimum) {
                return Err(RmuxError::InvalidSetOption(format!(
                    "value is too small: {raw}"
                )));
            }
            let parsed = u32::try_from(parsed)
                .map_err(|_| RmuxError::InvalidSetOption(format!("value is too large: {raw}")))?;
            Ok(StoredOptionValue::Number(parsed))
        }
        OptionValueType::Key => {
            let raw = value.ok_or_else(|| RmuxError::InvalidSetOption("empty value".to_owned()))?;
            Ok(StoredOptionValue::Key(normalize_key(raw).ok_or_else(
                || invalid_integer(query.canonical_name(), "key code"),
            )?))
        }
        OptionValueType::Colour => {
            let raw = value.ok_or_else(|| RmuxError::InvalidSetOption("empty value".to_owned()))?;
            Ok(StoredOptionValue::Colour(normalize_colour(raw).map_err(
                |_| invalid_integer(query.canonical_name(), "colour value"),
            )?))
        }
        OptionValueType::Flag => {
            let toggled = match value.map(str::trim) {
                None | Some("") => !matches!(current, Some("on")),
                Some(raw) if matches_flag_true(raw) => true,
                Some(raw) if matches_flag_false(raw) => false,
                Some(raw) => return Err(RmuxError::InvalidSetOption(format!("bad value: {raw}"))),
            };
            Ok(StoredOptionValue::Flag(toggled))
        }
        OptionValueType::Choice(choices) => {
            let raw = value.unwrap_or_default();
            if raw.is_empty() {
                if choices.contains(&"on") && choices.contains(&"off") {
                    let current = current.unwrap_or(choices[0]);
                    let next = if current == "on" { "off" } else { "on" };
                    return Ok(StoredOptionValue::Choice(next.to_owned()));
                }
                return Err(RmuxError::InvalidSetOption("empty value".to_owned()));
            }
            if choices.contains(&raw) {
                Ok(StoredOptionValue::Choice(raw.to_owned()))
            } else {
                Err(RmuxError::InvalidSetOption(format!("unknown value: {raw}")))
            }
        }
        OptionValueType::Command => {
            let raw = value.ok_or_else(|| RmuxError::InvalidSetOption("empty value".to_owned()))?;
            let commands = parse_command_string(raw).map_err(|error| {
                RmuxError::InvalidSetOption(format!(
                    "{} expects a command list: {error}",
                    query.canonical_name()
                ))
            })?;
            Ok(StoredOptionValue::Command(commands))
        }
    }
}

pub(super) fn apply_array_mutation(
    entry: &mut OptionEntry,
    query: &OptionQuery,
    value: &str,
    mode: SetOptionMode,
    current: Option<&str>,
) -> Result<(), RmuxError> {
    let separator = query.separator();
    let indexes = split_array_assignment(value, separator);
    match (query.index(), mode) {
        (Some(index), SetOptionMode::Replace) => {
            let item = array_item_from_value(query, Some(value), None)?;
            entry.set_array_item(index, item, separator);
        }
        (Some(index), SetOptionMode::Append) => {
            let item = array_item_from_value(query, Some(value), current)?;
            entry.set_array_item(index, item, separator);
        }
        (None, SetOptionMode::Replace) => {
            entry.clear_array();
            for item_value in indexes {
                let next_index = entry.next_array_index();
                let item = array_item_from_value(query, Some(&item_value), None)?;
                entry.set_array_item(next_index, item, separator);
            }
        }
        (None, SetOptionMode::Append) => {
            for item_value in indexes {
                let next_index = entry.next_array_index();
                let item = array_item_from_value(query, Some(&item_value), None)?;
                entry.set_array_item(next_index, item, separator);
            }
        }
    }
    Ok(())
}

fn array_item_from_value(
    query: &OptionQuery,
    value: Option<&str>,
    current: Option<&str>,
) -> Result<ArrayItem, RmuxError> {
    let normalized = match current {
        Some(current)
            if query.index().is_some() && matches!(query.value_type(), OptionValueType::String) =>
        {
            let joined = format!("{current}{}", value.unwrap_or_default());
            normalize_scalar_value(query, Some(&joined), None)?
        }
        _ => normalize_scalar_value(query, value, None)?,
    };
    Ok(ArrayItem::new(normalized))
}

pub(super) fn default_array_items(
    query: &OptionQuery,
    default: DefaultValue,
) -> Result<BTreeMap<u32, ArrayItem>, RmuxError> {
    let mut items = BTreeMap::new();
    match default {
        DefaultValue::Scalar(value) => {
            for (index, item) in split_array_assignment(value, query.separator())
                .into_iter()
                .enumerate()
            {
                items.insert(
                    index as u32,
                    array_item_from_value(query, Some(&item), None)?,
                );
            }
        }
        DefaultValue::Array(values) => {
            for (index, item) in values.iter().enumerate() {
                items.insert(
                    index as u32,
                    array_item_from_value(query, Some(item), None)?,
                );
            }
        }
    }
    Ok(items)
}

pub(super) fn split_array_assignment(value: &str, separator: &str) -> Vec<String> {
    if separator.is_empty() {
        return vec![value.to_owned()];
    }
    if separator.contains(',') {
        return value
            .split(',')
            .map(str::trim)
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect();
    }
    value
        .split(separator)
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(super) fn default_scalar_text(default: DefaultValue) -> &'static str {
    match default {
        DefaultValue::Scalar(value) => value,
        DefaultValue::Array(_) => "",
    }
}

pub(super) fn build_mutation_outcome(
    query: &OptionQuery,
    scope: OptionScopeSelector,
) -> OptionMutationOutcome {
    let notification = OptionNotification {
        name: query.canonical_name().to_owned(),
        scope: scope.clone(),
        effects: query.effects(),
    };
    OptionMutationOutcome {
        name: query.canonical_name().to_owned(),
        known_option: query.known_option(),
        notifications: if notification.effects.is_empty() {
            Vec::new()
        } else {
            vec![notification]
        },
    }
}

pub(super) fn legacy_scope_for_option(
    option: OptionName,
    scope: &ScopeSelector,
) -> OptionScopeSelector {
    match scope {
        ScopeSelector::Global => match option_metadata(option).global_root() {
            GlobalRoot::Server => OptionScopeSelector::ServerGlobal,
            GlobalRoot::Session => OptionScopeSelector::SessionGlobal,
            GlobalRoot::Window => OptionScopeSelector::WindowGlobal,
        },
        ScopeSelector::Session(session_name) => OptionScopeSelector::Session(session_name.clone()),
        ScopeSelector::Window(target) => OptionScopeSelector::Window(target.clone()),
        ScopeSelector::Pane(target) => OptionScopeSelector::Pane(target.clone()),
    }
}

pub(super) fn is_global_scope(scope: &OptionScopeSelector) -> bool {
    matches!(
        scope,
        OptionScopeSelector::ServerGlobal
            | OptionScopeSelector::SessionGlobal
            | OptionScopeSelector::WindowGlobal
    )
}

fn allowed_scope_message(metadata: &OptionMetadata) -> String {
    let mut scopes = Vec::new();
    if metadata.scope_mask() & registry::SCOPE_SERVER != 0 {
        scopes.push("global");
    }
    if metadata.scope_mask() & registry::SCOPE_SESSION != 0 {
        scopes.push("session");
    }
    if metadata.scope_mask() & registry::SCOPE_WINDOW != 0 {
        scopes.push("window");
    }
    if metadata.scope_mask() & registry::SCOPE_PANE != 0 {
        scopes.push("pane");
    }
    scopes.join(" or ")
}

fn invalid_integer(name: &str, label: &str) -> RmuxError {
    RmuxError::InvalidSetOption(format!("{name} expects a {label}"))
}

fn matches_default_size_pattern(value: &str) -> bool {
    let Some((width, height)) = value.split_once('x') else {
        return false;
    };
    !width.is_empty()
        && !height.is_empty()
        && width.chars().all(|character| character.is_ascii_digit())
        && height.chars().all(|character| character.is_ascii_digit())
}

fn normalize_key(value: &str) -> Option<String> {
    let key = key_string_lookup_string(value.trim())?;
    if key == KEYC_UNKNOWN {
        return None;
    }
    Some(key_string_lookup_key(key, false))
}

fn normalize_colour(value: &str) -> Result<String, ()> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(());
    }
    Ok(colour_to_string(parse_colour(trimmed).map_err(|_| ())?))
}

fn normalize_style_string(value: &str) -> Result<(), ()> {
    Style::parse(value).map(|_| ()).map_err(|_| ())
}

fn matches_flag_true(value: &str) -> bool {
    matches!(value.to_ascii_lowercase().as_str(), "on" | "1" | "yes")
}

fn matches_flag_false(value: &str) -> bool {
    matches!(value.to_ascii_lowercase().as_str(), "off" | "0" | "no")
}
