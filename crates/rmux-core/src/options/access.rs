use std::collections::HashMap;

use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{OptionName, PaneTarget, SessionName, WindowTarget};

use super::mutation::{
    default_array_items, default_scalar_text, normalize_scalar_value, split_array_assignment,
};
use super::registry::{
    option_metadata, registry, resolve_exact_option_name, resolve_option_name, DefaultValue,
    GlobalRoot,
};
use super::render::format_rendered_option_value;
use super::scope::{
    push_known_global_roots, scope_allows_pane, scope_allows_session, scope_allows_window,
    ResolveContext,
};
use super::storage::{OptionEntry, OptionNode};
use super::{OptionQuery, OptionStore};

impl OptionStore {
    /// Returns the exact explicit global value for the given known option.
    #[must_use]
    pub fn global_value(&self, option: OptionName) -> Option<&str> {
        let query = OptionQuery::known(option);
        self.node_for_global_root(option_metadata(option).global_root())
            .and_then(|node| node.value(query.canonical_name(), None))
    }

    /// Returns the exact session-local value for the given known option.
    #[must_use]
    pub fn session_value(&self, session_name: &SessionName, option: OptionName) -> Option<&str> {
        let query = OptionQuery::known(option);
        self.sessions
            .get(session_name)
            .and_then(|node| node.value(query.canonical_name(), None))
    }

    /// Returns the exact window-local value for the given known option.
    #[must_use]
    pub fn window_value(&self, target: &WindowTarget, option: OptionName) -> Option<&str> {
        let query = OptionQuery::known(option);
        self.windows
            .get(target)
            .and_then(|node| node.value(query.canonical_name(), None))
    }

    /// Returns the exact pane-local value for the given known option.
    #[must_use]
    pub fn pane_value(&self, target: &PaneTarget, option: OptionName) -> Option<&str> {
        let query = OptionQuery::known(option);
        self.panes
            .get(target)
            .and_then(|node| node.value(query.canonical_name(), None))
    }

    /// Resolves the effective value using the tmux-style parent chain for the known option.
    #[must_use]
    pub fn resolve<'a>(
        &'a self,
        session_name: Option<&'a SessionName>,
        option: OptionName,
    ) -> Option<&'a str> {
        let query = OptionQuery::known(option);
        let resolved = match session_name {
            Some(session_name) => self
                .resolve_query(&ResolveContext::Session(session_name), &query)
                .map(OptionEntry::rendered),
            None => self
                .resolve_query(&ResolveContext::Server, &query)
                .map(OptionEntry::rendered),
        };
        if let Some(resolved) = resolved {
            Some(resolved)
        } else {
            self.default_rendered_value(&query)
        }
    }

    /// Resolves the effective window value using the tmux-style parent chain.
    #[must_use]
    pub fn resolve_for_window<'a>(
        &'a self,
        session_name: &'a SessionName,
        window_index: u32,
        option: OptionName,
    ) -> Option<&'a str> {
        let query = OptionQuery::known(option);
        let resolved = self
            .resolve_query(&ResolveContext::Window(session_name, window_index), &query)
            .map(OptionEntry::rendered);
        if let Some(resolved) = resolved {
            Some(resolved)
        } else {
            self.default_rendered_value(&query)
        }
    }

    /// Resolves the effective pane value using the tmux-style parent chain.
    #[must_use]
    pub fn resolve_for_pane<'a>(
        &'a self,
        session_name: &'a SessionName,
        window_index: u32,
        pane_index: u32,
        option: OptionName,
    ) -> Option<&'a str> {
        let query = OptionQuery::known(option);
        let resolved = self
            .resolve_query(
                &ResolveContext::Pane(session_name, window_index, pane_index),
                &query,
            )
            .map(OptionEntry::rendered);
        if let Some(resolved) = resolved {
            Some(resolved)
        } else {
            self.default_rendered_value(&query)
        }
    }

    /// Resolves the effective array values for the known option.
    #[must_use]
    pub fn resolve_array_values(
        &self,
        session_name: Option<&SessionName>,
        option: OptionName,
    ) -> Vec<String> {
        let query = OptionQuery::known(option);
        if !query.is_array() {
            return Vec::new();
        }

        let resolved = match session_name {
            Some(session_name) => self
                .resolve_query(&ResolveContext::Session(session_name), &query)
                .map(OptionEntry::array_values),
            None => self
                .resolve_query(&ResolveContext::Server, &query)
                .map(OptionEntry::array_values),
        };
        if let Some(resolved) = resolved {
            return resolved;
        }

        match query.default_value() {
            Some(DefaultValue::Scalar(value)) => split_array_assignment(value, query.separator()),
            Some(DefaultValue::Array(values)) => {
                values.iter().map(|value| (*value).to_owned()).collect()
            }
            None => Vec::new(),
        }
    }

    /// Resolves a string-keyed option in a session context.
    #[must_use]
    pub fn resolve_name(&self, session_name: Option<&SessionName>, name: &str) -> Option<String> {
        let query = resolve_option_name(name).ok()?;
        match session_name {
            // tmux format.c::format_find checks server options first, then the
            // context-specific tree, then any wider parents for that context.
            Some(session_name) => self
                .resolve_name_from_nodes(
                    &query,
                    [
                        Some(&self.server_global),
                        self.sessions.get(session_name),
                        Some(&self.session_global),
                    ],
                )
                .or_else(|| self.default_value_as_string(&query)),
            None => self
                .resolve_name_from_nodes(&query, [Some(&self.server_global)])
                .or_else(|| self.default_value_as_string(&query)),
        }
    }

    /// Resolves a string-keyed option for tmux format evaluation in a session context.
    #[must_use]
    pub fn resolve_name_for_format(
        &self,
        session_name: Option<&SessionName>,
        name: &str,
    ) -> Option<String> {
        let query = resolve_exact_option_name(name).ok()?;
        let value = match session_name {
            Some(session_name) => self
                .resolve_name_from_nodes(
                    &query,
                    [
                        Some(&self.server_global),
                        self.sessions.get(session_name),
                        Some(&self.session_global),
                    ],
                )
                .or_else(|| self.default_value_as_string(&query)),
            None => self
                .resolve_name_from_nodes(
                    &query,
                    [
                        Some(&self.server_global),
                        Some(&self.session_global),
                        Some(&self.window_global),
                    ],
                )
                .or_else(|| self.default_value_as_string(&query)),
        }?;
        Some(format_rendered_option_value(&query, value))
    }

    /// Resolves a string-keyed option in a window context.
    #[must_use]
    pub fn resolve_name_for_window(
        &self,
        session_name: &SessionName,
        window_index: u32,
        name: &str,
    ) -> Option<String> {
        let query = resolve_option_name(name).ok()?;
        let target = WindowTarget::with_window(session_name.clone(), window_index);
        self.resolve_name_from_nodes(
            &query,
            [
                Some(&self.server_global),
                self.windows.get(&target),
                Some(&self.window_global),
                self.sessions.get(session_name),
                Some(&self.session_global),
            ],
        )
        .or_else(|| self.default_value_as_string(&query))
    }

    /// Resolves a string-keyed option for tmux format evaluation in a window context.
    #[must_use]
    pub fn resolve_name_for_window_format(
        &self,
        session_name: &SessionName,
        window_index: u32,
        name: &str,
    ) -> Option<String> {
        let query = resolve_exact_option_name(name).ok()?;
        let target = WindowTarget::with_window(session_name.clone(), window_index);
        let value = self
            .resolve_name_from_nodes(
                &query,
                [
                    Some(&self.server_global),
                    self.windows.get(&target),
                    Some(&self.window_global),
                    self.sessions.get(session_name),
                    Some(&self.session_global),
                ],
            )
            .or_else(|| self.default_value_as_string(&query))?;
        Some(format_rendered_option_value(&query, value))
    }

    /// Resolves a string-keyed option in a pane context.
    #[must_use]
    pub fn resolve_name_for_pane(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
        name: &str,
    ) -> Option<String> {
        let query = resolve_option_name(name).ok()?;
        let pane_target = PaneTarget::with_window(session_name.clone(), window_index, pane_index);
        let window_target = WindowTarget::with_window(session_name.clone(), window_index);
        self.resolve_name_from_nodes(
            &query,
            [
                Some(&self.server_global),
                self.panes.get(&pane_target),
                self.windows.get(&window_target),
                Some(&self.window_global),
                self.sessions.get(session_name),
                Some(&self.session_global),
            ],
        )
        .or_else(|| self.default_value_as_string(&query))
    }

    /// Resolves a string-keyed option for tmux format evaluation in a pane context.
    #[must_use]
    pub fn resolve_name_for_pane_format(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
        name: &str,
    ) -> Option<String> {
        let query = resolve_exact_option_name(name).ok()?;
        let pane_target = PaneTarget::with_window(session_name.clone(), window_index, pane_index);
        let window_target = WindowTarget::with_window(session_name.clone(), window_index);
        let value = self
            .resolve_name_from_nodes(
                &query,
                [
                    Some(&self.server_global),
                    self.panes.get(&pane_target),
                    self.windows.get(&window_target),
                    Some(&self.window_global),
                    self.sessions.get(session_name),
                    Some(&self.session_global),
                ],
            )
            .or_else(|| self.default_value_as_string(&query))?;
        Some(format_rendered_option_value(&query, value))
    }

    /// Returns the option snapshot that future panes in a session should inherit.
    #[must_use]
    pub fn resolved(&self, session_name: &SessionName) -> HashMap<OptionName, String> {
        registry()
            .iter()
            .map(|metadata| {
                let query = OptionQuery::known(metadata.option());
                let value = self
                    .resolve(Some(session_name), metadata.option())
                    .map(str::to_owned)
                    .or_else(|| self.default_value_as_string(&query))
                    .unwrap_or_default();
                (metadata.option(), value)
            })
            .collect()
    }

    /// Returns the option snapshot for a pane after the full inheritance chain is applied.
    #[must_use]
    pub fn resolved_for_pane(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> HashMap<OptionName, String> {
        registry()
            .iter()
            .map(|metadata| {
                let query = OptionQuery::known(metadata.option());
                let value = self
                    .resolve_for_pane(session_name, window_index, pane_index, metadata.option())
                    .map(str::to_owned)
                    .or_else(|| self.default_value_as_string(&query))
                    .unwrap_or_default();
                (metadata.option(), value)
            })
            .collect()
    }

    pub(super) fn node_for_exact_scope_mut(
        &mut self,
        scope: &OptionScopeSelector,
    ) -> &mut OptionNode {
        match scope {
            OptionScopeSelector::ServerGlobal => &mut self.server_global,
            OptionScopeSelector::SessionGlobal => &mut self.session_global,
            OptionScopeSelector::WindowGlobal => &mut self.window_global,
            OptionScopeSelector::Session(session_name) => {
                self.sessions.entry(session_name.clone()).or_default()
            }
            OptionScopeSelector::Window(target) => self.windows.entry(target.clone()).or_default(),
            OptionScopeSelector::Pane(target) => self.panes.entry(target.clone()).or_default(),
        }
    }

    pub(super) fn node_for_exact_scope(&self, scope: &OptionScopeSelector) -> Option<&OptionNode> {
        match scope {
            OptionScopeSelector::ServerGlobal => Some(&self.server_global),
            OptionScopeSelector::SessionGlobal => Some(&self.session_global),
            OptionScopeSelector::WindowGlobal => Some(&self.window_global),
            OptionScopeSelector::Session(session_name) => self.sessions.get(session_name),
            OptionScopeSelector::Window(target) => self.windows.get(target),
            OptionScopeSelector::Pane(target) => self.panes.get(target),
        }
    }

    pub(super) fn resolve_query<'a>(
        &'a self,
        context: &ResolveContext<'a>,
        query: &OptionQuery,
    ) -> Option<&'a OptionEntry> {
        for node in self.chain_for_context(context, query) {
            if let Some(entry) = node.entry(query.canonical_name()) {
                if query.index().is_none() || entry.value(query.index()).is_some() {
                    return Some(entry);
                }
            }
        }
        None
    }

    pub(super) fn effective_value_for_scope(
        &self,
        scope: &OptionScopeSelector,
        query: &OptionQuery,
    ) -> Option<String> {
        match scope {
            OptionScopeSelector::ServerGlobal => self
                .resolve_query(&ResolveContext::Server, query)
                .and_then(|entry| entry.value(query.index()).map(str::to_owned)),
            OptionScopeSelector::SessionGlobal => self
                .resolve_query(&ResolveContext::SessionGlobal, query)
                .and_then(|entry| entry.value(query.index()).map(str::to_owned)),
            OptionScopeSelector::WindowGlobal => self
                .resolve_query(&ResolveContext::WindowGlobal, query)
                .and_then(|entry| entry.value(query.index()).map(str::to_owned)),
            OptionScopeSelector::Session(session_name) => self
                .resolve_query(&ResolveContext::Session(session_name), query)
                .and_then(|entry| entry.value(query.index()).map(str::to_owned)),
            OptionScopeSelector::Window(target) => self
                .resolve_query(
                    &ResolveContext::Window(target.session_name(), target.window_index()),
                    query,
                )
                .and_then(|entry| entry.value(query.index()).map(str::to_owned)),
            OptionScopeSelector::Pane(target) => self
                .resolve_query(
                    &ResolveContext::Pane(
                        target.session_name(),
                        target.window_index(),
                        target.pane_index(),
                    ),
                    query,
                )
                .and_then(|entry| entry.value(query.index()).map(str::to_owned)),
        }
    }

    pub(super) fn explicit_value_for_scope(
        &self,
        scope: &OptionScopeSelector,
        query: &OptionQuery,
    ) -> Option<String> {
        self.node_for_exact_scope(scope)
            .and_then(|node| node.value(query.canonical_name(), query.index()))
            .map(str::to_owned)
    }

    pub(super) fn default_entry_for_scope(
        &self,
        query: &OptionQuery,
        scope: OptionScopeSelector,
    ) -> Option<OptionEntry> {
        let default = query.default_value()?;

        if query.is_array() {
            let items = default_array_items(query, default).ok()?;
            Some(OptionEntry::new_array(query, scope, items))
        } else {
            let value =
                normalize_scalar_value(query, Some(default_scalar_text(default)), None).ok()?;
            Some(OptionEntry::new_scalar(query, scope, value))
        }
    }

    pub(super) fn default_value_as_string(&self, query: &OptionQuery) -> Option<String> {
        let default = query.default_value()?;
        match default {
            DefaultValue::Scalar(value) => Some(value.to_owned()),
            DefaultValue::Array(values) => Some(values.join(query.separator())),
        }
    }

    fn node_for_global_root(&self, root: GlobalRoot) -> Option<&OptionNode> {
        match root {
            GlobalRoot::Server => Some(&self.server_global),
            GlobalRoot::Session => Some(&self.session_global),
            GlobalRoot::Window => Some(&self.window_global),
        }
    }

    fn chain_for_context<'a>(
        &'a self,
        context: &ResolveContext<'a>,
        query: &OptionQuery,
    ) -> Vec<&'a OptionNode> {
        match context {
            ResolveContext::Server => vec![&self.server_global],
            // tmux global roots are independent; known options resolve to their
            // own global root only. User options cross all roots.
            ResolveContext::SessionGlobal => {
                if query.is_user() {
                    vec![&self.session_global, &self.server_global]
                } else {
                    vec![&self.session_global]
                }
            }
            ResolveContext::WindowGlobal => {
                if query.is_user() {
                    vec![
                        &self.window_global,
                        &self.session_global,
                        &self.server_global,
                    ]
                } else {
                    vec![&self.window_global]
                }
            }
            ResolveContext::Session(session_name) => {
                let mut chain = Vec::new();
                if let Some(node) = self.sessions.get(*session_name) {
                    if scope_allows_session(query)
                        || node.contains(query.canonical_name(), query.index())
                    {
                        chain.push(node);
                    }
                }
                push_known_global_roots(&mut chain, self, query);
                chain
            }
            ResolveContext::Window(session_name, window_index) => {
                let mut chain = Vec::new();
                let target = WindowTarget::with_window((*session_name).clone(), *window_index);
                if let Some(node) = self.windows.get(&target) {
                    if scope_allows_window(query)
                        || node.contains(query.canonical_name(), query.index())
                    {
                        chain.push(node);
                    }
                }
                if let Some(node) = self.sessions.get(*session_name) {
                    if scope_allows_session(query)
                        || node.contains(query.canonical_name(), query.index())
                    {
                        chain.push(node);
                    }
                }
                push_known_global_roots(&mut chain, self, query);
                chain
            }
            ResolveContext::Pane(session_name, window_index, pane_index) => {
                let mut chain = Vec::new();
                let pane_target =
                    PaneTarget::with_window((*session_name).clone(), *window_index, *pane_index);
                if let Some(node) = self.panes.get(&pane_target) {
                    if scope_allows_pane(query)
                        || node.contains(query.canonical_name(), query.index())
                    {
                        chain.push(node);
                    }
                }
                let window_target =
                    WindowTarget::with_window((*session_name).clone(), *window_index);
                if let Some(node) = self.windows.get(&window_target) {
                    if scope_allows_window(query)
                        || node.contains(query.canonical_name(), query.index())
                    {
                        chain.push(node);
                    }
                }
                if let Some(node) = self.sessions.get(*session_name) {
                    if scope_allows_session(query)
                        || node.contains(query.canonical_name(), query.index())
                    {
                        chain.push(node);
                    }
                }
                push_known_global_roots(&mut chain, self, query);
                chain
            }
        }
    }

    fn default_rendered_value(&self, query: &OptionQuery) -> Option<&'static str> {
        if query.index().is_some() {
            return None;
        }
        match query.default_value()? {
            DefaultValue::Scalar(value) => Some(value),
            // Array defaults with DefaultValue::Array require a computed join;
            // the static-str callers fall back to default_value_as_string for
            // those options. Currently all array options use Scalar defaults.
            DefaultValue::Array(_) => None,
        }
    }

    fn resolve_name_from_nodes<'a, I>(&self, query: &OptionQuery, nodes: I) -> Option<String>
    where
        I: IntoIterator<Item = Option<&'a OptionNode>>,
    {
        for node in nodes.into_iter().flatten() {
            if let Some(entry) = node.entry(query.canonical_name()) {
                if let Some(value) = entry.value(query.index()) {
                    return Some(value.to_owned());
                }
            }
        }
        None
    }
}
