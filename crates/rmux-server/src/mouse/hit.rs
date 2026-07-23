use rmux_core::PaneId;
use rmux_proto::PaneTarget;

use crate::input_keys::MouseForwardEvent;

use super::types::{
    AttachedMouseEvent, MouseLayout, MouseLocation, PaneBorderStatus, PaneMouseTarget,
    ScrollbarPosition, StatusLineLayout, StatusRangeType,
};

#[derive(Debug, Clone)]
pub(super) struct MouseHit {
    pub(super) location: MouseLocation,
    session_id: u32,
    window_id: Option<u32>,
    pane_id: Option<PaneId>,
    pane_target: Option<PaneTarget>,
    pub(super) slider_mpos: Option<u16>,
}

impl MouseHit {
    fn nowhere(session_id: u32) -> Self {
        Self {
            location: MouseLocation::Nowhere,
            session_id,
            window_id: None,
            pane_id: None,
            pane_target: None,
            slider_mpos: None,
        }
    }

    fn status_default(session_id: u32) -> Self {
        Self {
            location: MouseLocation::StatusDefault,
            session_id,
            window_id: None,
            pane_id: None,
            pane_target: None,
            slider_mpos: None,
        }
    }
}

pub(super) fn resolve_mouse_hit(
    layout: &MouseLayout,
    x: u16,
    y: u16,
    scrolling: bool,
    current: Option<&AttachedMouseEvent>,
) -> MouseHit {
    if let Some(status_at) = layout.status_at {
        if y >= status_at && y < status_at.saturating_add(layout.status_lines) {
            let status_line = y.saturating_sub(status_at);
            if let Some(status) = &layout.status {
                return status_hit(layout.session_id, status, x, status_line);
            }
            return MouseHit::status_default(layout.session_id);
        }
    }

    if scrolling {
        return MouseHit {
            location: MouseLocation::ScrollbarSlider,
            session_id: layout.session_id,
            window_id: current.and_then(|event| event.window_id),
            pane_id: current.and_then(|event| event.pane_id),
            pane_target: current.and_then(|event| event.pane_target.clone()),
            slider_mpos: None,
        };
    }

    let py = if layout.status_at == Some(0) && y >= layout.status_lines {
        y - layout.status_lines
    } else if layout.status_at.is_some_and(|status_at| y >= status_at) {
        layout.status_at.unwrap_or_default().saturating_sub(1)
    } else {
        y
    };

    let Some(pane) = active_pane_at(layout, x, py) else {
        return MouseHit::nowhere(layout.session_id);
    };
    let (location, slider_mpos) = check_mouse_in_pane(pane, x, py, layout.pane_border_status);
    let location = match location {
        MouseLocation::Border => pane
            .border_controls
            .iter()
            .find(|range| range.y == py && range.x.contains(&x))
            .map(|range| MouseLocation::Control(range.control))
            .unwrap_or(MouseLocation::Border),
        other => other,
    };

    MouseHit {
        location,
        session_id: layout.session_id,
        window_id: Some(pane.window_id),
        pane_id: Some(pane.pane_id),
        pane_target: pane.pane_target.clone(),
        slider_mpos,
    }
}

pub(super) fn hit_to_attached_event(
    layout: &MouseLayout,
    raw: MouseForwardEvent,
    hit: MouseHit,
    ignore: bool,
) -> Option<AttachedMouseEvent> {
    if hit.location == MouseLocation::Nowhere {
        return None;
    }
    Some(AttachedMouseEvent {
        raw,
        session_id: hit.session_id,
        window_id: hit.window_id,
        pane_id: hit.pane_id,
        pane_target: hit.pane_target,
        location: hit.location,
        status_at: layout.status_at,
        status_lines: layout.status_lines,
        ignore,
    })
}

fn status_hit(session_id: u32, status: &StatusLineLayout, x: u16, line: u16) -> MouseHit {
    let Some(range) = status
        .ranges
        .iter()
        .find(|range| range.line == line && range.x.contains(&x))
    else {
        return MouseHit::status_default(session_id);
    };
    match &range.kind {
        StatusRangeType::None => MouseHit::nowhere(session_id),
        StatusRangeType::Left => MouseHit {
            location: MouseLocation::StatusLeft,
            session_id,
            window_id: None,
            pane_id: None,
            pane_target: None,
            slider_mpos: None,
        },
        StatusRangeType::Right => MouseHit {
            location: MouseLocation::StatusRight,
            session_id,
            window_id: None,
            pane_id: None,
            pane_target: None,
            slider_mpos: None,
        },
        StatusRangeType::Pane(pane_id) => MouseHit {
            location: MouseLocation::Status,
            session_id,
            window_id: None,
            pane_id: Some(*pane_id),
            pane_target: None,
            slider_mpos: None,
        },
        StatusRangeType::Window(window_id) => MouseHit {
            location: MouseLocation::Status,
            session_id,
            window_id: Some(*window_id),
            pane_id: None,
            pane_target: None,
            slider_mpos: None,
        },
        StatusRangeType::Session(target_session_id) => MouseHit {
            location: MouseLocation::Status,
            session_id: *target_session_id,
            window_id: None,
            pane_id: None,
            pane_target: None,
            slider_mpos: None,
        },
        StatusRangeType::User => MouseHit {
            location: MouseLocation::Status,
            session_id,
            window_id: None,
            pane_id: None,
            pane_target: None,
            slider_mpos: None,
        },
        StatusRangeType::Control(control) => MouseHit {
            location: MouseLocation::Control(*control),
            session_id,
            window_id: None,
            pane_id: None,
            pane_target: None,
            slider_mpos: None,
        },
    }
}

fn active_pane_at(layout: &MouseLayout, x: u16, y: u16) -> Option<&PaneMouseTarget> {
    layout.panes.iter().find(|pane| {
        let scrollbar_width = pane
            .scrollbar
            .as_ref()
            .map(|scrollbar| scrollbar.width.saturating_add(scrollbar.pad))
            .unwrap_or(0);
        let (xoff, sx) = match pane.scrollbar.as_ref().map(|scrollbar| scrollbar.position) {
            Some(ScrollbarPosition::Left) => (
                pane.geometry.x().saturating_sub(scrollbar_width),
                pane.geometry.cols().saturating_add(scrollbar_width),
            ),
            _ => (
                pane.geometry.x(),
                pane.geometry.cols().saturating_add(scrollbar_width),
            ),
        };
        let yoff = pane.geometry.y();
        let sy = pane.geometry.rows();
        if x < xoff || x > xoff.saturating_add(sx) {
            return false;
        }

        match layout.pane_border_status {
            PaneBorderStatus::Top => y >= yoff.saturating_sub(1) && y <= yoff.saturating_add(sy),
            PaneBorderStatus::Off | PaneBorderStatus::Bottom => {
                !(y < yoff || y > yoff.saturating_add(sy))
            }
        }
    })
}

fn check_mouse_in_pane(
    pane: &PaneMouseTarget,
    px: u16,
    py: u16,
    pane_border_status: PaneBorderStatus,
) -> (MouseLocation, Option<u16>) {
    let pane_status_line = match pane_border_status {
        PaneBorderStatus::Top => Some(pane.geometry.y().saturating_sub(1)),
        PaneBorderStatus::Bottom => Some(pane.geometry.y().saturating_add(pane.geometry.rows())),
        PaneBorderStatus::Off => None,
    };

    if pane_status_line == Some(py) {
        return (MouseLocation::Border, None);
    }

    let inside_vertical =
        py >= pane.geometry.y() && py < pane.geometry.y().saturating_add(pane.geometry.rows());

    if inside_vertical {
        if let Some(scrollbar) = &pane.scrollbar {
            let left = match scrollbar.position {
                ScrollbarPosition::Right => pane
                    .geometry
                    .x()
                    .saturating_add(pane.geometry.cols())
                    .saturating_add(scrollbar.pad),
                ScrollbarPosition::Left => pane
                    .geometry
                    .x()
                    .saturating_sub(scrollbar.pad + scrollbar.width),
            };
            let right = left.saturating_add(scrollbar.width);
            if px >= left && px < right {
                let slider_top = pane.geometry.y().saturating_add(scrollbar.slider_y);
                let slider_bottom = slider_top.saturating_add(scrollbar.slider_h.saturating_sub(1));
                if py < slider_top {
                    return (MouseLocation::ScrollbarUp, None);
                }
                if py <= slider_bottom {
                    return (
                        MouseLocation::ScrollbarSlider,
                        Some(
                            py.saturating_sub(scrollbar.slider_y)
                                .saturating_sub(pane.geometry.y()),
                        ),
                    );
                }
                return (MouseLocation::ScrollbarDown, None);
            }

            let content_left = pane.geometry.x();
            let content_right = content_left.saturating_add(pane.geometry.cols());
            let (padding_left, padding_right) = match scrollbar.position {
                ScrollbarPosition::Left => {
                    (content_left.saturating_sub(scrollbar.pad), content_left)
                }
                ScrollbarPosition::Right => {
                    (content_right, content_right.saturating_add(scrollbar.pad))
                }
            };
            if px >= padding_left && px < padding_right {
                // tmux treats the gutter between content and track as a pane
                // binding location, while application passthrough still
                // rejects it against the content geometry.
                return (MouseLocation::Pane, None);
            }
        }
        if px >= pane.geometry.x() && px < pane.geometry.x().saturating_add(pane.geometry.cols()) {
            return (MouseLocation::Pane, None);
        }
    }

    let right_border = pane
        .geometry
        .x()
        .saturating_add(pane.geometry.cols())
        .saturating_add(
            pane.scrollbar
                .as_ref()
                .filter(|scrollbar| scrollbar.position == ScrollbarPosition::Right)
                .map(|scrollbar| scrollbar.width.saturating_add(scrollbar.pad))
                .unwrap_or(0),
        );
    if py >= pane.geometry.y().saturating_sub(1)
        && py <= pane.geometry.y().saturating_add(pane.geometry.rows())
        && px == right_border
    {
        return (MouseLocation::Border, None);
    }
    if px >= pane.geometry.x().saturating_sub(1)
        && px <= pane.geometry.x().saturating_add(pane.geometry.cols())
        && (py == pane.geometry.y().saturating_sub(1)
            || py == pane.geometry.y().saturating_add(pane.geometry.rows()))
    {
        return (MouseLocation::Border, None);
    }

    (MouseLocation::Nowhere, None)
}
