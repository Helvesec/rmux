use std::collections::{BTreeMap, HashMap};

use crate::command_parser::ParsedCommands;
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::OptionName;

use super::registry::{OptionQuery, OptionValueType};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct OptionNode {
    pub(super) entries: BTreeMap<String, OptionEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OptionEntry {
    pub(super) name: String,
    pub(super) known_option: Option<OptionName>,
    scope: OptionScopeSelector,
    value_type: OptionValueType,
    pub(super) value: OptionEntryValue,
    pub(super) rendered: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OptionEntryValue {
    Scalar(StoredOptionValue),
    Array(BTreeMap<u32, ArrayItem>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ArrayItem {
    value: StoredOptionValue,
    pub(super) rendered: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StoredOptionValue {
    String(String),
    Number(u32),
    Key(String),
    Colour(String),
    Flag(bool),
    Choice(String),
    Command(ParsedCommands),
}

impl OptionNode {
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn contains(&self, name: &str, index: Option<u32>) -> bool {
        self.value(name, index).is_some()
    }

    pub(super) fn entry(&self, name: &str) -> Option<&OptionEntry> {
        self.entries.get(name)
    }

    pub(super) fn value(&self, name: &str, index: Option<u32>) -> Option<&str> {
        self.entries.get(name).and_then(|entry| entry.value(index))
    }

    pub(super) fn into_known_values(self) -> HashMap<OptionName, String> {
        self.entries
            .into_values()
            .filter_map(|entry| entry.known_option.map(|option| (option, entry.rendered)))
            .collect()
    }

    pub(super) fn with_scope(mut self, scope: OptionScopeSelector) -> Self {
        for entry in self.entries.values_mut() {
            entry.scope = scope.clone();
        }
        self
    }
}

impl OptionEntry {
    pub(super) fn new_scalar(
        query: &OptionQuery,
        scope: OptionScopeSelector,
        value: StoredOptionValue,
    ) -> Self {
        let rendered = value.rendered();
        Self {
            name: query.canonical_name().to_owned(),
            known_option: query.known_option(),
            scope,
            value_type: query.value_type(),
            value: OptionEntryValue::Scalar(value),
            rendered,
        }
    }

    pub(super) fn new_array(
        query: &OptionQuery,
        scope: OptionScopeSelector,
        items: BTreeMap<u32, ArrayItem>,
    ) -> Self {
        let rendered = render_array(&items, query.separator());
        Self {
            name: query.canonical_name().to_owned(),
            known_option: query.known_option(),
            scope,
            value_type: query.value_type(),
            value: OptionEntryValue::Array(items),
            rendered,
        }
    }

    pub(super) fn new_empty_array(
        name: &str,
        known_option: Option<OptionName>,
        scope: OptionScopeSelector,
        value_type: OptionValueType,
    ) -> Self {
        Self {
            name: name.to_owned(),
            known_option,
            scope,
            value_type,
            value: OptionEntryValue::Array(BTreeMap::new()),
            rendered: String::new(),
        }
    }

    pub(super) fn rendered(&self) -> &str {
        &self.rendered
    }

    pub(super) fn value(&self, index: Option<u32>) -> Option<&str> {
        match (&self.value, index) {
            (OptionEntryValue::Scalar(_), None) => Some(&self.rendered),
            (OptionEntryValue::Array(items), Some(index)) => {
                items.get(&index).map(|item| item.rendered.as_str())
            }
            (OptionEntryValue::Array(_), None) => Some(&self.rendered),
            _ => None,
        }
    }

    pub(super) fn array_values(&self) -> Vec<String> {
        match &self.value {
            OptionEntryValue::Array(items) => {
                items.values().map(|item| item.rendered.clone()).collect()
            }
            OptionEntryValue::Scalar(_) => Vec::new(),
        }
    }

    pub(super) fn array_entries(&self) -> Vec<(u32, String)> {
        match &self.value {
            OptionEntryValue::Array(items) => items
                .iter()
                .map(|(index, item)| (*index, item.rendered.clone()))
                .collect(),
            OptionEntryValue::Scalar(_) => Vec::new(),
        }
    }

    pub(super) fn set_array_item(&mut self, index: u32, item: ArrayItem, separator: &str) {
        if let OptionEntryValue::Array(items) = &mut self.value {
            items.insert(index, item);
            self.rendered = render_array(items, separator);
        }
    }

    pub(super) fn next_array_index(&self) -> u32 {
        match &self.value {
            OptionEntryValue::Array(items) => items
                .keys()
                .next_back()
                .copied()
                .map_or(0, |index| index + 1),
            OptionEntryValue::Scalar(_) => 0,
        }
    }

    pub(super) fn clear_array(&mut self) {
        if let OptionEntryValue::Array(items) = &mut self.value {
            items.clear();
            self.rendered.clear();
        }
    }

    pub(super) fn remove_array_index(&mut self, index: u32, separator: &str) {
        if let OptionEntryValue::Array(items) = &mut self.value {
            items.remove(&index);
            self.rendered = render_array(items, separator);
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        matches!(&self.value, OptionEntryValue::Array(items) if items.is_empty())
    }
}

impl ArrayItem {
    pub(super) fn new(value: StoredOptionValue) -> Self {
        let rendered = value.rendered();
        Self { value, rendered }
    }
}

impl StoredOptionValue {
    fn rendered(&self) -> String {
        match self {
            Self::String(value) => value.clone(),
            Self::Number(value) => value.to_string(),
            Self::Key(value) | Self::Colour(value) => value.clone(),
            Self::Flag(true) => "on".to_owned(),
            Self::Flag(false) => "off".to_owned(),
            Self::Choice(value) => value.clone(),
            Self::Command(commands) => commands.to_tmux_string(),
        }
    }
}

fn render_array(items: &BTreeMap<u32, ArrayItem>, separator: &str) -> String {
    items
        .values()
        .map(|item| item.rendered.as_str())
        .collect::<Vec<_>>()
        .join(separator)
}
