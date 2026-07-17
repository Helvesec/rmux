//! Authoritative winlink mappings for silence timers during pane transfers.

use std::collections::{HashMap, HashSet};

use rmux_core::WindowId;
use rmux_proto::{BreakPaneRequest, SessionName, WindowTarget};

use crate::pane_terminals::HandlerState;

pub(super) type TimerTargetOverrides = Vec<(WindowTarget, Option<WindowTarget>)>;

#[derive(Clone)]
struct WindowSnapshot {
    target: WindowTarget,
    window_id: WindowId,
}

pub(super) struct BreakPaneTimerTargetPlan {
    windows: Vec<WindowSnapshot>,
    source: WindowTarget,
    source_window_id: Option<WindowId>,
    source_was_single_pane: bool,
    source_group_members: Vec<SessionName>,
}

impl BreakPaneTimerTargetPlan {
    pub(super) fn capture(state: &HandlerState, request: &BreakPaneRequest) -> Self {
        let source = WindowTarget::with_window(
            request.source.session_name().clone(),
            request.source.window_index(),
        );
        let source_window = state
            .sessions
            .session(source.session_name())
            .and_then(|session| session.window_at(source.window_index()));
        Self {
            windows: window_snapshots(state),
            source_group_members: state.sessions.session_group_members(source.session_name()),
            source_window_id: source_window.map(rmux_core::Window::id),
            source_was_single_pane: source_window.is_some_and(|window| window.pane_count() == 1),
            source,
        }
    }

    pub(super) fn overrides_after(
        &self,
        state: &HandlerState,
        destination: &WindowTarget,
    ) -> TimerTargetOverrides {
        let post_windows = window_snapshots(state);
        let destination_window_id = window_id_at(state, destination);
        let inserted_destinations = state
            .sessions
            .session_group_members(destination.session_name())
            .into_iter()
            .filter_map(|session_name| {
                let target = WindowTarget::with_window(session_name, destination.window_index());
                (window_id_at(state, &target) == destination_window_id).then_some(target)
            })
            .collect::<HashSet<_>>();

        let mut mappings = HashMap::new();
        let mut consumed_sources = HashSet::new();
        let mut consumed_destinations = inserted_destinations.clone();
        if self.source_was_single_pane
            && self.source_window_id.is_some()
            && self.source_window_id == destination_window_id
        {
            if self
                .source_group_members
                .contains(destination.session_name())
            {
                for session_name in &self.source_group_members {
                    let source =
                        WindowTarget::with_window(session_name.clone(), self.source.window_index());
                    let destination =
                        WindowTarget::with_window(session_name.clone(), destination.window_index());
                    map_if_same_window(
                        &self.windows,
                        &post_windows,
                        source,
                        destination,
                        &mut mappings,
                        &mut consumed_sources,
                        &mut consumed_destinations,
                    );
                }
            } else {
                map_if_same_window(
                    &self.windows,
                    &post_windows,
                    self.source.clone(),
                    destination.clone(),
                    &mut mappings,
                    &mut consumed_sources,
                    &mut consumed_destinations,
                );
            }
        }

        let mut remaining_sources = self
            .windows
            .iter()
            .filter(|window| !consumed_sources.contains(&window.target))
            .collect::<Vec<_>>();
        remaining_sources.sort_by_key(|window| {
            (
                window.target.session_name().to_string(),
                window.window_id.as_u32(),
                window.target.window_index(),
            )
        });

        let mut remaining_destinations = post_windows
            .iter()
            .filter(|window| !consumed_destinations.contains(&window.target))
            .collect::<Vec<_>>();
        remaining_destinations.sort_by_key(|window| {
            (
                window.target.session_name().to_string(),
                window.window_id.as_u32(),
                window.target.window_index(),
            )
        });

        let mut used_destinations = HashSet::new();
        for source in remaining_sources {
            let destination = remaining_destinations.iter().find(|destination| {
                !used_destinations.contains(&destination.target)
                    && destination.target.session_name() == source.target.session_name()
                    && destination.window_id == source.window_id
            });
            if let Some(destination) = destination {
                used_destinations.insert(destination.target.clone());
                mappings.insert(source.target.clone(), Some(destination.target.clone()));
            } else {
                mappings.insert(source.target.clone(), None);
            }
        }

        mappings.into_iter().collect()
    }
}

fn map_if_same_window(
    pre_windows: &[WindowSnapshot],
    post_windows: &[WindowSnapshot],
    source: WindowTarget,
    destination: WindowTarget,
    mappings: &mut HashMap<WindowTarget, Option<WindowTarget>>,
    consumed_sources: &mut HashSet<WindowTarget>,
    consumed_destinations: &mut HashSet<WindowTarget>,
) {
    let source_window_id = pre_windows
        .iter()
        .find(|window| window.target == source)
        .map(|window| window.window_id);
    let destination_window_id = post_windows
        .iter()
        .find(|window| window.target == destination)
        .map(|window| window.window_id);
    if source_window_id.is_none() || source_window_id != destination_window_id {
        return;
    }
    consumed_sources.insert(source.clone());
    consumed_destinations.insert(destination.clone());
    mappings.insert(source, Some(destination));
}

fn window_snapshots(state: &HandlerState) -> Vec<WindowSnapshot> {
    state
        .sessions
        .iter()
        .flat_map(|(session_name, session)| {
            session
                .windows()
                .iter()
                .map(|(window_index, window)| WindowSnapshot {
                    target: WindowTarget::with_window(session_name.clone(), *window_index),
                    window_id: window.id(),
                })
        })
        .collect()
}

fn window_id_at(state: &HandlerState, target: &WindowTarget) -> Option<WindowId> {
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .map(rmux_core::Window::id)
}
