use std::collections::BTreeMap;
use std::fmt;

use crate::{SessionId, SessionName, Window, WindowId, WindowLayoutContext};

mod link_order;
mod reshape;
pub(crate) use link_order::LinkOrderPlan;
pub(crate) use reshape::SessionReshape;

/// A window index in a session, independent of the window's stable identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WindowLink {
    /// Stable identity of the session containing the link.
    pub session_id: SessionId,
    /// Session-local display index.
    pub window_index: u32,
}

/// A rejected owner or link operation; the store is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowStateError {
    /// This identity already has an owner.
    DuplicateWindow(WindowId),
    /// This session/index already contains a link.
    OccupiedLink(WindowLink),
    /// No owned window has this identity.
    MissingWindow(WindowId),
    /// This session/index contains no link.
    MissingLink(WindowLink),
}

impl fmt::Display for WindowStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateWindow(id) => write!(f, "window {id} is already owned"),
            Self::OccupiedLink(link) => write!(f, "window link {link:?} is occupied"),
            Self::MissingWindow(id) => write!(f, "window {id} is not owned"),
            Self::MissingLink(link) => write!(f, "window link {link:?} is missing"),
        }
    }
}

impl std::error::Error for WindowStateError {}

/// Failed introduction, returning the supplied window without cloning it.
#[derive(Debug)]
pub struct IntroduceWindowError {
    /// Reason the introduction failed.
    pub reason: WindowStateError,
    /// The rejected owner, available for retry or transfer elsewhere.
    pub window: Box<Window>,
}

impl fmt::Display for IntroduceWindowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.reason.fmt(f)
    }
}

impl std::error::Error for IntroduceWindowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.reason)
    }
}

/// Owns one window per stable identity and all links to those owners.
///
/// Every owner has at least one link. Session identities are opaque keys here;
/// this primitive does not create sessions or validate their existence. It is
/// not yet used by server consumers or by `Session`/`SessionStore`.
/// Mutations validate before changing either table. Exclusive borrowing keeps
/// the combined update unobservable until both tables agree.
#[derive(Debug, Default)]
pub struct WindowStateStore {
    owners: BTreeMap<WindowId, Box<Window>>,
    links: BTreeMap<WindowLink, WindowId>,
    /// The order each window's links were MADE in, when it is known.
    ///
    /// `links` is a map and has no order of its own beyond its keys; tmux prints
    /// one session name per winlink in the order the links were made
    /// (`format.c:686-714`, `window.c:183-193`), which no sort of identities or
    /// names reconstructs. This records it, as the concrete links themselves -
    /// duplicates included, because two links of one window are two names.
    ///
    /// A window ABSENT from this map has an order this store does not know, and
    /// the reader must fall back rather than invent one. Nothing is ever derived
    /// from the iteration order of `links`.
    order: BTreeMap<WindowId, Vec<WindowLink>>,
}

impl WindowStateStore {
    pub(crate) fn render_snapshot(&self) -> Self {
        Self {
            owners: self.owners.clone(),
            links: self.links.clone(),
            order: self.order.clone(),
        }
    }

    /// Creates an empty owner/link store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Introduces a window and its first link, preserving window and pane IDs.
    /// On failure, returns the supplied window and leaves both tables unchanged.
    pub fn introduce(
        &mut self,
        link: WindowLink,
        window: Window,
    ) -> Result<WindowId, IntroduceWindowError> {
        let id = window.id();
        let reason = if self.owners.contains_key(&id) {
            Some(WindowStateError::DuplicateWindow(id))
        } else if self.links.contains_key(&link) {
            Some(WindowStateError::OccupiedLink(link))
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(IntroduceWindowError {
                reason,
                window: Box::new(window),
            });
        }
        self.owners.insert(id, Box::new(window));
        self.links.insert(link, id);
        self.order.insert(id, vec![link]);
        Ok(id)
    }

    /// Adds a link to an existing owner without copying any window state.
    pub fn link(&mut self, link: WindowLink, id: WindowId) -> Result<(), WindowStateError> {
        if self.links.contains_key(&link) {
            return Err(WindowStateError::OccupiedLink(link));
        }
        if !self.owners.contains_key(&id) {
            return Err(WindowStateError::MissingWindow(id));
        }
        self.links.insert(link, id);
        // A link made now goes at the tail, which is where tmux puts it.
        if let Some(made) = self.order.get_mut(&id) {
            made.push(link);
        }
        Ok(())
    }

    /// Borrows the canonical window through a session/index link.
    pub fn resolve(&self, link: WindowLink) -> Result<&Window, WindowStateError> {
        let id = self.linked_id(link)?;
        Ok(self.owners[&id].as_ref())
    }

    /// Borrows layout mutation access without exposing owner replacement.
    /// The name is used only for context diagnostics, not link resolution.
    pub fn layout_context(
        &mut self,
        link: WindowLink,
        session_name: SessionName,
    ) -> Result<WindowLayoutContext<'_>, WindowStateError> {
        let id = self.linked_id(link)?;
        let window = self.owners.get_mut(&id).expect("every link has an owner");
        Ok(WindowLayoutContext::new(
            session_name,
            link.window_index,
            window,
        ))
    }

    /// Removes a link, transferring the owner only when its final link is gone.
    /// Other links retain the same allocation and all window/pane state.
    pub fn unlink(&mut self, link: WindowLink) -> Result<Option<Box<Window>>, WindowStateError> {
        let id = self.linked_id(link)?;
        self.links.remove(&link);
        if let Some(made) = self.order.get_mut(&id) {
            // Only this occurrence goes; a window linked twice by one session
            // keeps its other name.
            if let Some(at) = made.iter().position(|entry| *entry == link) {
                made.remove(at);
            }
        }
        if self.links.values().any(|linked_id| *linked_id == id) {
            Ok(None)
        } else {
            self.order.remove(&id);
            Ok(Some(
                self.owners.remove(&id).expect("every link has an owner"),
            ))
        }
    }

    /// Restores scoped link tables, the original removed allocations and the
    /// EXACT order each captured window's links were made in - including the
    /// windows whose order was not known when the checkpoint was taken, which
    /// come back not known rather than invented.
    ///
    /// Everything is validated before anything is written, the captured order
    /// included: an order that names a link the restored tables do not have, or
    /// misses one they do, is an internal disagreement and is refused with both
    /// tables and both orders untouched.
    #[allow(clippy::vec_box)] // Reinstall original canonical allocations without moving windows.
    pub(crate) fn restore_links(
        &mut self,
        tables: &BTreeMap<SessionId, BTreeMap<u32, WindowId>>,
        removed: &mut Vec<Box<Window>>,
        order: &BTreeMap<WindowId, Option<Vec<WindowLink>>>,
    ) -> Result<(), WindowStateError> {
        for id in tables.values().flat_map(|table| table.values()) {
            if !self.owners.contains_key(id) && !removed.iter().any(|window| window.id() == *id) {
                return Err(WindowStateError::MissingWindow(*id));
            }
        }
        let mut retained_ids = std::collections::BTreeSet::new();
        for window in removed.iter() {
            if self.owners.contains_key(&window.id()) || !retained_ids.insert(window.id()) {
                return Err(WindowStateError::DuplicateWindow(window.id()));
            }
        }
        let restored = self.restored_links(tables);
        for (id, made) in order {
            let Some(made) = made else {
                continue;
            };
            let mut named = made.clone();
            named.sort_unstable();
            let mut present = restored
                .iter()
                .filter(|(_, linked)| *linked == id)
                .map(|(link, _)| *link)
                .collect::<Vec<_>>();
            present.sort_unstable();
            if named != present {
                return Err(WindowStateError::MissingWindow(*id));
            }
        }
        for window in removed.drain(..) {
            self.owners.insert(window.id(), window);
        }
        // The replacement itself can vouch for nothing: it is undoing a change
        // whose shape it was not told. Every window it touches loses its order,
        // and the captured one is then written back exactly, `None` included.
        let _new_owners = self.replace_session_links(tables, None, LinkOrderPlan::Rebuilt)?;
        for (id, made) in order {
            match made {
                Some(made) => {
                    self.order.insert(*id, made.clone());
                }
                None => {
                    self.order.remove(id);
                }
            }
        }
        Ok(())
    }

    /// The complete link map this scoped replacement would leave behind.
    fn restored_links(
        &self,
        tables: &BTreeMap<SessionId, BTreeMap<u32, WindowId>>,
    ) -> BTreeMap<WindowLink, WindowId> {
        let mut links = self.links.clone();
        links.retain(|link, _| !tables.contains_key(&link.session_id));
        for (&session_id, table) in tables {
            for (&window_index, &id) in table {
                links.insert(
                    WindowLink {
                        session_id,
                        window_index,
                    },
                    id,
                );
            }
        }
        links
    }

    // Restricted to core contexts: callers must not replace a canonical owner.
    pub(crate) fn resolve_mut(
        &mut self,
        link: WindowLink,
    ) -> Result<&mut Window, WindowStateError> {
        let id = self.linked_id(link)?;
        Ok(self.owners.get_mut(&id).expect("every link has an owner"))
    }

    // Distinct canonical identities are checked by the transfer boundary.
    pub(crate) fn transfer_pair_mut(
        &mut self,
        source: WindowId,
        target: WindowId,
    ) -> (&mut Window, &mut Window) {
        assert_ne!(source, target);
        let mut owners = self
            .owners
            .iter_mut()
            .filter(|(id, _)| **id == source || **id == target);
        let (first_id, first) = owners.next().expect("validated owner");
        let (_, second) = owners.next().expect("validated owner");
        if *first_id == source {
            (first, second)
        } else {
            (second, first)
        }
    }

    /// Iterates a session's borrowed windows in local index order, including aliases.
    pub fn session_windows(
        &self,
        session_id: SessionId,
    ) -> impl DoubleEndedIterator<Item = (u32, &Window)> {
        let first = WindowLink {
            session_id,
            window_index: 0,
        };
        let last = WindowLink {
            session_id,
            window_index: u32::MAX,
        };
        self.links
            .range(first..=last)
            .map(|(link, id)| (link.window_index, self.owners[id].as_ref()))
    }

    /// Atomically replaces complete session link tables and optionally introduces
    /// one owner. Only identity tables are staged; existing boxes never move out
    /// of the owner table unless their final occurrence disappears.
    #[allow(clippy::vec_box)] // Transfer the original allocations for runtime teardown.
    pub(crate) fn replace_session_links(
        &mut self,
        tables: &BTreeMap<SessionId, BTreeMap<u32, WindowId>>,
        introduced: Option<Window>,
        plan: LinkOrderPlan,
    ) -> Result<Vec<Box<Window>>, WindowStateError> {
        let new_id = introduced.as_ref().map(Window::id);
        if let Some(id) = new_id {
            if self.owners.contains_key(&id) {
                return Err(WindowStateError::DuplicateWindow(id));
            }
        }
        let mut links = self.links.clone();
        links.retain(|link, _| !tables.contains_key(&link.session_id));
        for (&session_id, table) in tables {
            for (&window_index, &id) in table {
                if Some(id) != new_id && !self.owners.contains_key(&id) {
                    return Err(WindowStateError::MissingWindow(id));
                }
                links.insert(
                    WindowLink {
                        session_id,
                        window_index,
                    },
                    id,
                );
            }
        }
        if let Some(id) = new_id {
            if !links.values().any(|linked| *linked == id) {
                return Err(WindowStateError::MissingWindow(id));
            }
        }
        let retained = links
            .values()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let removed = self
            .owners
            .keys()
            .filter(|id| !retained.contains(id))
            .copied()
            .collect::<Vec<_>>();
        let touched = self
            .links
            .iter()
            .chain(links.iter())
            .filter(|(link, _)| tables.contains_key(&link.session_id))
            .map(|(_, id)| *id)
            .collect::<std::collections::BTreeSet<_>>();
        if let Some(window) = introduced {
            self.owners.insert(window.id(), Box::new(window));
            // A window that did not exist has made no link yet; the plan says
            // which one it is about to have.
            self.order
                .insert(new_id.expect("introduced window"), Vec::new());
        }
        let before = std::mem::replace(&mut self.links, links);
        self.apply_link_order_plan(&before, &touched, tables, plan);
        for id in &removed {
            self.order.remove(id);
        }
        Ok(removed
            .into_iter()
            .map(|id| self.owners.remove(&id).expect("existing owner"))
            .collect())
    }

    fn linked_id(&self, link: WindowLink) -> Result<WindowId, WindowStateError> {
        self.links
            .get(&link)
            .copied()
            .ok_or(WindowStateError::MissingLink(link))
    }
}

#[cfg(test)]
mod tests;
