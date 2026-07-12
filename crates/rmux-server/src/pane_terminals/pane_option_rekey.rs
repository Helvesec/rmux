use std::collections::{BTreeMap, HashMap};

use rmux_core::{PaneId, Session};
use rmux_proto::{OptionScopeSelector, PaneTarget, RmuxError, SessionName};

use super::{session_not_found, HandlerState};

pub(crate) type PaneSlotSnapshot = HashMap<PaneId, Vec<PaneTarget>>;

impl HandlerState {
    pub(crate) fn pane_alias_targets(&self, pane_id: PaneId) -> Vec<PaneTarget> {
        let mut aliases = self
            .sessions
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
                                PaneTarget::with_window(
                                    session_name.clone(),
                                    *window_index,
                                    pane.index(),
                                )
                            })
                    })
            })
            .collect::<Vec<_>>();
        aliases.sort_by(|left, right| {
            left.session_name()
                .as_str()
                .cmp(right.session_name().as_str())
                .then_with(|| left.window_index().cmp(&right.window_index()))
                .then_with(|| left.pane_index().cmp(&right.pane_index()))
        });
        aliases
    }

    pub(crate) fn synchronize_pane_alias_options_from_target(
        &mut self,
        source: &PaneTarget,
    ) -> Result<PaneId, RmuxError> {
        let pane_id = pane_id_for_target(&self.sessions, source)?;
        for alias in self.pane_alias_targets(pane_id) {
            if alias != *source {
                self.options.copy_pane_overrides(source, &alias);
            }
        }
        Ok(pane_id)
    }

    pub(crate) fn synchronize_pane_alias_options_from_session(
        &mut self,
        source: &Session,
    ) -> Result<(), RmuxError> {
        let sources = pane_option_slots(source)
            .into_values()
            .filter_map(|targets| targets.into_iter().next())
            .collect::<Vec<_>>();
        for source in sources {
            self.synchronize_pane_alias_options_from_target(&source)?;
        }
        Ok(())
    }

    pub(crate) fn pane_explicit_option_value_by_name(
        &self,
        target: &PaneTarget,
        name: &str,
    ) -> Result<(String, Option<String>), RmuxError> {
        let pane_id = pane_id_for_target(&self.sessions, target)?;
        let scope = OptionScopeSelector::Pane(target.clone());
        let (canonical_name, direct) = self.options.explicit_value_by_name(&scope, name)?;
        if direct.is_some() {
            return Ok((canonical_name, direct));
        }
        for alias in self.pane_alias_targets(pane_id) {
            if alias == *target {
                continue;
            }
            let (_, value) = self
                .options
                .explicit_value_by_name(&OptionScopeSelector::Pane(alias), name)?;
            if value.is_some() {
                return Ok((canonical_name, value));
            }
        }
        Ok((canonical_name, None))
    }

    pub(crate) fn pane_explicit_option_entries(
        &self,
        target: &PaneTarget,
    ) -> Result<Vec<(String, String)>, RmuxError> {
        let pane_id = pane_id_for_target(&self.sessions, target)?;
        let mut entries = BTreeMap::new();
        for alias in self.pane_alias_targets(pane_id) {
            for (name, value) in self
                .options
                .explicit_entries_for_scope(&OptionScopeSelector::Pane(alias))
            {
                entries.entry(name).or_insert(value);
            }
        }
        for (name, value) in self
            .options
            .explicit_entries_for_scope(&OptionScopeSelector::Pane(target.clone()))
        {
            entries.insert(name, value);
        }
        Ok(entries.into_iter().collect())
    }

    pub(crate) fn pane_option_slots_for_session(
        &self,
        session_name: &SessionName,
    ) -> Result<PaneSlotSnapshot, RmuxError> {
        let session = self
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        Ok(pane_option_slots(session))
    }

    pub(crate) fn rekey_pane_options_after_session_change(
        &mut self,
        before: &PaneSlotSnapshot,
        session_name: &SessionName,
    ) -> Result<(), RmuxError> {
        let after = self
            .sessions
            .session(session_name)
            .map(pane_option_slots)
            .unwrap_or_default();
        self.rekey_pane_options_between_snapshots(before, &after)
    }

    pub(crate) fn rekey_and_synchronize_pane_options_after_session_change(
        &mut self,
        before: &PaneSlotSnapshot,
        session_name: &SessionName,
    ) -> Result<(), RmuxError> {
        self.rekey_pane_options_after_session_change(before, session_name)?;
        let session = self
            .sessions
            .session(session_name)
            .cloned()
            .ok_or_else(|| session_not_found(session_name))?;
        self.synchronize_pane_alias_options_from_session(&session)
    }

    pub(crate) fn rekey_pane_options_between_snapshots(
        &mut self,
        before: &PaneSlotSnapshot,
        after: &PaneSlotSnapshot,
    ) -> Result<(), RmuxError> {
        let mut mappings = Vec::new();
        for (pane_id, sources) in before {
            let targets = after.get(pane_id).map(Vec::as_slice).unwrap_or_default();
            for (source, target) in sources.iter().zip(targets) {
                if source != target {
                    mappings.push((source.clone(), Some(target.clone())));
                }
            }
            mappings.extend(
                sources
                    .iter()
                    .skip(targets.len())
                    .cloned()
                    .map(|source| (source, None)),
            );
        }
        self.options.rekey_pane_overrides(&mappings)
    }
}

fn pane_id_for_target(
    sessions: &rmux_core::SessionStore,
    target: &PaneTarget,
) -> Result<PaneId, RmuxError> {
    sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(|pane| pane.id())
        .ok_or_else(|| {
            RmuxError::invalid_target(target.to_string(), "pane index does not exist in session")
        })
}

fn pane_option_slots(session: &Session) -> PaneSlotSnapshot {
    let mut slots = PaneSlotSnapshot::new();
    for (window_index, window) in session.windows() {
        for pane in window.panes() {
            slots
                .entry(pane.id())
                .or_default()
                .push(PaneTarget::with_window(
                    session.name().clone(),
                    *window_index,
                    pane.index(),
                ));
        }
    }
    for targets in slots.values_mut() {
        targets.sort_by(|left, right| {
            left.window_index()
                .cmp(&right.window_index())
                .then_with(|| left.pane_index().cmp(&right.pane_index()))
        });
    }
    slots
}
