use std::collections::BTreeMap;

use rmux_proto::RmuxError;

use super::super::target_error::invalid_window_target;
use super::super::Session;
use crate::{AlertFlags, WindowId, WINLINK_ALERTFLAGS};

pub(in crate::session) fn select_existing_window(
    window_index: u32,
    active_window: &mut u32,
    last_window: &mut Option<u32>,
    window_stack: &mut Vec<u32>,
    group_initial_window_id: &mut Option<WindowId>,
    winlink_alert_flags: &mut BTreeMap<u32, AlertFlags>,
) {
    if *active_window == window_index {
        return;
    }

    crate::session::visit_history::record_visit(window_stack, *active_window, window_index);
    *last_window = Some(*active_window);
    *active_window = window_index;
    *group_initial_window_id = None;
    if let Some(flags) = winlink_alert_flags.get_mut(&window_index) {
        flags.remove(WINLINK_ALERTFLAGS);
    }
}

impl Session {
    /// Selects the active window by index.
    pub fn select_window(&mut self, window_index: u32) -> Result<(), RmuxError> {
        if !self.windows.contains_key(&window_index) {
            return Err(invalid_window_target(&self.name, window_index));
        }

        select_existing_window(
            window_index,
            &mut self.active_window,
            &mut self.last_window,
            &mut self.window_stack,
            &mut self.group_initial_window_id,
            &mut self.winlink_alert_flags,
        );

        Ok(())
    }

    /// Selects the next window in sparse index order, wrapping to the lowest index.
    pub fn next_window(&mut self) -> Result<u32, RmuxError> {
        self.next_window_alert_mode(false)
    }

    /// Selects the next alerted window in sparse index order.
    pub fn next_window_with_alerts(&mut self) -> Result<u32, RmuxError> {
        self.next_window_alert_mode(true)
    }

    fn next_window_alert_mode(&mut self, alerts_only: bool) -> Result<u32, RmuxError> {
        if self.windows.len() <= 1 {
            return Err(RmuxError::Message("no next window".to_owned()));
        }

        let next_window = self
            .ordered_window_indexes_after(self.active_window, true)
            .find(|window_index| {
                !alerts_only
                    || self
                        .winlink_alert_flags(*window_index)
                        .intersects(crate::WINLINK_ALERTFLAGS)
            })
            .ok_or_else(|| RmuxError::Message("no next window".to_owned()))?;
        self.select_window(next_window)?;
        Ok(next_window)
    }

    /// Selects the previous window in sparse index order, wrapping to the highest index.
    pub fn previous_window(&mut self) -> Result<u32, RmuxError> {
        self.previous_window_alert_mode(false)
    }

    /// Selects the previous alerted window in sparse index order.
    pub fn previous_window_with_alerts(&mut self) -> Result<u32, RmuxError> {
        self.previous_window_alert_mode(true)
    }

    fn previous_window_alert_mode(&mut self, alerts_only: bool) -> Result<u32, RmuxError> {
        if self.windows.len() <= 1 {
            return Err(RmuxError::Message("no previous window".to_owned()));
        }

        let previous_window = self
            .ordered_window_indexes_after(self.active_window, false)
            .find(|window_index| {
                !alerts_only
                    || self
                        .winlink_alert_flags(*window_index)
                        .intersects(crate::WINLINK_ALERTFLAGS)
            })
            .ok_or_else(|| RmuxError::Message("no previous window".to_owned()))?;
        self.select_window(previous_window)?;
        Ok(previous_window)
    }

    /// Selects the most recently active window.
    pub fn last_window(&mut self) -> Result<u32, RmuxError> {
        let last_window = self
            .last_window
            .ok_or_else(|| RmuxError::Message("no last window".to_owned()))?;
        self.select_window(last_window)?;
        Ok(last_window)
    }

    /// Restores tmux's winlink stack fallback after unlinking an active linked slot.
    pub fn restore_last_window_after_active_unlink(&mut self) {
        if self.last_window.is_some() {
            return;
        }
        let next_last = self
            .ordered_window_indexes_after(self.active_window, true)
            .next();
        self.last_window = next_last;
    }

    fn ordered_window_indexes_after(
        &self,
        start_window: u32,
        forward: bool,
    ) -> impl Iterator<Item = u32> + '_ {
        let ordered = self.windows.keys().copied().collect::<Vec<_>>();
        let Some(start_index) = ordered
            .iter()
            .position(|window_index| *window_index == start_window)
        else {
            return Vec::new().into_iter();
        };

        let len = ordered.len();
        let mut next = Vec::with_capacity(len.saturating_sub(1));
        for offset in 1..len {
            let index = if forward {
                (start_index + offset) % len
            } else {
                (start_index + len - offset) % len
            };
            next.push(ordered[index]);
        }
        next.into_iter()
    }
}
