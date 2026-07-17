use std::collections::BTreeMap;

use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{PaneTarget, RmuxError, WindowTarget};

use super::registry::{registry, resolve_option_name, GlobalRoot};
use super::render::{
    default_array_show_values, render_entry_show_lines, render_known_show_line, render_show_line,
    show_option_name,
};
use super::scope::{ResolveContext, ShowScope};
use super::storage::{OptionEntry, OptionNode};
use super::{OptionQuery, OptionStore, ShowOptionsMode};

impl OptionStore {
    /// Returns rendered `show-options` lines for the selected scope.
    pub fn show_options_lines(
        &self,
        scope: &OptionScopeSelector,
        value_only: bool,
    ) -> Result<Vec<String>, RmuxError> {
        self.show_options_lines_filtered(scope, None, value_only)
    }

    /// Returns rendered `show-options` lines for the selected scope, optionally filtered by name.
    pub fn show_options_lines_filtered(
        &self,
        scope: &OptionScopeSelector,
        name: Option<&str>,
        value_only: bool,
    ) -> Result<Vec<String>, RmuxError> {
        self.show_options_lines_with_mode_filtered(
            scope,
            name,
            value_only,
            ShowOptionsMode::Resolved,
        )
    }

    /// Returns rendered `show-options` lines for the selected scope and mode.
    pub fn show_options_lines_with_mode(
        &self,
        scope: &OptionScopeSelector,
        value_only: bool,
        mode: ShowOptionsMode,
    ) -> Result<Vec<String>, RmuxError> {
        self.show_options_lines_with_mode_filtered(scope, None, value_only, mode)
    }

    /// Returns rendered `show-options` lines for the selected scope, mode, and optional name.
    pub fn show_options_lines_with_mode_filtered(
        &self,
        scope: &OptionScopeSelector,
        name: Option<&str>,
        value_only: bool,
        mode: ShowOptionsMode,
    ) -> Result<Vec<String>, RmuxError> {
        let show_scope = ShowScope::from_selector(scope);
        if let Some(name) = name {
            return self.show_options_lines_for_name(&show_scope, name, value_only, mode);
        }

        let mut lines = Vec::new();

        match mode {
            ShowOptionsMode::Resolved | ShowOptionsMode::ResolvedWithInheritanceMarkers => {
                for (name, value) in self.resolved_user_values_for_show_scope(&show_scope) {
                    lines.push(render_show_line(&name, &value, value_only));
                }

                for metadata in registry()
                    .iter()
                    .filter(|metadata| metadata.visible_in(show_scope.mask()))
                {
                    let query = OptionQuery::known(metadata.option());
                    lines.extend(self.render_show_lines_for_query(
                        &show_scope,
                        &query,
                        value_only,
                        mode,
                    ));
                }
            }
            ShowOptionsMode::Explicit => {
                if let Some(node) = self.node_for_show_scope(&show_scope) {
                    for entry in node
                        .entries
                        .values()
                        .filter(|entry| entry.known_option.is_none())
                    {
                        lines.extend(render_entry_show_lines(entry, value_only));
                    }
                    for entry in node.entries.values() {
                        if entry.known_option.is_some() {
                            lines.extend(render_entry_show_lines(entry, value_only));
                        }
                    }
                }
            }
        }

        Ok(lines)
    }

    fn resolved_value_for_show_scope(
        &self,
        scope: &ShowScope,
        query: &OptionQuery,
    ) -> Option<String> {
        match scope {
            ShowScope::Server => self
                .resolve_query(&ResolveContext::Server, query)
                .and_then(|entry| entry.value(query.index()).map(str::to_owned))
                .or_else(|| self.default_value_as_string(query)),
            ShowScope::SessionGlobal => self
                .resolve_query(&ResolveContext::SessionGlobal, query)
                .and_then(|entry| entry.value(query.index()).map(str::to_owned))
                .or_else(|| self.default_value_as_string(query)),
            ShowScope::WindowGlobal => self
                .resolve_query(&ResolveContext::WindowGlobal, query)
                .and_then(|entry| entry.value(query.index()).map(str::to_owned))
                .or_else(|| self.default_value_as_string(query)),
            ShowScope::Session(session_name) => self
                .resolve_query(&ResolveContext::Session(session_name), query)
                .and_then(|entry| entry.value(query.index()).map(str::to_owned))
                .or_else(|| self.default_value_as_string(query)),
            ShowScope::Window(target) => self
                .resolve_query(
                    &ResolveContext::Window(target.session_name(), target.window_index()),
                    query,
                )
                .and_then(|entry| entry.value(query.index()).map(str::to_owned))
                .or_else(|| self.default_value_as_string(query)),
            ShowScope::Pane(target) => self
                .resolve_query(
                    &ResolveContext::Pane(
                        target.session_name(),
                        target.window_index(),
                        target.pane_index(),
                    ),
                    query,
                )
                .and_then(|entry| entry.value(query.index()).map(str::to_owned))
                .or_else(|| self.default_value_as_string(query)),
        }
    }

    fn resolved_user_values_for_show_scope(&self, scope: &ShowScope) -> Vec<(String, String)> {
        let context = match scope {
            ShowScope::Server => ResolveContext::Server,
            ShowScope::SessionGlobal => ResolveContext::SessionGlobal,
            ShowScope::WindowGlobal => ResolveContext::WindowGlobal,
            ShowScope::Session(session_name) => ResolveContext::Session(session_name),
            ShowScope::Window(target) => {
                ResolveContext::Window(target.session_name(), target.window_index())
            }
            ShowScope::Pane(target) => ResolveContext::Pane(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            ),
        };

        let mut values = BTreeMap::<String, String>::new();
        for node in self.user_chain_for_context(&context) {
            for (name, entry) in &node.entries {
                if entry.known_option.is_none() {
                    values
                        .entry(name.clone())
                        .or_insert_with(|| entry.rendered.clone());
                }
            }
        }
        values.into_iter().collect()
    }

    fn show_options_lines_for_name(
        &self,
        scope: &ShowScope,
        name: &str,
        value_only: bool,
        mode: ShowOptionsMode,
    ) -> Result<Vec<String>, RmuxError> {
        let query = resolve_option_name(name)?;
        let resolved_scope = self.show_scope_for_named_query(scope, &query);
        let mode = if mode == ShowOptionsMode::Explicit && resolved_scope.mask() != scope.mask() {
            ShowOptionsMode::Resolved
        } else {
            mode
        };
        let lines = self.render_show_lines_for_query(&resolved_scope, &query, value_only, mode);
        if query.is_user() && lines.is_empty() {
            return Err(RmuxError::Message(format!(
                "invalid option: {}",
                query.canonical_name()
            )));
        }
        if query.is_array()
            && query.index().is_none()
            && lines.is_empty()
            && !value_only
            && empty_array_shows_name(&resolved_scope, mode, &query)
        {
            return Ok(vec![query.canonical_name().to_owned()]);
        }
        Ok(lines)
    }

    fn render_show_lines_for_query(
        &self,
        scope: &ShowScope,
        query: &OptionQuery,
        value_only: bool,
        mode: ShowOptionsMode,
    ) -> Vec<String> {
        if query.is_user() {
            return match mode {
                ShowOptionsMode::Resolved | ShowOptionsMode::ResolvedWithInheritanceMarkers => self
                    .resolved_user_values_for_show_scope(scope)
                    .into_iter()
                    .find(|(entry_name, _)| entry_name == query.canonical_name())
                    .map(|(_, value)| {
                        render_show_line(
                            &self.show_name_for_mode(scope, query, mode),
                            &value,
                            value_only,
                        )
                    })
                    .into_iter()
                    .collect(),
                ShowOptionsMode::Explicit => self
                    .node_for_show_scope(scope)
                    .and_then(|node| node.value(query.canonical_name(), query.index()))
                    .map(|value| render_show_line(query.canonical_name(), value, value_only))
                    .into_iter()
                    .collect(),
            };
        }

        if query.is_array() && query.index().is_none() {
            let values = match mode {
                ShowOptionsMode::Resolved | ShowOptionsMode::ResolvedWithInheritanceMarkers => {
                    self.resolved_array_values_for_show_scope(scope, query)
                }
                ShowOptionsMode::Explicit => self
                    .node_for_show_scope(scope)
                    .and_then(|node| node.entry(query.canonical_name()))
                    .map(OptionEntry::array_entries)
                    .unwrap_or_default(),
            };
            if values.is_empty() && !value_only && empty_array_shows_name(scope, mode, query) {
                return vec![query.canonical_name().to_owned()];
            }
            return values
                .into_iter()
                .map(|(index, value)| {
                    let name = show_option_name(query.canonical_name(), Some(index));
                    let name =
                        self.show_index_name_for_mode(scope, query, Some(index), mode, &name);
                    render_known_show_line(query, &name, &value, value_only)
                })
                .collect();
        }

        let value = match mode {
            ShowOptionsMode::Resolved | ShowOptionsMode::ResolvedWithInheritanceMarkers => {
                self.resolved_value_for_show_scope(scope, query)
            }
            ShowOptionsMode::Explicit => self
                .node_for_show_scope(scope)
                .and_then(|node| node.value(query.canonical_name(), query.index()))
                .map(str::to_owned),
        };

        value
            .into_iter()
            .map(|value| {
                let name = show_option_name(query.canonical_name(), query.index());
                let name = self.show_index_name_for_mode(scope, query, query.index(), mode, &name);
                render_known_show_line(query, &name, &value, value_only)
            })
            .collect()
    }

    fn show_name_for_mode(
        &self,
        scope: &ShowScope,
        query: &OptionQuery,
        mode: ShowOptionsMode,
    ) -> String {
        let name = query.canonical_name();
        self.show_index_name_for_mode(scope, query, query.index(), mode, name)
    }

    fn show_index_name_for_mode(
        &self,
        scope: &ShowScope,
        query: &OptionQuery,
        index: Option<u32>,
        mode: ShowOptionsMode,
        name: &str,
    ) -> String {
        if mode != ShowOptionsMode::ResolvedWithInheritanceMarkers {
            return name.to_owned();
        }
        if matches!(
            scope,
            ShowScope::Server | ShowScope::SessionGlobal | ShowScope::WindowGlobal
        ) {
            return name.to_owned();
        }
        if self
            .node_for_show_scope(scope)
            .is_some_and(|node| node.contains(query.canonical_name(), index))
        {
            name.to_owned()
        } else {
            format!("{name}*")
        }
    }

    fn resolved_array_values_for_show_scope(
        &self,
        scope: &ShowScope,
        query: &OptionQuery,
    ) -> Vec<(u32, String)> {
        let resolved = match scope {
            ShowScope::Server => self.resolve_query(&ResolveContext::Server, query),
            ShowScope::SessionGlobal => self.resolve_query(&ResolveContext::SessionGlobal, query),
            ShowScope::WindowGlobal => self.resolve_query(&ResolveContext::WindowGlobal, query),
            ShowScope::Session(session_name) => {
                self.resolve_query(&ResolveContext::Session(session_name), query)
            }
            ShowScope::Window(target) => self.resolve_query(
                &ResolveContext::Window(target.session_name(), target.window_index()),
                query,
            ),
            ShowScope::Pane(target) => self.resolve_query(
                &ResolveContext::Pane(
                    target.session_name(),
                    target.window_index(),
                    target.pane_index(),
                ),
                query,
            ),
        };

        resolved
            .map(OptionEntry::array_entries)
            .unwrap_or_else(|| default_array_show_values(query))
    }

    fn show_scope_for_named_query<'a>(
        &self,
        scope: &ShowScope<'a>,
        query: &OptionQuery,
    ) -> ShowScope<'a> {
        let Some(metadata) = query.metadata() else {
            return *scope;
        };
        if metadata.visible_in(scope.mask()) {
            return *scope;
        }

        if self
            .node_for_show_scope(scope)
            .is_some_and(|node| node.contains(query.canonical_name(), query.index()))
        {
            return *scope;
        }

        match metadata.global_root() {
            GlobalRoot::Server => ShowScope::Server,
            GlobalRoot::Session => ShowScope::SessionGlobal,
            GlobalRoot::Window => ShowScope::WindowGlobal,
        }
    }

    fn node_for_show_scope(&self, scope: &ShowScope) -> Option<&OptionNode> {
        match scope {
            ShowScope::Server => Some(&self.server_global),
            ShowScope::SessionGlobal => Some(&self.session_global),
            ShowScope::WindowGlobal => Some(&self.window_global),
            ShowScope::Session(session_name) => self.sessions.get(session_name),
            ShowScope::Window(target) => self.windows.get(target),
            ShowScope::Pane(target) => self.panes.get(target),
        }
    }

    fn user_chain_for_context<'a>(&'a self, context: &ResolveContext<'a>) -> Vec<&'a OptionNode> {
        match context {
            ResolveContext::Server => vec![&self.server_global],
            ResolveContext::SessionGlobal => vec![&self.session_global, &self.server_global],
            ResolveContext::WindowGlobal => {
                vec![
                    &self.window_global,
                    &self.session_global,
                    &self.server_global,
                ]
            }
            ResolveContext::Session(session_name) => {
                let mut chain = Vec::new();
                if let Some(node) = self.sessions.get(*session_name) {
                    chain.push(node);
                }
                chain.push(&self.session_global);
                chain.push(&self.server_global);
                chain
            }
            ResolveContext::Window(session_name, window_index) => {
                let mut chain = Vec::new();
                let target = WindowTarget::with_window((*session_name).clone(), *window_index);
                if let Some(node) = self.windows.get(&target) {
                    chain.push(node);
                }
                if let Some(node) = self.sessions.get(*session_name) {
                    chain.push(node);
                }
                chain.push(&self.window_global);
                chain.push(&self.session_global);
                chain.push(&self.server_global);
                chain
            }
            ResolveContext::Pane(session_name, window_index, pane_index) => {
                let mut chain = Vec::new();
                let target =
                    PaneTarget::with_window((*session_name).clone(), *window_index, *pane_index);
                if let Some(node) = self.panes.get(&target) {
                    chain.push(node);
                }
                let window_target =
                    WindowTarget::with_window((*session_name).clone(), *window_index);
                if let Some(node) = self.windows.get(&window_target) {
                    chain.push(node);
                }
                if let Some(node) = self.sessions.get(*session_name) {
                    chain.push(node);
                }
                chain.push(&self.window_global);
                chain.push(&self.session_global);
                chain.push(&self.server_global);
                chain
            }
        }
    }
}

fn empty_array_shows_name(
    scope: &ShowScope<'_>,
    mode: ShowOptionsMode,
    query: &OptionQuery,
) -> bool {
    query.is_array()
        && matches!(
            (scope, mode),
            (
                ShowScope::Server | ShowScope::SessionGlobal | ShowScope::WindowGlobal,
                ShowOptionsMode::Resolved | ShowOptionsMode::ResolvedWithInheritanceMarkers
            ) | (
                ShowScope::Window(_) | ShowScope::Pane(_),
                ShowOptionsMode::ResolvedWithInheritanceMarkers
            )
        )
}
