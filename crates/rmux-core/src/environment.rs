use std::collections::{HashMap, HashSet};

use rmux_proto::{RmuxError, ScopeSelector, SessionName};

/// tmux-compatible hidden environment entry flag.
pub const ENVIRON_HIDDEN: u8 = 0x1;

/// Renderable `show-environment` entry with tmux-compatible flags and tombstones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowEnvironmentEntry {
    /// Environment variable name.
    pub name: String,
    /// Stored value, or `None` for a cleared tombstone.
    pub value: Option<String>,
    /// tmux-compatible entry flags.
    pub flags: u8,
    /// Whether `value` already contains tmux-style display escapes for raw bytes.
    pub value_is_display_escape: bool,
}

impl ShowEnvironmentEntry {
    /// Returns whether the entry is hidden from normal `show-environment` output.
    #[must_use]
    pub const fn is_hidden(&self) -> bool {
        self.flags & ENVIRON_HIDDEN != 0
    }

    /// Returns whether the entry is a cleared tombstone.
    #[must_use]
    pub const fn is_cleared(&self) -> bool {
        self.value.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct EnvironmentEntry {
    value: Option<String>,
    display_value: Option<String>,
    flags: u8,
    implicit: bool,
}

impl EnvironmentEntry {
    fn new(value: String, flags: u8) -> Self {
        Self {
            value: Some(value),
            display_value: None,
            flags,
            implicit: false,
        }
    }

    fn implicit(value: String) -> Self {
        Self {
            value: Some(value),
            display_value: None,
            flags: 0,
            implicit: true,
        }
    }

    fn implicit_display(value: String) -> Self {
        Self {
            value: None,
            display_value: Some(value),
            flags: 0,
            implicit: true,
        }
    }

    fn clear(&mut self) {
        self.value = None;
        self.display_value = None;
        self.implicit = false;
    }

    const fn flags(&self) -> u8 {
        self.flags
    }

    fn value(&self) -> Option<&str> {
        self.value.as_deref()
    }

    fn show_value(&self) -> Option<&str> {
        self.value.as_deref().or(self.display_value.as_deref())
    }

    const fn is_hidden(&self) -> bool {
        self.flags & ENVIRON_HIDDEN != 0
    }

    const fn is_cleared(&self) -> bool {
        self.value.is_none() && self.display_value.is_none()
    }

    const fn is_implicit(&self) -> bool {
        self.implicit
    }
}

/// In-memory storage for global and session-local environment values.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnvironmentStore {
    global: HashMap<String, EnvironmentEntry>,
    sessions: HashMap<SessionName, HashMap<String, EnvironmentEntry>>,
    global_unsets: HashSet<String>,
    session_unsets: HashMap<SessionName, HashSet<String>>,
}

impl EnvironmentStore {
    /// Creates an empty environment store with no implicit defaults.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether neither global nor session-local values are present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.global.is_empty() && self.sessions.is_empty()
    }

    /// Stores the given visible value in the selected scope.
    pub fn set(&mut self, scope: ScopeSelector, name: String, value: String) {
        self.set_with_flags(scope, name, value, 0);
    }

    /// Stores the given value and flag word in the selected scope.
    pub fn set_with_flags(&mut self, scope: ScopeSelector, name: String, value: String, flags: u8) {
        self.forget_unset(&scope, &name);
        insert_environment_entry(
            self.scope_entries_mut(scope),
            name,
            EnvironmentEntry::new(value, flags),
        );
    }

    /// Stores an implicit global value captured from the server environment.
    pub fn set_implicit_global(&mut self, name: String, value: String) {
        remove_name_from_set(&mut self.global_unsets, &name);
        insert_environment_entry(&mut self.global, name, EnvironmentEntry::implicit(value));
    }

    /// Stores an implicit global value that is only renderable by `show-environment`.
    pub fn set_implicit_global_display(&mut self, name: String, value: String) {
        remove_name_from_set(&mut self.global_unsets, &name);
        insert_environment_entry(
            &mut self.global,
            name,
            EnvironmentEntry::implicit_display(value),
        );
    }

    /// Clears the selected variable, leaving a tombstone entry behind.
    pub fn clear(&mut self, scope: ScopeSelector, name: String) {
        self.record_unset(&scope, name.clone());
        let entries = self.scope_entries_mut(scope);
        if let Some(entry) = environment_entry_mut(entries, &name) {
            entry.clear();
        } else {
            insert_environment_entry(entries, name, EnvironmentEntry::default());
        }
    }

    /// Removes the selected variable entirely.
    pub fn unset(&mut self, scope: ScopeSelector, name: &str) -> bool {
        match &scope {
            ScopeSelector::Global => self.record_unset(&scope, name.to_owned()),
            ScopeSelector::Session(_) => self.forget_unset(&scope, name),
            ScopeSelector::Window(_) | ScopeSelector::Pane(_) => {}
        }
        remove_environment_entry(self.scope_entries_mut(scope), name).is_some()
    }

    /// Returns whether the exact entry exists in the selected scope.
    #[must_use]
    pub fn contains_entry(&self, scope: &ScopeSelector, name: &str) -> bool {
        self.scope_entries(scope)
            .is_some_and(|entries| environment_entry(entries, name).is_some())
    }

    /// Returns the exact global value for the given variable, when present and not cleared.
    #[must_use]
    pub fn global_value(&self, name: &str) -> Option<&str> {
        environment_entry(&self.global, name).and_then(EnvironmentEntry::value)
    }

    /// Returns all exact global environment entries in unspecified order.
    pub fn global_entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.global
            .iter()
            .filter_map(|(name, entry)| entry.value().map(|value| (name.as_str(), value)))
    }

    /// Returns the exact session-local value for the given variable, when present and not cleared.
    #[must_use]
    pub fn session_value(&self, session_name: &SessionName, name: &str) -> Option<&str> {
        self.sessions
            .get(session_name)
            .and_then(|values| environment_entry(values, name))
            .and_then(EnvironmentEntry::value)
    }

    /// Resolves a single variable using session-local then global lookup.
    #[must_use]
    pub fn resolve(&self, session_name: Option<&SessionName>, name: &str) -> Option<&str> {
        if let Some(session_name) = session_name {
            if let Some(entry) = self
                .sessions
                .get(session_name)
                .and_then(|values| environment_entry(values, name))
            {
                return entry.value();
            }
        }

        environment_entry(&self.global, name).and_then(EnvironmentEntry::value)
    }

    /// Returns the visible explicit environment snapshot that future panes should inherit.
    #[must_use]
    pub fn resolved(&self, session_name: &SessionName) -> HashMap<String, String> {
        let mut values = HashMap::new();
        for (name, entry) in &self.global {
            apply_entry_to_child_environment(&mut values, name, entry);
        }
        if let Some(session_values) = self.sessions.get(session_name) {
            for (name, entry) in session_values {
                apply_entry_to_child_environment(&mut values, name, entry);
            }
        }
        values
    }

    /// Applies the selected scope chain to a process environment map.
    pub fn apply_to_process_environment(
        &self,
        session_name: Option<&SessionName>,
        values: &mut HashMap<String, String>,
    ) {
        self.apply_to_process_environment_inner(session_name, values, true);
    }

    /// Applies explicit values only, skipping implicit globals captured from the server process.
    pub fn apply_to_process_environment_without_implicit_globals(
        &self,
        session_name: Option<&SessionName>,
        values: &mut HashMap<String, String>,
    ) {
        self.apply_to_process_environment_inner(session_name, values, false);
    }

    /// Returns names that must be removed from raw process environments.
    #[must_use]
    pub fn suppressed_process_environment_names(
        &self,
        session_name: Option<&SessionName>,
        include_implicit_globals: bool,
    ) -> HashSet<String> {
        let mut names = self.global_unsets.clone();
        collect_suppressed_entry_names(&self.global, include_implicit_globals, &mut names);

        if let Some(session_name) = session_name {
            if let Some(session_unsets) = self.session_unsets.get(session_name) {
                names.extend(session_unsets.iter().cloned());
            }
            if let Some(entries) = self.sessions.get(session_name) {
                collect_suppressed_entry_names(entries, true, &mut names);
            }
        }

        names
    }

    fn apply_to_process_environment_inner(
        &self,
        session_name: Option<&SessionName>,
        values: &mut HashMap<String, String>,
        include_implicit_globals: bool,
    ) {
        for (name, entry) in &self.global {
            if !include_implicit_globals && entry.is_implicit() {
                continue;
            }
            apply_entry_to_child_environment(values, name, entry);
        }
        remove_unset_names(values, &self.global_unsets);

        if let Some(session_name) = session_name {
            if let Some(session_values) = self.sessions.get(session_name) {
                for (name, entry) in session_values {
                    apply_entry_to_child_environment(values, name, entry);
                }
            }
            if let Some(session_unsets) = self.session_unsets.get(session_name) {
                remove_unset_names(values, session_unsets);
            }
        }
    }

    /// Merges client variables into a session environment using tmux `update-environment`.
    pub fn update(
        &mut self,
        session_name: &SessionName,
        patterns: &[String],
        source: &HashMap<String, String>,
    ) {
        for pattern in patterns {
            let mut found = false;
            for (name, value) in source {
                if crate::fnmatch(pattern, name) {
                    self.set(
                        ScopeSelector::Session(session_name.clone()),
                        name.clone(),
                        value.clone(),
                    );
                    found = true;
                }
            }
            if !found {
                self.clear(
                    ScopeSelector::Session(session_name.clone()),
                    pattern.clone(),
                );
            }
        }
    }

    /// Returns sorted `show-environment` entries for the selected global or session scope.
    pub fn show_environment_entries(
        &self,
        scope: &ScopeSelector,
        hidden_only: bool,
        name: Option<&str>,
    ) -> Result<Vec<ShowEnvironmentEntry>, RmuxError> {
        let exact_entries = match scope {
            ScopeSelector::Global => &self.global,
            ScopeSelector::Session(session_name) => {
                if let Some(entries) = self.sessions.get(session_name) {
                    entries
                } else {
                    empty_environment_entries()
                }
            }
            ScopeSelector::Window(_) | ScopeSelector::Pane(_) => {
                return Err(RmuxError::Server(
                    "show-environment only supports global or session scope".to_owned(),
                ));
            }
        };

        if let Some(name) = name {
            let Some((stored_name, entry)) = environment_entry_with_name(exact_entries, name)
            else {
                return Err(RmuxError::Server(format!("unknown variable: {name}")));
            };
            if hidden_only && !entry.is_hidden() {
                return Ok(Vec::new());
            }
            if !hidden_only && entry.is_hidden() {
                return Ok(Vec::new());
            }
            return Ok(vec![ShowEnvironmentEntry {
                name: stored_name.to_owned(),
                value: entry.show_value().map(str::to_owned),
                flags: entry.flags(),
                value_is_display_escape: entry.value.is_none() && entry.display_value.is_some(),
            }]);
        }

        let mut values = exact_entries
            .iter()
            .filter(|(_, entry)| hidden_only == entry.is_hidden())
            .map(|(name, entry)| ShowEnvironmentEntry {
                name: name.clone(),
                value: entry.show_value().map(str::to_owned),
                flags: entry.flags(),
                value_is_display_escape: entry.value.is_none() && entry.display_value.is_some(),
            })
            .collect::<Vec<_>>();
        values.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(values)
    }

    /// Removes all session-local values for the given session.
    pub fn remove_session(
        &mut self,
        session_name: &SessionName,
    ) -> Option<HashMap<String, String>> {
        self.session_unsets.remove(session_name);
        self.sessions.remove(session_name).map(|entries| {
            entries
                .into_iter()
                .filter_map(|(name, entry)| entry.value.map(|value| (name, value)))
                .collect()
        })
    }

    /// Rekeys all session-local values from one validated session name to another.
    pub fn rename_session(
        &mut self,
        session_name: &SessionName,
        new_name: SessionName,
    ) -> Result<(), RmuxError> {
        if self.sessions.contains_key(&new_name) {
            return Err(RmuxError::Server(format!(
                "environment already exists for session {new_name}"
            )));
        }

        let mut sessions = std::mem::take(&mut self.sessions);
        if let Some(values) = sessions.remove(session_name) {
            let replaced = sessions.insert(new_name.clone(), values);
            debug_assert!(replaced.is_none());
        }
        self.sessions = sessions;
        let unsets = self.session_unsets.remove(session_name);
        if let Some(unsets) = unsets {
            let replaced = self.session_unsets.insert(new_name, unsets);
            debug_assert!(replaced.is_none());
        }
        Ok(())
    }

    fn scope_entries_mut(
        &mut self,
        scope: ScopeSelector,
    ) -> &mut HashMap<String, EnvironmentEntry> {
        match scope {
            ScopeSelector::Global => &mut self.global,
            ScopeSelector::Session(session_name) => self.sessions.entry(session_name).or_default(),
            ScopeSelector::Window(_) | ScopeSelector::Pane(_) => {
                unreachable!("environment mutations are validated before storage")
            }
        }
    }

    fn scope_entries(&self, scope: &ScopeSelector) -> Option<&HashMap<String, EnvironmentEntry>> {
        match scope {
            ScopeSelector::Global => Some(&self.global),
            ScopeSelector::Session(session_name) => self.sessions.get(session_name),
            ScopeSelector::Window(_) | ScopeSelector::Pane(_) => None,
        }
    }

    fn record_unset(&mut self, scope: &ScopeSelector, name: String) {
        match scope {
            ScopeSelector::Global => {
                insert_name_into_set(&mut self.global_unsets, name);
            }
            ScopeSelector::Session(session_name) => {
                let unsets = self.session_unsets.entry(session_name.clone()).or_default();
                insert_name_into_set(unsets, name);
            }
            ScopeSelector::Window(_) | ScopeSelector::Pane(_) => {}
        }
    }

    fn forget_unset(&mut self, scope: &ScopeSelector, name: &str) {
        match scope {
            ScopeSelector::Global => {
                remove_name_from_set(&mut self.global_unsets, name);
            }
            ScopeSelector::Session(session_name) => {
                let remove_bucket = if let Some(unsets) = self.session_unsets.get_mut(session_name)
                {
                    remove_name_from_set(unsets, name);
                    unsets.is_empty()
                } else {
                    false
                };
                if remove_bucket {
                    self.session_unsets.remove(session_name);
                }
            }
            ScopeSelector::Window(_) | ScopeSelector::Pane(_) => {}
        }
    }
}

fn apply_entry_to_child_environment(
    values: &mut HashMap<String, String>,
    name: &str,
    entry: &EnvironmentEntry,
) {
    if entry.is_hidden() || entry.is_cleared() {
        remove_environment_name(values, name);
    } else if let Some(value) = entry.value() {
        remove_environment_name(values, name);
        values.insert(name.to_owned(), value.to_owned());
    }
}

fn collect_suppressed_entry_names(
    entries: &HashMap<String, EnvironmentEntry>,
    include_implicit: bool,
    names: &mut HashSet<String>,
) {
    for (name, entry) in entries {
        if !include_implicit && entry.is_implicit() {
            continue;
        }
        if entry.is_hidden() || entry.is_cleared() {
            names.insert(name.clone());
        }
    }
}

fn remove_unset_names(values: &mut HashMap<String, String>, names: &HashSet<String>) {
    for name in names {
        remove_environment_name(values, name);
    }
}

fn remove_environment_name(values: &mut HashMap<String, String>, name: &str) {
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

fn insert_environment_entry(
    entries: &mut HashMap<String, EnvironmentEntry>,
    name: String,
    entry: EnvironmentEntry,
) {
    let _ = remove_environment_entry(entries, &name);
    entries.insert(name, entry);
}

fn remove_environment_entry(
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

fn environment_entry<'a>(
    entries: &'a HashMap<String, EnvironmentEntry>,
    name: &str,
) -> Option<&'a EnvironmentEntry> {
    environment_entry_with_name(entries, name).map(|(_, entry)| entry)
}

fn environment_entry_with_name<'a>(
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

fn environment_entry_mut<'a>(
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

fn insert_name_into_set(names: &mut HashSet<String>, name: String) {
    remove_name_from_set(names, &name);
    names.insert(name);
}

fn remove_name_from_set(names: &mut HashSet<String>, name: &str) -> bool {
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

fn empty_environment_entries() -> &'static HashMap<String, EnvironmentEntry> {
    static EMPTY: std::sync::OnceLock<HashMap<String, EnvironmentEntry>> =
        std::sync::OnceLock::new();
    EMPTY.get_or_init(HashMap::new)
}

#[cfg(test)]
#[path = "environment/tests.rs"]
mod tests;
