use std::collections::HashMap;

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
    flags: u8,
}

impl EnvironmentEntry {
    fn new(value: String, flags: u8) -> Self {
        Self {
            value: Some(value),
            flags,
        }
    }

    fn clear(&mut self) {
        self.value = None;
    }

    const fn flags(&self) -> u8 {
        self.flags
    }

    fn value(&self) -> Option<&str> {
        self.value.as_deref()
    }

    const fn is_hidden(&self) -> bool {
        self.flags & ENVIRON_HIDDEN != 0
    }

    const fn is_cleared(&self) -> bool {
        self.value.is_none()
    }
}

/// In-memory storage for global and session-local environment values.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnvironmentStore {
    global: HashMap<String, EnvironmentEntry>,
    sessions: HashMap<SessionName, HashMap<String, EnvironmentEntry>>,
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
        self.scope_entries_mut(scope)
            .insert(name, EnvironmentEntry::new(value, flags));
    }

    /// Clears the selected variable, leaving a tombstone entry behind.
    pub fn clear(&mut self, scope: ScopeSelector, name: String) {
        let entries = self.scope_entries_mut(scope);
        if let Some(entry) = entries.get_mut(&name) {
            entry.clear();
        } else {
            entries.insert(name, EnvironmentEntry::default());
        }
    }

    /// Removes the selected variable entirely.
    pub fn unset(&mut self, scope: ScopeSelector, name: &str) -> bool {
        self.scope_entries_mut(scope).remove(name).is_some()
    }

    /// Returns whether the exact entry exists in the selected scope.
    #[must_use]
    pub fn contains_entry(&self, scope: &ScopeSelector, name: &str) -> bool {
        self.scope_entries(scope)
            .is_some_and(|entries| entries.contains_key(name))
    }

    /// Returns the exact global value for the given variable, when present and not cleared.
    #[must_use]
    pub fn global_value(&self, name: &str) -> Option<&str> {
        self.global.get(name).and_then(EnvironmentEntry::value)
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
            .and_then(|values| values.get(name))
            .and_then(EnvironmentEntry::value)
    }

    /// Resolves a single variable using session-local then global lookup.
    #[must_use]
    pub fn resolve(&self, session_name: Option<&SessionName>, name: &str) -> Option<&str> {
        if let Some(session_name) = session_name
            && let Some(entry) = self
                .sessions
                .get(session_name)
                .and_then(|values| values.get(name))
            {
                return entry.value();
            }

        self.global.get(name).and_then(EnvironmentEntry::value)
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
        for (name, entry) in &self.global {
            apply_entry_to_child_environment(values, name, entry);
        }

        if let Some(session_name) = session_name
            && let Some(session_values) = self.sessions.get(session_name) {
                for (name, entry) in session_values {
                    apply_entry_to_child_environment(values, name, entry);
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
            let Some(entry) = exact_entries.get(name) else {
                return Err(RmuxError::Server(format!("unknown variable: {name}")));
            };
            if hidden_only && !entry.is_hidden() {
                return Ok(Vec::new());
            }
            if !hidden_only && entry.is_hidden() {
                return Ok(Vec::new());
            }
            return Ok(vec![ShowEnvironmentEntry {
                name: name.to_owned(),
                value: entry.value.clone(),
                flags: entry.flags(),
            }]);
        }

        let mut values = exact_entries
            .iter()
            .filter(|(_, entry)| hidden_only == entry.is_hidden())
            .map(|(name, entry)| ShowEnvironmentEntry {
                name: name.clone(),
                value: entry.value.clone(),
                flags: entry.flags(),
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
            let replaced = sessions.insert(new_name, values);
            debug_assert!(replaced.is_none());
        }
        self.sessions = sessions;
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
}

fn apply_entry_to_child_environment(
    values: &mut HashMap<String, String>,
    name: &str,
    entry: &EnvironmentEntry,
) {
    if entry.is_hidden() || entry.is_cleared() {
        values.remove(name);
    } else if let Some(value) = entry.value() {
        values.insert(name.to_owned(), value.to_owned());
    }
}

fn empty_environment_entries() -> &'static HashMap<String, EnvironmentEntry> {
    static EMPTY: std::sync::OnceLock<HashMap<String, EnvironmentEntry>> =
        std::sync::OnceLock::new();
    EMPTY.get_or_init(HashMap::new)
}

#[cfg(test)]
#[path = "environment/tests.rs"]
mod tests;
