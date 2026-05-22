use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{PaneTarget, RmuxError, SessionName, WindowTarget};

use super::registry::{
    self, resolve_option_name, GlobalRoot, SHOW_PANE, SHOW_SERVER, SHOW_SESSION, SHOW_WINDOW,
};
use super::storage::OptionNode;
use super::{OptionQuery, OptionStore};

/// Returns the tmux-compatible global option tree for a string option name.
///
/// `set -g` and `show-options -g` are not always session-global in tmux:
/// their global tree follows the option itself. User options keep rmux's
/// existing session-global default because they do not have registry metadata.
pub fn default_global_scope_for_option_name(name: &str) -> Result<OptionScopeSelector, RmuxError> {
    let query = resolve_option_name(name)?;
    Ok(
        match query.metadata().map(|metadata| metadata.global_root()) {
            Some(GlobalRoot::Server) => OptionScopeSelector::ServerGlobal,
            Some(GlobalRoot::Session) | None => OptionScopeSelector::SessionGlobal,
            Some(GlobalRoot::Window) => OptionScopeSelector::WindowGlobal,
        },
    )
}

pub(super) fn scope_allows_session(query: &OptionQuery) -> bool {
    query.is_user()
        || query
            .metadata()
            .is_some_and(|metadata| metadata.scope_mask() & registry::SCOPE_SESSION != 0)
}

pub(super) fn scope_allows_window(query: &OptionQuery) -> bool {
    query.is_user()
        || query
            .metadata()
            .is_some_and(|metadata| metadata.scope_mask() & registry::SCOPE_WINDOW != 0)
}

pub(super) fn scope_allows_pane(query: &OptionQuery) -> bool {
    query.is_user()
        || query
            .metadata()
            .is_some_and(|metadata| metadata.scope_mask() & registry::SCOPE_PANE != 0)
}

pub(super) fn push_known_global_roots<'a>(
    chain: &mut Vec<&'a OptionNode>,
    store: &'a OptionStore,
    query: &OptionQuery,
) {
    if query.is_user() {
        chain.push(&store.window_global);
        chain.push(&store.session_global);
        chain.push(&store.server_global);
        return;
    }

    // tmux parent chains: each global root is independent with no parent.
    // global_options (server) - no parent
    // global_s_options (session globals) - no parent
    // global_w_options (window globals) - no parent
    match query.metadata().expect("known query").global_root() {
        GlobalRoot::Server => chain.push(&store.server_global),
        GlobalRoot::Session => chain.push(&store.session_global),
        GlobalRoot::Window => chain.push(&store.window_global),
    }
}

pub(super) enum ResolveContext<'a> {
    Server,
    SessionGlobal,
    WindowGlobal,
    Session(&'a SessionName),
    Window(&'a SessionName, u32),
    Pane(&'a SessionName, u32, u32),
}

#[derive(Clone, Copy)]
pub(super) enum ShowScope<'a> {
    Server,
    SessionGlobal,
    WindowGlobal,
    Session(&'a SessionName),
    Window(&'a WindowTarget),
    Pane(&'a PaneTarget),
}

impl<'a> ShowScope<'a> {
    pub(super) fn from_selector(scope: &'a OptionScopeSelector) -> Self {
        match scope {
            OptionScopeSelector::ServerGlobal => Self::Server,
            OptionScopeSelector::SessionGlobal => Self::SessionGlobal,
            OptionScopeSelector::WindowGlobal => Self::WindowGlobal,
            OptionScopeSelector::Session(session_name) => Self::Session(session_name),
            OptionScopeSelector::Window(target) => Self::Window(target),
            OptionScopeSelector::Pane(target) => Self::Pane(target),
        }
    }

    pub(super) fn mask(&self) -> u8 {
        match self {
            Self::Server => SHOW_SERVER,
            Self::SessionGlobal | Self::Session(_) => SHOW_SESSION,
            Self::WindowGlobal | Self::Window(_) => SHOW_WINDOW,
            Self::Pane(_) => SHOW_PANE,
        }
    }
}
