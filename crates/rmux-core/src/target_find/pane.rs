use super::*;

impl TargetStore<'_> {
    pub(super) fn resolve_pane_from_context(
        &self,
        raw: &str,
        value: &str,
        only_pane: bool,
        flags: TargetFindFlags,
        context: &TargetFindContext,
    ) -> Result<ResolvedParts, RmuxError> {
        if value.starts_with('%') {
            return self.resolve_pane_id(value);
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

    pub(super) fn resolve_pane_in_session(
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

    pub(super) fn resolve_pane_in_window(
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

    /// A bare `%id` names ONE pane, whatever number of sessions link its window.
    ///
    /// tmux finds the pane in its single global pane tree and then chooses among
    /// the sessions that hold that pane's window: the most recently active of
    /// them, the first by name on a tie (`cmd-find.c:498-512` through
    /// `cmd_find_best_session_with_window` at 176-197, whose comparator
    /// `cmd_find_session_better` ends on the activity times). The context that
    /// asked plays no part on this path - there is no context branch in
    /// `cmd_find_get_pane` - so a pane reachable through several sessions is
    /// resolved, never refused as ambiguous. The link inside the chosen session
    /// is the active one when it holds the window, otherwise the first index,
    /// which is what `find_pane_id_in_session` answers.
    ///
    /// The comparator is the one `current_session_name` already ranks with, so
    /// the two readings of "which session" cannot drift apart. Only sessions
    /// that really hold the pane are compared; an id held by nobody is refused.
    pub(super) fn resolve_pane_id(&self, value: &str) -> Result<ResolvedParts, RmuxError> {
        let pane_id = crate::PaneId::new(parse_required_prefixed_id(value, '%')?);
        let chosen = self
            .iter()
            .filter(|(_, session)| find_pane_id_in_session(*session, pane_id).is_some())
            .max_by(|(left_name, left), (right_name, right)| {
                left.recency()
                    .cmp(&right.recency())
                    .then(right_name.as_str().cmp(left_name.as_str()))
            });
        let Some((session_name, session)) = chosen else {
            return Err(target_not_found(value, "pane"));
        };
        let (window_index, pane_index) = find_pane_id_in_session(session, pane_id)
            .expect("the session was kept because it holds this pane");
        Ok(ResolvedParts::pane(
            session_name.clone(),
            window_index,
            pane_index,
        ))
    }
}
