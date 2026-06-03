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

use crate::SessionStore;

#[path = "target_find/current.rs"]
mod current;
#[path = "target_find/matching.rs"]
mod matching;
#[path = "target_find/metadata.rs"]
mod metadata;
#[path = "target_find/pane_navigation.rs"]
mod pane_navigation;
#[path = "target_find/session.rs"]
mod session;
#[path = "target_find/syntax.rs"]
mod syntax;
#[path = "target_find/types.rs"]
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
    CommandTargetMetadata, CommandTargetSpec, TargetFindContext, TargetFindFlags, TargetFindType,
    UnresolvedTarget,
};

impl SessionStore {
    /// Resolves an unresolved command target to the existing typed target representation.
    pub fn resolve_unresolved_target(
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
        if matches!(raw, "@" | "{active}" | "{current}") {
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
            let window_id = crate::WindowId::new(parse_required_prefixed_id(value, '@')?);
            let Some(window_index) = session
                .windows()
                .iter()
                .find_map(|(index, window)| (window.id() == window_id).then_some(*index))
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
        let window_id = crate::WindowId::new(parse_required_prefixed_id(value, '@')?);
        if let Ok(current) = self.current_parts(context) {
            if let Some(window) = self
                .session(&current.session_name)
                .and_then(|session| session.window_at(current.window_index))
            {
                if window.id() == window_id {
                    return Ok(ResolvedParts::window(
                        current.session_name,
                        current.window_index,
                    ));
                }
            }
        }

        let mut matches = self
            .iter()
            .flat_map(|(session_name, session)| {
                session
                    .windows()
                    .iter()
                    .filter(move |(_, window)| window.id() == window_id)
                    .map(move |(window_index, _)| {
                        ResolvedParts::window(session_name.clone(), *window_index)
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

    fn resolve_pane_from_context(
        &self,
        raw: &str,
        value: &str,
        only_pane: bool,
        flags: TargetFindFlags,
        context: &TargetFindContext,
    ) -> Result<ResolvedParts, RmuxError> {
        if value.starts_with('%') {
            return self.resolve_pane_id(raw, value, context);
        }

        let current = match self.current_parts(context) {
            Ok(current) => current,
            Err(error) if !only_pane => {
                return self
                    .resolve_window_from_context(raw, value, false, flags, context)
                    .and_then(|window| {
                        self.target_for_window_current(TargetFindType::Pane, window)
                            .and_then(|target| {
                                if let Target::Pane(target) = target {
                                    Ok(ResolvedParts::pane(
                                        target.session_name().clone(),
                                        target.window_index(),
                                        target.pane_index(),
                                    ))
                                } else {
                                    Err(RmuxError::Server(
                                        "pane fallback produced non-pane target".to_owned(),
                                    ))
                                }
                            })
                    })
                    .map_err(|_| error);
            }
            Err(error) => return Err(error),
        };
        match self.resolve_pane_in_window(
            raw,
            &current.session_name,
            current.window_index,
            value,
            context,
        ) {
            Ok(pane) => Ok(pane),
            Err(error) if !only_pane => {
                match self.resolve_window_from_context(raw, value, false, flags, context) {
                    Ok(window) => self
                        .target_for_window_current(TargetFindType::Pane, window)
                        .and_then(|target| {
                            if let Target::Pane(target) = target {
                                Ok(ResolvedParts::pane(
                                    target.session_name().clone(),
                                    target.window_index(),
                                    target.pane_index(),
                                ))
                            } else {
                                Err(RmuxError::Server(
                                    "pane fallback produced non-pane target".to_owned(),
                                ))
                            }
                        }),
                    Err(_) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    fn resolve_pane_in_session(
        &self,
        raw: &str,
        session_name: &SessionName,
        value: &str,
        context: &TargetFindContext,
    ) -> Result<ResolvedParts, RmuxError> {
        if value.starts_with('%') {
            let pane_id = crate::PaneId::new(parse_required_prefixed_id(value, '%')?);
            let session = self
                .session(session_name)
                .ok_or_else(|| target_not_found(session_name.as_str(), "session"))?;
            let Some((window_index, pane_index)) = find_pane_id_in_session(session, pane_id) else {
                return Err(target_not_found(value, "pane"));
            };
            return Ok(ResolvedParts::pane(
                session_name.clone(),
                window_index,
                pane_index,
            ));
        }

        let window_index = self
            .session(session_name)
            .ok_or_else(|| target_not_found(session_name.as_str(), "session"))?
            .active_window_index();
        self.resolve_pane_in_window(raw, session_name, window_index, value, context)
    }

    fn resolve_pane_in_window(
        &self,
        raw: &str,
        session_name: &SessionName,
        window_index: u32,
        value: &str,
        context: &TargetFindContext,
    ) -> Result<ResolvedParts, RmuxError> {
        let session = self
            .session(session_name)
            .ok_or_else(|| target_not_found(session_name.as_str(), "session"))?;
        let window = session
            .window_at(window_index)
            .ok_or_else(|| target_not_found(&format!("{session_name}:{window_index}"), "window"))?;

        if value.starts_with('%') {
            let pane_id = crate::PaneId::new(parse_required_prefixed_id(value, '%')?);
            let Some(pane) = window.panes().iter().find(|pane| pane.id() == pane_id) else {
                return Err(target_not_found(value, "pane"));
            };
            return Ok(ResolvedParts::pane(
                session_name.clone(),
                window_index,
                pane.index(),
            ));
        }

        if value == "!" {
            let pane_index = window
                .last_pane_index()
                .ok_or_else(|| target_not_found(value, "pane"))?;
            return Ok(ResolvedParts::pane(
                session_name.clone(),
                window_index,
                pane_index,
            ));
        }

        if let Some(pane_index) = directional_pane(window, value) {
            return Ok(ResolvedParts::pane(
                session_name.clone(),
                window_index,
                pane_index,
            ));
        }

        if let Some(offset) = parse_offset(value)? {
            let pane_index = cyclic_pane_offset(window, offset)
                .ok_or_else(|| target_not_found(value, "pane"))?;
            return Ok(ResolvedParts::pane(
                session_name.clone(),
                window_index,
                pane_index,
            ));
        }

        if let Ok(visible_pane_index) = value.parse::<u32>() {
            let pane_base_index = context.pane_base_index(session_name, window_index);
            if let Some(pane_index) = visible_pane_index.checked_sub(pane_base_index) {
                if window.pane(pane_index).is_some() {
                    return Ok(ResolvedParts::pane(
                        session_name.clone(),
                        window_index,
                        pane_index,
                    ));
                }
            }
            if pane_base_index == 0 && window.pane(visible_pane_index).is_some() {
                return Ok(ResolvedParts::pane(
                    session_name.clone(),
                    window_index,
                    visible_pane_index,
                ));
            }
        }

        if let Some(pane_index) = pane_description(window, value) {
            return Ok(ResolvedParts::pane(
                session_name.clone(),
                window_index,
                pane_index,
            ));
        }

        Err(RmuxError::invalid_target(
            raw,
            format!("can't find pane: {value}"),
        ))
    }

    fn resolve_pane_id(
        &self,
        raw: &str,
        value: &str,
        context: &TargetFindContext,
    ) -> Result<ResolvedParts, RmuxError> {
        let pane_id = crate::PaneId::new(parse_required_prefixed_id(value, '%')?);
        if let Ok(current) = self.current_parts(context) {
            if let Some(pane) = self
                .session(&current.session_name)
                .and_then(|session| session.window_at(current.window_index))
                .and_then(|window| window.pane(current.pane_index))
            {
                if pane.id() == pane_id {
                    return Ok(current);
                }
            }
        }

        let mut matches = self
            .iter()
            .flat_map(|(session_name, session)| {
                session
                    .windows()
                    .iter()
                    .flat_map(move |(window_index, window)| {
                        window
                            .panes()
                            .iter()
                            .filter(move |pane| pane.id() == pane_id)
                            .map(move |pane| {
                                ResolvedParts::pane(
                                    session_name.clone(),
                                    *window_index,
                                    pane.index(),
                                )
                            })
                    })
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            left.session_name
                .as_str()
                .cmp(right.session_name.as_str())
                .then(left.window_index.cmp(&right.window_index))
                .then(left.pane_index.cmp(&right.pane_index))
        });
        unique_match(matches, raw, "pane")?.ok_or_else(|| target_not_found(value, "pane"))
    }
}

#[cfg(test)]
#[path = "target_find/tests.rs"]
mod tests;
