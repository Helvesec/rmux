//! Environment name keys: exact on Unix, ASCII case-insensitive on Windows.

use std::collections::{HashMap, HashSet};

use super::EnvironmentEntry;

pub(super) fn remove_environment_name(values: &mut HashMap<String, String>, name: &str) {
    #[cfg(windows)]
    if let Some(existing) = values
        .keys()
        .find(|key| key.eq_ignore_ascii_case(name))
        .cloned()
    {
        values.remove(&existing);
        return;
    }

    values.remove(name);
}

pub(super) fn insert_environment_entry(
    entries: &mut HashMap<String, EnvironmentEntry>,
    name: String,
    entry: EnvironmentEntry,
) {
    let _ = remove_environment_entry(entries, &name);
    entries.insert(name, entry);
}

pub(super) fn remove_environment_entry(
    entries: &mut HashMap<String, EnvironmentEntry>,
    name: &str,
) -> Option<EnvironmentEntry> {
    #[cfg(windows)]
    if let Some(existing) = entries
        .keys()
        .find(|key| key.eq_ignore_ascii_case(name))
        .cloned()
    {
        return entries.remove(&existing);
    }

    entries.remove(name)
}

pub(super) fn environment_entry<'a>(
    entries: &'a HashMap<String, EnvironmentEntry>,
    name: &str,
) -> Option<&'a EnvironmentEntry> {
    environment_entry_with_name(entries, name).map(|(_, entry)| entry)
}

pub(super) fn environment_entry_with_name<'a>(
    entries: &'a HashMap<String, EnvironmentEntry>,
    name: &str,
) -> Option<(&'a str, &'a EnvironmentEntry)> {
    #[cfg(windows)]
    if let Some((existing, entry)) = entries
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
    {
        return Some((existing.as_str(), entry));
    }

    entries
        .get_key_value(name)
        .map(|(key, entry)| (key.as_str(), entry))
}

pub(super) fn environment_entry_mut<'a>(
    entries: &'a mut HashMap<String, EnvironmentEntry>,
    name: &str,
) -> Option<&'a mut EnvironmentEntry> {
    #[cfg(windows)]
    if let Some(existing) = entries
        .keys()
        .find(|key| key.eq_ignore_ascii_case(name))
        .cloned()
    {
        return entries.get_mut(&existing);
    }

    entries.get_mut(name)
}

pub(super) fn insert_name_into_set(names: &mut HashSet<String>, name: String) {
    remove_name_from_set(names, &name);
    names.insert(name);
}

pub(super) fn remove_name_from_set(names: &mut HashSet<String>, name: &str) -> bool {
    #[cfg(windows)]
    if let Some(existing) = names
        .iter()
        .find(|candidate| candidate.eq_ignore_ascii_case(name))
        .cloned()
    {
        return names.remove(&existing);
    }

    names.remove(name)
}

pub(super) fn empty_environment_entries() -> &'static HashMap<String, EnvironmentEntry> {
    static EMPTY: std::sync::OnceLock<HashMap<String, EnvironmentEntry>> =
        std::sync::OnceLock::new();
    EMPTY.get_or_init(HashMap::new)
}
