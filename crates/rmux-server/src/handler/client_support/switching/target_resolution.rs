use super::*;

impl RequestHandler {
    pub(in crate::handler::client_support) async fn apply_switch_target(
        &self,
        target: &str,
        current_session: Option<&SwitchSessionIdentity>,
        flags: TargetFindFlags,
        zoom: bool,
    ) -> Result<SwitchSessionIdentity, RmuxError> {
        self.apply_switch_target_inner(target, current_session, flags, zoom, None, true)
            .await
            .map(|resolved| resolved.session)
    }

    pub(in crate::handler::client_support) async fn apply_switch_target_for_client_identity(
        &self,
        target: &str,
        current_session: Option<&SwitchSessionIdentity>,
        flags: TargetFindFlags,
        zoom: bool,
        client: SwitchManagedClientIdentity,
    ) -> Result<ResolvedSwitchTarget, RmuxError> {
        self.apply_switch_target_inner(target, current_session, flags, zoom, Some(client), false)
            .await
    }

    pub(super) async fn apply_switch_target_inner(
        &self,
        target: &str,
        current_session: Option<&SwitchSessionIdentity>,
        flags: TargetFindFlags,
        zoom: bool,
        client: Option<SwitchManagedClientIdentity>,
        apply_selection: bool,
    ) -> Result<ResolvedSwitchTarget, RmuxError> {
        let find_type = switch_client_target_find_type(target);
        let outcome = self.with_switch_client_state(client, |state| {
            let current_target = match current_session {
                Some(identity) => {
                    if state
                        .sessions
                        .session(&identity.session_name)
                        .is_none_or(|session| session.id() != identity.session_id)
                    {
                        return Err(session_not_found(&identity.session_name));
                    }
                    active_session_target(&state.sessions, &identity.session_name)
                }
                None => None,
            };
            let context = with_visible_pane_bases(
                TargetFindContext::new(current_target),
                &state.sessions,
                &state.options,
            );
            let resolved = state
                .sessions
                .resolve_unresolved_target(
                    &UnresolvedTarget::new(target.to_owned()),
                    find_type,
                    flags,
                    &context,
                )
                .map_err(|error| {
                    if find_type == TargetFindType::Session {
                        normalize_switch_session_lookup_error(target, error)
                    } else {
                        error
                    }
                })?;

            if find_type == TargetFindType::Session && !matches!(resolved, Target::Session(_)) {
                return Err(RmuxError::Server(format!(
                    "resolve-target produced {} where a session target was required",
                    switch_target_response_name(&resolved)
                )));
            }

            let (session_name, selection) = match resolved {
                Target::Session(session_name) => (session_name, None),
                Target::Window(target) => {
                    let window_id = state
                        .sessions
                        .session(target.session_name())
                        .and_then(|session| session.window_at(target.window_index()))
                        .ok_or_else(|| {
                            RmuxError::invalid_target(
                                target.to_string(),
                                "window disappeared while resolving switch target",
                            )
                        })?
                        .id();
                    (
                        target.session_name().clone(),
                        Some(SwitchTargetSelection::Window { target, window_id }),
                    )
                }
                Target::Pane(target) => {
                    let (window_id, pane_id) = state
                        .sessions
                        .session(target.session_name())
                        .and_then(|session| session.window_at(target.window_index()))
                        .and_then(|window| {
                            window
                                .pane(target.pane_index())
                                .map(|pane| (window.id(), pane.id()))
                        })
                        .ok_or_else(|| {
                            RmuxError::invalid_target(
                                target.to_string(),
                                "pane disappeared while resolving switch target",
                            )
                        })?;
                    (
                        target.session_name().clone(),
                        Some(SwitchTargetSelection::Pane {
                            target,
                            window_id,
                            pane_id,
                            pane_output_generation: None,
                            zoom,
                        }),
                    )
                }
            };
            let refresh_sessions = if apply_selection {
                if let Some(selection) = selection.as_ref() {
                    selection.apply_to_state(state)?
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            let session_id = state
                .sessions
                .session(&session_name)
                .ok_or_else(|| session_not_found(&session_name))?
                .id();
            Ok((
                ResolvedSwitchTarget {
                    session: SwitchSessionIdentity {
                        session_name,
                        session_id,
                    },
                    selection,
                },
                refresh_sessions,
            ))
        });
        let (resolved, refresh_sessions) = outcome.await?;
        self.refresh_linked_window_sessions(refresh_sessions).await;
        Ok(resolved)
    }

    pub(super) async fn adjacent_session_name(
        &self,
        current_session: Option<&SwitchSessionIdentity>,
        forward: bool,
        sort_order: Option<&str>,
    ) -> Result<Option<SwitchSessionIdentity>, RmuxError> {
        let session_names = {
            let state = self.state.lock().await;
            if let Some(identity) = current_session {
                if state
                    .sessions
                    .session(&identity.session_name)
                    .is_none_or(|session| session.id() != identity.session_id)
                {
                    return Err(session_not_found(&identity.session_name));
                }
            }
            let mut sessions = state
                .sessions
                .iter()
                .map(|session| (session.name(), session))
                .map(|(session_name, session)| {
                    (
                        session_name.clone(),
                        session.created_at(),
                        session.activity_at(),
                        session.window().size().cols,
                        session.id(),
                    )
                })
                .collect::<Vec<_>>();
            sessions.sort_by(|left, right| {
                let ordering = match parse_session_sort_order(sort_order) {
                    Some(SessionSortOrder::Activity) => left.2.cmp(&right.2),
                    Some(SessionSortOrder::Creation) => left.1.cmp(&right.1),
                    Some(SessionSortOrder::Index) => left.4.cmp(&right.4),
                    Some(SessionSortOrder::Size) => left.3.cmp(&right.3),
                    Some(
                        SessionSortOrder::Name
                        | SessionSortOrder::Modifier
                        | SessionSortOrder::Order,
                    )
                    | None => left.0.as_str().cmp(right.0.as_str()),
                };
                if ordering.is_eq() {
                    left.4.cmp(&right.4)
                } else {
                    ordering
                }
            });
            sessions
                .into_iter()
                .map(
                    |(session_name, _, _, _, session_id)| SwitchSessionIdentity {
                        session_name,
                        session_id,
                    },
                )
                .collect::<Vec<_>>()
        };
        if session_names.is_empty() {
            return Err(RmuxError::Server("no sessions".to_owned()));
        }

        let index = current_session
            .and_then(|current| {
                session_names
                    .iter()
                    .position(|candidate| candidate.session_name == current.session_name)
            })
            .unwrap_or(0);
        let next_index = if forward {
            (index + 1) % session_names.len()
        } else if index == 0 {
            session_names.len().saturating_sub(1)
        } else {
            index - 1
        };
        Ok(session_names.get(next_index).cloned())
    }

    pub(in crate::handler::client_support) async fn switch_session_identity(
        &self,
        session_name: SessionName,
    ) -> Result<SwitchSessionIdentity, RmuxError> {
        let state = self.state.lock().await;
        let session_id = state
            .sessions
            .session(&session_name)
            .ok_or_else(|| session_not_found(&session_name))?
            .id();
        Ok(SwitchSessionIdentity {
            session_name,
            session_id,
        })
    }
}

fn normalize_switch_session_lookup_error(target: &str, error: RmuxError) -> RmuxError {
    if matches!(
        &error,
        RmuxError::InvalidTarget { reason, .. } if reason.starts_with("can't find session: ")
    ) {
        let lookup = target.strip_prefix('=').unwrap_or(target);
        if let Ok(session_name) = SessionName::from_canonical(lookup) {
            return session_not_found(&session_name);
        }
    }
    error
}

fn switch_target_response_name(target: &Target) -> &'static str {
    match target {
        Target::Session(_) => "session",
        Target::Window(_) => "window",
        Target::Pane(_) => "pane",
    }
}
