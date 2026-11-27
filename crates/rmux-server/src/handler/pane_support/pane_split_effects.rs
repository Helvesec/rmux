use rmux_proto::{
    PaneTarget, ResizePaneAdjustment, RmuxError, SplitDirection, SplitWindowTarget, Target,
    WindowTarget,
};

use crate::pane_terminals::HandlerState;

#[derive(Debug, Clone)]
pub(super) struct SplitWindowEffects {
    pub(super) detached_anchor: Option<PaneTarget>,
    pub(super) size: Option<u32>,
}

pub(super) fn split_window_effects(
    state: &HandlerState,
    target: &SplitWindowTarget,
    direction: SplitDirection,
    detached: bool,
    size: Option<&str>,
    expand_size: impl Fn(&str) -> Result<String, RmuxError>,
) -> Result<SplitWindowEffects, RmuxError> {
    let (session_name, window_index, pane_index) = split_target_anchor(state, target)?;
    let session = state
        .sessions
        .session(&session_name)
        .ok_or_else(|| RmuxError::SessionNotFound(session_name.to_string()))?;
    let window = session.window_at(window_index).ok_or_else(|| {
        RmuxError::invalid_target(
            format!("{session_name}:{window_index}"),
            "window index does not exist in session",
        )
    })?;
    let pane = window.pane(pane_index).ok_or_else(|| {
        RmuxError::invalid_target(
            format!("{session_name}:{window_index}.{pane_index}"),
            "pane index does not exist in session",
        )
    })?;
    let split_size = size
        .map(|size| split_size_cells(size, direction, pane.geometry(), &expand_size))
        .transpose()?;

    Ok(SplitWindowEffects {
        detached_anchor: detached
            .then(|| PaneTarget::with_window(session_name.clone(), window_index, pane_index)),
        size: split_size,
    })
}

/// A detached `-Z` split zooms its window's active pane once the split is
/// made, which resizes that pane under the strict policy: a poisoned active
/// pane fails fast before anything changes.
pub(super) fn ensure_detached_zoom_pane_available(
    state: &HandlerState,
    effects: &SplitWindowEffects,
    preserve_zoom: bool,
) -> Result<(), RmuxError> {
    let Some(anchor) = effects.detached_anchor.as_ref().filter(|_| preserve_zoom) else {
        return Ok(());
    };
    let Some(active) = state
        .sessions
        .session(anchor.session_name())
        .and_then(|session| session.window_at(anchor.window_index()))
        .and_then(|window| window.active_pane())
    else {
        return Ok(());
    };
    state.ensure_pane_id_available(anchor.session_name(), anchor.window_index(), active.id())
}

pub(super) fn apply_split_window_effects(
    state: &mut HandlerState,
    pane: &PaneTarget,
    effects: SplitWindowEffects,
    preserve_zoom: bool,
) -> Result<(), RmuxError> {
    if preserve_zoom {
        let zoom_target = effects.detached_anchor.as_ref().unwrap_or(pane);
        let session_name = zoom_target.session_name().clone();
        let detached = effects.detached_anchor.is_some();
        let window_index = zoom_target.window_index();
        let window_target = WindowTarget::with_window(session_name, window_index);
        match state
            .sessions
            .resolve_window(&Target::Window(window_target.clone()))
        {
            Ok(_) => {}
            Err(RmuxError::InvalidTarget { .. }) => return Ok(()),
            Err(error) => return Err(error),
        }
        state
            .mutate_window_layout_and_resize_terminal_with_family(
                &window_target,
                |mut window_layout| {
                    if window_layout.is_zoomed() {
                        return Ok(());
                    }
                    // tmux `split-window -d -Z` zooms the window around the pane
                    // that is already active; only a non-detached -Z zooms (and
                    // activates) the new pane. Zooming the anchor target here used
                    // to re-select it, re-introducing the active/last/active_point
                    // side effects a detached split must not have.
                    let zoom_pane_index = if detached {
                        window_layout.active_pane_index()
                    } else {
                        zoom_target.pane_index()
                    };
                    window_layout.resize_pane(zoom_pane_index, ResizePaneAdjustment::Zoom)
                },
            )
            .map(|_| ())?;
    }

    Ok(())
}

fn split_target_anchor(
    state: &HandlerState,
    target: &SplitWindowTarget,
) -> Result<(rmux_proto::SessionName, u32, u32), RmuxError> {
    match target {
        SplitWindowTarget::Session(session_name) => {
            let session = state
                .sessions
                .session(session_name)
                .ok_or_else(|| RmuxError::SessionNotFound(session_name.to_string()))?;
            Ok((
                session_name.clone(),
                session.active_window_index(),
                session.active_pane_index(),
            ))
        }
        SplitWindowTarget::Pane(target) => Ok((
            target.session_name().clone(),
            target.window_index(),
            target.pane_index(),
        )),
    }
}

/// Reads a `-l` size the way tmux's `args_percentage_and_expand` orders it:
/// the raw value decides whether it is a percentage, and only then is the part
/// before the `%`, or the whole value, expanded as a format and read.
fn split_size_cells(
    value: &str,
    direction: SplitDirection,
    pane_geometry: rmux_core::PaneGeometry,
    expand: &impl Fn(&str) -> Result<String, RmuxError>,
) -> Result<u32, RmuxError> {
    if let Some(percentage) = value.strip_suffix('%') {
        let percentage = expand(percentage)?.parse::<u8>().map_err(|error| {
            RmuxError::Server(format!("invalid split size percentage '{value}': {error}"))
        })?;
        if percentage == 0 || percentage > 100 {
            return Err(RmuxError::Server(format!(
                "invalid split size percentage '{value}': must be 1..=100"
            )));
        }
        let total = match direction {
            SplitDirection::Vertical => pane_geometry.rows(),
            SplitDirection::Horizontal => pane_geometry.cols(),
        };
        return Ok(((u32::from(total) * u32::from(percentage)) / 100).max(1));
    }

    expand(value)?
        .parse::<u32>()
        .map(|value| value.max(1))
        .map_err(|error| RmuxError::Server(format!("invalid split size '{value}': {error}")))
}
