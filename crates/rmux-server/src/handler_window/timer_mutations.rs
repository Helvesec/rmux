//! Authoritative winlink mappings for silence timers during window moves.

use std::collections::{BTreeMap, HashMap};

use rmux_proto::{
    MoveWindowRequest, MoveWindowTarget, OptionName, SessionName, SwapWindowRequest, WindowTarget,
};

use crate::pane_terminals::HandlerState;

pub(super) type TimerTargetOverrides = Vec<(WindowTarget, Option<WindowTarget>)>;

pub(super) fn move_window_timer_target_overrides(
    state: &HandlerState,
    request: &MoveWindowRequest,
) -> TimerTargetOverrides {
    if request.renumber {
        let session_name = match &request.target {
            MoveWindowTarget::Session(session_name) => session_name,
            MoveWindowTarget::Window(target) => target.session_name(),
        };
        let Some(session) = state.sessions.session(session_name) else {
            return Vec::new();
        };
        let base_index = state
            .options
            .resolve(Some(session_name), OptionName::BaseIndex)
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        let mut index_map = BTreeMap::new();
        for (offset, source_index) in session.windows().keys().copied().enumerate() {
            let Ok(offset) = u32::try_from(offset) else {
                return Vec::new();
            };
            let Some(destination_index) = base_index.checked_add(offset) else {
                return Vec::new();
            };
            index_map.insert(source_index, destination_index);
        }
        let mut mappings = HashMap::new();
        add_group_index_map(state, session_name, &index_map, &mut mappings);
        return mappings.into_iter().collect();
    }

    let Some(source) = request.source.as_ref() else {
        return Vec::new();
    };
    let (target_session_name, target_index) = match &request.target {
        MoveWindowTarget::Window(target) => (target.session_name().clone(), target.window_index()),
        MoveWindowTarget::Session(session_name) => {
            let Some(index) = first_available_window_index(state, session_name) else {
                return Vec::new();
            };
            (session_name.clone(), index)
        }
    };
    let destination_index = if request.before {
        target_index
    } else if request.after {
        let Some(index) = target_index.checked_add(1) else {
            return Vec::new();
        };
        index
    } else {
        target_index
    };

    let mut mappings = HashMap::new();
    add_group_identity_map(state, source.session_name(), &mut mappings);
    if source.session_name() != &target_session_name {
        add_group_identity_map(state, &target_session_name, &mut mappings);
    }

    if request.after || request.before {
        let Some(target_session) = state.sessions.session(&target_session_name) else {
            return Vec::new();
        };
        let Some(mut target_map) = make_room_index_map(target_session, destination_index) else {
            return Vec::new();
        };
        if source.session_name() == &target_session_name {
            target_map.insert(source.window_index(), destination_index);
            add_group_index_map(state, &target_session_name, &target_map, &mut mappings);
        } else {
            add_group_index_map(state, &target_session_name, &target_map, &mut mappings);
            mappings.insert(
                source.clone(),
                Some(WindowTarget::with_window(
                    target_session_name,
                    destination_index,
                )),
            );
        }
        return mappings.into_iter().collect();
    }

    if source.session_name() == &target_session_name {
        let Some(session) = state.sessions.session(source.session_name()) else {
            return Vec::new();
        };
        let destination_exists = session.window_at(destination_index).is_some();
        let mut index_map = identity_index_map(session);
        index_map.insert(source.window_index(), destination_index);
        add_group_index_map(state, source.session_name(), &index_map, &mut mappings);
        if request.kill_destination && destination_exists {
            for member in state.sessions.session_group_members(source.session_name()) {
                mappings.insert(WindowTarget::with_window(member, destination_index), None);
            }
        }
    } else {
        if request.kill_destination
            && state
                .sessions
                .session(&target_session_name)
                .and_then(|session| session.window_at(destination_index))
                .is_some()
        {
            for member in state.sessions.session_group_members(&target_session_name) {
                mappings.insert(WindowTarget::with_window(member, destination_index), None);
            }
        }
        mappings.insert(
            source.clone(),
            Some(WindowTarget::with_window(
                target_session_name,
                destination_index,
            )),
        );
    }

    mappings.into_iter().collect()
}

pub(super) fn swap_window_timer_target_overrides(
    state: &HandlerState,
    request: &SwapWindowRequest,
) -> TimerTargetOverrides {
    let mut mappings = HashMap::new();
    add_group_identity_map(state, request.source.session_name(), &mut mappings);
    if request.source.session_name() != request.target.session_name() {
        add_group_identity_map(state, request.target.session_name(), &mut mappings);
    }

    if request.source.session_name() == request.target.session_name() {
        let Some(session) = state.sessions.session(request.source.session_name()) else {
            return Vec::new();
        };
        let mut index_map = identity_index_map(session);
        index_map.insert(request.source.window_index(), request.target.window_index());
        index_map.insert(request.target.window_index(), request.source.window_index());
        add_group_index_map(
            state,
            request.source.session_name(),
            &index_map,
            &mut mappings,
        );
    } else {
        mappings.insert(request.source.clone(), Some(request.target.clone()));
        mappings.insert(request.target.clone(), Some(request.source.clone()));
    }

    mappings.into_iter().collect()
}

fn add_group_identity_map(
    state: &HandlerState,
    session_name: &SessionName,
    mappings: &mut HashMap<WindowTarget, Option<WindowTarget>>,
) {
    let Some(session) = state.sessions.session(session_name) else {
        return;
    };
    let index_map = identity_index_map(session);
    add_group_index_map(state, session_name, &index_map, mappings);
}

fn add_group_index_map(
    state: &HandlerState,
    session_name: &SessionName,
    index_map: &BTreeMap<u32, u32>,
    mappings: &mut HashMap<WindowTarget, Option<WindowTarget>>,
) {
    for member in state.sessions.session_group_members(session_name) {
        for (source_index, destination_index) in index_map {
            mappings.insert(
                WindowTarget::with_window(member.clone(), *source_index),
                Some(WindowTarget::with_window(
                    member.clone(),
                    *destination_index,
                )),
            );
        }
    }
}

fn identity_index_map(session: &rmux_core::Session) -> BTreeMap<u32, u32> {
    session
        .windows()
        .keys()
        .copied()
        .map(|window_index| (window_index, window_index))
        .collect()
}

fn make_room_index_map(
    session: &rmux_core::Session,
    destination_index: u32,
) -> Option<BTreeMap<u32, u32>> {
    let mut first_gap = destination_index;
    while session.window_at(first_gap).is_some() {
        first_gap = first_gap.checked_add(1)?;
    }
    let mut index_map = identity_index_map(session);
    for source_index in destination_index..first_gap {
        index_map.insert(source_index, source_index.checked_add(1)?);
    }
    Some(index_map)
}

fn first_available_window_index(state: &HandlerState, session_name: &SessionName) -> Option<u32> {
    let session = state.sessions.session(session_name)?;
    let mut index = state
        .options
        .resolve(Some(session_name), OptionName::BaseIndex)
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    loop {
        if session.window_at(index).is_none() {
            return Some(index);
        }
        index = index.checked_add(1)?;
    }
}
