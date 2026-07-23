use rmux_proto::{PaneTarget, ResizePaneAdjustment, RmuxError, SplitDirection, SplitWindowTarget};

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
        .map(|size| split_size_cells(size, direction, pane.geometry()))
        .transpose()?;

    Ok(SplitWindowEffects {
        detached_anchor: detached
            .then(|| PaneTarget::with_window(session_name.clone(), window_index, pane_index)),
        size: split_size,
    })
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
        state.mutate_session_and_resize_window_terminal(
            &session_name,
            window_index,
            |session| {
                let Some(window) = session.window_at(window_index) else {
                    return Ok(());
                };
                if window.is_zoomed() {
                    return Ok(());
                }
                // tmux `split-window -d -Z` zooms the window around the pane
                // that is already active; only a non-detached -Z zooms (and
                // activates) the new pane. Zooming the anchor target here used
                // to re-select it, re-introducing the active/last/active_point
                // side effects a detached split must not have.
                let zoom_pane_index = if detached {
                    window.active_pane_index()
                } else {
                    zoom_target.pane_index()
                };
                session.resize_pane_in_window(
                    window_index,
                    zoom_pane_index,
                    ResizePaneAdjustment::Zoom,
                )?;
                Ok(())
            },
        )?;
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

fn split_size_cells(
    value: &str,
    direction: SplitDirection,
    pane_geometry: rmux_core::PaneGeometry,
) -> Result<u32, RmuxError> {
    if let Some(percentage) = value.strip_suffix('%') {
        let percentage = percentage.parse::<u8>().map_err(|error| {
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

    value
        .parse::<u32>()
        .map(|value| value.max(1))
        .map_err(|error| RmuxError::Server(format!("invalid split size '{value}': {error}")))
}
