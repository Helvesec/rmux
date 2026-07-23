use std::time::Instant;

use rmux_core::{PaneGeometry, PaneId};
use rmux_proto::PaneTarget;

use crate::input_keys::MouseForwardEvent;
#[cfg(test)]
pub(crate) use crate::pane_scrollbar::PaneScrollbarsMode;
pub(crate) use crate::pane_scrollbar::{PaneScrollbar, ScrollbarPosition};
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
pub(crate) enum PaneBorderStatus {
    Off,
    Top,
    Bottom,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PanePassthroughMouseEvent {
    pub(crate) event: AttachedMouseEvent,
    pub(crate) focus_target: Option<PaneId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MouseClickTimerToken {
    deadline: Instant,
    generation: u64,
}

impl MouseClickTimerToken {
    pub(crate) const fn deadline(self) -> Instant {
        self.deadline
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ClientMouseState {
    pub(crate) click_deadline: Option<Instant>,
    pub(crate) click_timer_generation: u64,
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
    pub(crate) drag_start_event: Option<AttachedMouseEvent>,
    pub(crate) drag_handler: Option<MouseDragHandler>,
}

impl ClientMouseState {
    pub(crate) const fn click_deadline(&self) -> Option<Instant> {
        self.click_deadline
    }

    pub(crate) fn click_timer_token(&self) -> Option<MouseClickTimerToken> {
        self.click_deadline.map(|deadline| MouseClickTimerToken {
            deadline,
            generation: self.click_timer_generation,
        })
    }

    pub(crate) fn click_timer_matches(&self, token: MouseClickTimerToken) -> bool {
        self.click_timer_token() == Some(token)
    }

    pub(crate) fn arm_click_timer(&mut self, deadline: Instant) {
        self.click_timer_generation = self.click_timer_generation.saturating_add(1);
        self.click_deadline = Some(deadline);
    }

    pub(crate) fn clear_click_timer_if_current(&mut self, token: MouseClickTimerToken) -> bool {
        if !self.click_timer_matches(token) {
            return false;
        }
        self.clear_click_timer_state();
        true
    }

    pub(crate) fn reset_for_session_switch(&mut self) {
        let next_generation = self.click_timer_generation.saturating_add(1);
        *self = Self {
            click_timer_generation: next_generation,
            slider_mpos: -1,
            ..Self::default()
        };
    }

    pub(crate) fn clear_click_timer_state(&mut self) {
        self.click_deadline = None;
        self.double_click_pending = false;
        self.triple_click_pending = false;
        self.click_event = None;
    }
}
