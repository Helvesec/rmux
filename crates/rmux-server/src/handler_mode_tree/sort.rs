use std::cmp::Ordering;

use rmux_core::{BufferView, Session, Window};
use rmux_proto::SessionName;

use super::mode_tree_model::{ClientSnapshot, SortOrder};

pub(super) fn sort_sessions(
    sessions: &mut Vec<(&SessionName, &Session)>,
    sort_order: Option<SortOrder>,
    reversed: bool,
) {
    sessions.sort_by(|(left_name, left), (right_name, right)| {
        let ordering = match sort_order.unwrap_or(SortOrder::Index) {
            SortOrder::Index => left.id().cmp(&right.id()),
            SortOrder::Name => left_name.as_str().cmp(right_name.as_str()),
            SortOrder::Activity => left.active_window_index().cmp(&right.active_window_index()),
            SortOrder::Creation => left.id().cmp(&right.id()),
            SortOrder::Size => left.window().size().cols.cmp(&right.window().size().cols),
        };
        stable_order(ordering, reversed, left_name.as_str(), right_name.as_str())
    });
}

pub(super) fn sort_windows(
    windows: &mut Vec<(&u32, &Window)>,
    sort_order: Option<SortOrder>,
    reversed: bool,
) {
    windows.sort_by(|(left_index, left), (right_index, right)| {
        let ordering = match sort_order.unwrap_or(SortOrder::Index) {
            SortOrder::Index => left_index.cmp(right_index),
            SortOrder::Name => left
                .name()
                .unwrap_or_default()
                .cmp(right.name().unwrap_or_default()),
            SortOrder::Activity => left.active_pane_index().cmp(&right.active_pane_index()),
            SortOrder::Creation => left_index.cmp(right_index),
            SortOrder::Size => left.size().cols.cmp(&right.size().cols),
        };
        stable_order(ordering, reversed, left_index, right_index)
    });
}

pub(super) fn sort_buffer_entries(
    entries: &mut [BufferView<'_>],
    sort_order: Option<SortOrder>,
    reversed: bool,
) {
    entries.sort_by(|left, right| {
        let ordering = match sort_order.unwrap_or(SortOrder::Creation) {
            SortOrder::Creation | SortOrder::Index => left.order().cmp(&right.order()),
            SortOrder::Name => left.name().cmp(right.name()),
            SortOrder::Size => left.size().cmp(&right.size()),
            SortOrder::Activity => left.created().cmp(&right.created()),
        };
        stable_order(ordering, reversed, left.name(), right.name())
    });
}

pub(super) fn sort_clients(
    clients: &mut [ClientSnapshot],
    sort_order: Option<SortOrder>,
    reversed: bool,
) {
    clients.sort_by(|left, right| {
        let ordering = match sort_order.unwrap_or(SortOrder::Name) {
            SortOrder::Name => left.label.cmp(&right.label),
            SortOrder::Size => (left.width, left.height).cmp(&(right.width, right.height)),
            SortOrder::Creation | SortOrder::Index => left.attach_id.cmp(&right.attach_id),
            SortOrder::Activity => right.activity.cmp(&left.activity),
        };
        stable_order(ordering, reversed, &left.label, &right.label)
    });
}

pub(super) fn stable_order<T: Ord>(
    ordering: Ordering,
    reversed: bool,
    left: T,
    right: T,
) -> Ordering {
    let primary = if reversed {
        ordering.reverse()
    } else {
        ordering
    };
    if primary.is_eq() {
        left.cmp(&right)
    } else {
        primary
    }
}

pub(super) fn split_name_value(line: &str) -> (String, String) {
    match line.split_once(' ') {
        Some((name, value)) => (name.to_owned(), value.to_owned()),
        None => (line.to_owned(), String::new()),
    }
}

pub(super) fn sort_order_name(sort_order: SortOrder) -> &'static str {
    match sort_order {
        SortOrder::Index => "index",
        SortOrder::Name => "name",
        SortOrder::Activity => "activity",
        SortOrder::Creation => "creation",
        SortOrder::Size => "size",
    }
}
