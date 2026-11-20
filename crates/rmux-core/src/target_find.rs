//! tmux-style unresolved target lookup over live in-memory session state.
//!
//! This module intentionally sits above the exact protocol target structs in
//! `rmux-proto`: it accepts raw command target text and resolves it to the
//! existing `Target`, `WindowTarget`, and `PaneTarget` output representation.
//! Frozen source anchors: `/opt/rmux/reference/tmux` at commit
//! `31d77e29b6c9fbb07d032018da78db3a8a38d979`, especially
//! `cmd-find.c:247`, `cmd-find.c:347`, `cmd-find.c:563`, and
//! `cmd-find.c:922`.

use rmux_proto::{RmuxError, SessionName, Target};

use crate::{ManagedSessionStore, SessionStore};

mod pane;
mod storage;
use storage::TargetStore;

mod current;
mod matching;
mod metadata;
mod pane_navigation;
mod session;
mod syntax;
mod types;

use matching::{
    cyclic_pane_offset, cyclic_window_offset, find_pane_id_in_session, offset_index, parse_offset,
    parse_required_prefixed_id, special_window_index, target_for_type, target_not_found,
    unique_match, unique_window_name_match, MatchMode, ResolvedParts,
};
pub use metadata::command_target_metadata;
use pane_navigation::{directional_pane, pane_description};
use syntax::TargetParts;
pub use types::{
    CommandTargetMetadata, CommandTargetSpec, MissingCurrentTargetFallback, TargetFindContext,
    TargetFindFlags, TargetFindType, UnresolvedTarget,
};

impl SessionStore {
    /// Resolves command target text using borrowed session metadata and windows.
    pub fn resolve_unresolved_target(
        &self,
        target: &UnresolvedTarget,
        find_type: TargetFindType,
        flags: TargetFindFlags,
        context: &TargetFindContext,
    ) -> Result<Target, RmuxError> {
        TargetStore::Standalone(self).resolve_unresolved_target(target, find_type, flags, context)
    }
}

impl ManagedSessionStore {
    /// Resolves command target text using borrowed session metadata and windows.
    pub fn resolve_unresolved_target(
        &self,
        target: &UnresolvedTarget,
        find_type: TargetFindType,
        flags: TargetFindFlags,
        context: &TargetFindContext,
    ) -> Result<Target, RmuxError> {
        TargetStore::Managed(self).resolve_unresolved_target(target, find_type, flags, context)
    }
}

impl TargetStore<'_> {
    /// Resolves an unresolved command target to the existing typed target representation.
    fn resolve_unresolved_target(
        &self,
        target: &UnresolvedTarget,
        find_type: TargetFindType,
        flags: TargetFindFlags,
        context: &TargetFindContext,
    ) -> Result<Target, RmuxError> {
        let Some(raw) = target.as_deref() else {
            return self.current_target_for_type(find_type, flags, context);
        };
        if raw.is_empty() {
            return self.current_target_for_type(find_type, flags, context);
        }

        if raw == "." {
            return self.current_target_for_type(find_type, flags, context);
        }
        if matches!(raw, "{active}" | "{current}") {
            return self.current_target_for_type(find_type, flags, context);
        }
        if matches!(raw, "=" | "{mouse}") {
            return self.mouse_target_for_type(find_type, context);
        }
        if matches!(raw, "~" | "{marked}") {
            return self.marked_target_for_type(find_type, context);
        }

        let mut parts = TargetParts::parse(raw, find_type);
        let mut flags = flags;
        parts.apply_exact_prefixes(&mut flags);
        if flags.contains(TargetFindFlags::WINDOW_INDEX)
            && parts.window_only
            && parts.window == Some("")
            && parts.pane.is_none()
        {
            return self.resolve_empty_window_index_target(
                raw,
                parts.session,
                find_type,
                flags,
                context,
            );
        }
        parts.drop_empty();
        parts.map_tokens();
        if flags.contains(TargetFindFlags::WINDOW_INDEX) && parts.pane.is_some() {
            return Err(RmuxError::invalid_target(raw, "can't specify pane here"));
        }

        self.resolve_parts(raw, parts, find_type, flags, context)
    }

    fn resolve_parts(
        &self,
        raw: &str,
        parts: TargetParts<'_>,
        find_type: TargetFindType,
        flags: TargetFindFlags,
        context: &TargetFindContext,
    ) -> Result<Target, RmuxError> {
        if let Some(session) = parts.session {
            let session_name = self.resolve_session_part(session, flags)?;
            if parts.window.is_none() && parts.pane.is_none() {
                return self.target_for_session_current(find_type, session_name);
            }
            if let Some(window) = parts.window {
                let window = self.resolve_window_in_session(raw, &session_name, window, flags)?;
                if let Some(pane) = parts.pane {
                    let pane = self.resolve_pane_in_window(
                        raw,
                        &window.session_name,
                        window.window_index,
                        pane,
                        context,
                    )?;
                    return Ok(target_for_type(find_type, pane));
                }
                return self.target_for_window_current(find_type, window);
            }

            let pane = self.resolve_pane_in_session(
                raw,
                &session_name,
                parts.pane.expect("pane branch requires pane"),
                context,
            )?;
            return Ok(target_for_type(find_type, pane));
        }

        if let Some(window) = parts.window {
            let window =
                self.resolve_window_from_context(raw, window, parts.window_only, flags, context)?;
            if let Some(pane) = parts.pane {
                let pane = self.resolve_pane_in_window(
                    raw,
                    &window.session_name,
                    window.window_index,
                    pane,
                    context,
                )?;
                return Ok(target_for_type(find_type, pane));
            }
            return self.target_for_window_current(find_type, window);
        }

        if let Some(pane) = parts.pane {
            let pane =
                self.resolve_pane_from_context(raw, pane, parts.pane_only, flags, context)?;
            return Ok(target_for_type(find_type, pane));
        }

        self.current_target_for_type(find_type, flags, context)
    }

    fn resolve_empty_window_index_target(
        &self,
        raw: &str,
        session_part: Option<&str>,
        find_type: TargetFindType,
        flags: TargetFindFlags,
        context: &TargetFindContext,
    ) -> Result<Target, RmuxError> {
        let session_name = match session_part.filter(|value| !value.is_empty()) {
            Some(session_part) => self.resolve_session_part(session_part, flags)?,
            None => self
                .current_parts(context)
                .map(|parts| parts.session_name)
                .or_else(|_| self.default_session_name(flags))?,
        };
        let session = self
            .session(&session_name)
            .ok_or_else(|| target_not_found(raw, "session"))?;
        let window_index = session.lowest_available_window_index_at_or_above(0)?;
        Ok(target_for_type(
            find_type,
            ResolvedParts::window(session_name, window_index),
        ))
    }

    fn resolve_window_from_context(
        &self,
        raw: &str,
        value: &str,
        only_window: bool,
        flags: TargetFindFlags,
        context: &TargetFindContext,
    ) -> Result<ResolvedParts, RmuxError> {
        if value.starts_with('@') {
            return self.resolve_window_id(raw, value, context);
        }

        let current = match self.current_parts(context) {
            Ok(current) => current,
            Err(error) if !only_window => {
                if let Ok(window) = self.resolve_window_globally(raw, value, flags) {
                    return Ok(window);
                }
                return self
                    .resolve_session_part(value, flags)
                    .and_then(|session_name| self.parts_for_session_current(&session_name))
                    .map_err(|_| error);
            }
            Err(_) if only_window => {
                return self.resolve_window_globally(raw, value, flags);
            }
            Err(error) => return Err(error),
        };
        match self.resolve_window_in_session(raw, &current.session_name, value, flags) {
            Ok(window) => Ok(window),
            Err(error) if !only_window => match self.resolve_session_part(value, flags) {
                Ok(session_name) => self.parts_for_session_current(&session_name),
                Err(_) => Err(error),
            },
            Err(error) => Err(error),
        }
    }

    fn resolve_window_globally(
        &self,
        raw: &str,
        value: &str,
        flags: TargetFindFlags,
    ) -> Result<ResolvedParts, RmuxError> {
        let mut exact = Vec::new();
        for (session_name, _) in self.iter() {
            if let Ok(window) = self.resolve_window_in_session(raw, session_name, value, flags) {
                exact.push(window);
            }
        }
        exact.sort_by(|left, right| {
            left.session_name
                .as_str()
                .cmp(right.session_name.as_str())
                .then(left.window_index.cmp(&right.window_index))
        });
        if let Some(window) = unique_match(exact, raw, "window")? {
            return Ok(window);
        }

        Err(target_not_found(value, "window"))
    }

    fn resolve_window_in_session(
        &self,
        raw: &str,
        session_name: &SessionName,
        value: &str,
        flags: TargetFindFlags,
    ) -> Result<ResolvedParts, RmuxError> {
        let session = self
            .session(session_name)
            .ok_or_else(|| target_not_found(session_name.as_str(), "session"))?;

        if value.starts_with('@') {
            if value == "@" {
                return Err(target_not_found(value, "window"));
            }
            let window_id = crate::WindowId::new(parse_required_prefixed_id(value, '@')?);
            let Some(window_index) = session
                .windows()
                .find_map(|(index, window)| (window.id() == window_id).then_some(index))
            else {
                return Err(target_not_found(value, "window"));
            };
            return self.parts_for_window(session_name, window_index);
        }

        if !flags.contains(TargetFindFlags::EXACT_WINDOW) {
            if let Some(offset) = parse_offset(value)? {
                let window_index = if flags.contains(TargetFindFlags::WINDOW_INDEX) {
                    offset_index(session.active_window_index(), offset)
                        .ok_or_else(|| target_not_found(value, "window"))?
                } else {
                    cyclic_window_offset(session, offset)
                        .ok_or_else(|| target_not_found(value, "window"))?
                };
                return Ok(ResolvedParts::window(session_name.clone(), window_index));
            }

            if let Some(window_index) = special_window_index(session, value) {
                return self.parts_for_window(session_name, window_index);
            }
        }

        if !value.starts_with(['+', '-']) {
            if let Ok(window_index) = value.parse::<u32>() {
                if session.window_at(window_index).is_some() {
                    return self.parts_for_window(session_name, window_index);
                }
                if flags.contains(TargetFindFlags::WINDOW_INDEX) {
                    return Ok(ResolvedParts::window(session_name.clone(), window_index));
                }
            }
        }

        if let Some(window_index) = unique_window_name_match(session, value, MatchMode::Exact)? {
            return self.parts_for_window(session_name, window_index);
        }

        if flags.contains(TargetFindFlags::EXACT_WINDOW) {
            return Err(target_not_found(value, "window"));
        }

        if let Some(window_index) = unique_window_name_match(session, value, MatchMode::Prefix)? {
            return self.parts_for_window(session_name, window_index);
        }
        if let Some(window_index) = unique_window_name_match(session, value, MatchMode::Pattern)? {
            return self.parts_for_window(session_name, window_index);
        }

        Err(RmuxError::invalid_target(
            raw,
            format!("can't find window: {value}"),
        ))
    }

    fn resolve_window_id(
        &self,
        raw: &str,
        value: &str,
        context: &TargetFindContext,
    ) -> Result<ResolvedParts, RmuxError> {
        if value == "@" {
            return Err(target_not_found(value, "window"));
        }
        let window_id = crate::WindowId::new(parse_required_prefixed_id(value, '@')?);
        if let Ok(current) = self.current_parts(context) {
            if let Some(window_index) = self.session(&current.session_name).and_then(|session| {
                session
                    .windows()
                    .find_map(|(index, window)| (window.id() == window_id).then_some(index))
            }) {
                return Ok(ResolvedParts::window(current.session_name, window_index));
            }
        }

        let mut matches = self
            .iter()
            .flat_map(|(session_name, session)| {
                session
                    .windows()
                    .filter(move |(_, window)| window.id() == window_id)
                    .map(move |(window_index, _)| {
                        ResolvedParts::window(session_name.clone(), window_index)
                    })
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            left.session_name
                .as_str()
                .cmp(right.session_name.as_str())
                .then(left.window_index.cmp(&right.window_index))
        });
        unique_match(matches, raw, "window")?.ok_or_else(|| target_not_found(value, "window"))
    }
}

#[cfg(test)]
#[path = "target_find/gatt1_r2_tests.rs"]
mod gatt1_r2_tests;
#[cfg(test)]
#[path = "target_find/offset_tests.rs"]
mod offset_tests;
#[cfg(test)]
#[path = "target_find/session_recency_tests.rs"]
mod session_recency_tests;
#[cfg(test)]
#[path = "target_find/tests.rs"]
mod tests;
