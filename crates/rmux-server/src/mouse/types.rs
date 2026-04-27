use std::time::Instant;

use rmux_core::{PaneGeometry, PaneId};
use rmux_proto::PaneTarget;

use crate::input_keys::MouseForwardEvent;
pub(crate) use crate::status_ranges::{StatusLineLayout, StatusRange, StatusRangeType};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum MouseLocation {
    #[default]
    Nowhere,
    Pane,
    Status,
    StatusLeft,
    StatusRight,
    StatusDefault,
    ScrollbarUp,
    ScrollbarSlider,
    ScrollbarDown,
    Border,
    Control(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MouseEventKind {
    MouseMove,
    MouseDown,
    MouseUp,
    MouseDrag,
    MouseDragEnd,
    WheelDown,
    WheelUp,
    SecondClick,
    DoubleClick,
    TripleClick,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrollbarPosition {
    Left,
    Right,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneScrollbarsMode {
    Off,
    Modal,
    On,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneBorderStatus {
    Off,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneScrollbar {
    pub(crate) position: ScrollbarPosition,
    pub(crate) width: u16,
    pub(crate) pad: u16,
    pub(crate) slider_y: u16,
    pub(crate) slider_h: u16,
}

impl PaneScrollbar {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_view(
        rows: u16,
        history_size: usize,
        alternate_on: bool,
        mode: PaneScrollbarsMode,
        position: ScrollbarPosition,
        width: u16,
        pad: u16,
        copy_mode_offset: Option<usize>,
    ) -> Option<Self> {
        if alternate_on || width == 0 || rows == 0 {
            return None;
        }
        if matches!(mode, PaneScrollbarsMode::Off) {
            return None;
        }
        if matches!(mode, PaneScrollbarsMode::Modal) && copy_mode_offset.is_none() {
            return None;
        }

        let sb_h = usize::from(rows);
        let (slider_y, slider_h) = if let Some(offset) = copy_mode_offset {
            let total_height = history_size.saturating_add(sb_h).max(1);
            let slider_h =
                ((sb_h as f64) * ((sb_h as f64) / (total_height as f64))).floor() as usize;
            let slider_y =
                (((sb_h + 1) as f64) * ((offset as f64) / (total_height as f64))).floor() as usize;
            (slider_y, slider_h)
        } else {
            let total_height = history_size.saturating_add(sb_h).max(1);
            let percent_view = (sb_h as f64) / (total_height as f64);
            let slider_h = ((sb_h as f64) * percent_view).floor() as usize;
            let slider_y = sb_h.saturating_sub(slider_h.max(1));
            (slider_y, slider_h)
        };

        let slider_h = slider_h.max(1).min(sb_h);
        let slider_y = slider_y.min(sb_h.saturating_sub(slider_h));
        Some(Self {
            position,
            width,
            pad,
            slider_y: slider_y as u16,
            slider_h: slider_h as u16,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BorderControlRange {
    pub(crate) x: std::ops::RangeInclusive<u16>,
    pub(crate) y: u16,
    pub(crate) control: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneMouseTarget {
    pub(crate) pane_id: PaneId,
    pub(crate) pane_target: Option<PaneTarget>,
    pub(crate) window_id: u32,
    pub(crate) geometry: PaneGeometry,
    pub(crate) scrollbar: Option<PaneScrollbar>,
    pub(crate) border_controls: Vec<BorderControlRange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MouseLayout {
    pub(crate) session_id: u32,
    pub(crate) status_at: Option<u16>,
    pub(crate) status_lines: u16,
    pub(crate) status: Option<StatusLineLayout>,
    pub(crate) pane_border_status: PaneBorderStatus,
    pub(crate) focus_follows_mouse: bool,
    pub(crate) active_pane: Option<PaneId>,
    pub(crate) panes: Vec<PaneMouseTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MouseDragHandler {
    CopyModeSelection { target: PaneTarget },
    CopyModeScrollbar { target: PaneTarget },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttachedMouseEvent {
    pub(crate) raw: MouseForwardEvent,
    pub(crate) session_id: u32,
    pub(crate) window_id: Option<u32>,
    pub(crate) pane_id: Option<PaneId>,
    pub(crate) pane_target: Option<PaneTarget>,
    pub(crate) location: MouseLocation,
    pub(crate) status_at: Option<u16>,
    pub(crate) status_lines: u16,
    pub(crate) ignore: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClassifiedMouseEvent {
    pub(crate) key: rmux_core::KeyCode,
    pub(crate) event: AttachedMouseEvent,
    pub(crate) focus_target: Option<PaneId>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ClientMouseState {
    pub(crate) click_deadline: Option<Instant>,
    pub(crate) double_click_pending: bool,
    pub(crate) triple_click_pending: bool,
    pub(crate) click_button: u16,
    pub(crate) click_location: MouseLocation,
    pub(crate) click_pane: Option<PaneId>,
    pub(crate) click_event: Option<AttachedMouseEvent>,
    pub(crate) drag_flag: u8,
    pub(crate) scrolling_flag: bool,
    pub(crate) slider_mpos: i32,
    pub(crate) current_event: Option<AttachedMouseEvent>,
    pub(crate) drag_handler: Option<MouseDragHandler>,
}
