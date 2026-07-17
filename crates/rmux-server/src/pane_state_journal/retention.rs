//! Byte accounting for variable-sized pane-state journal records.

use std::mem::size_of;

use rmux_proto::ForegroundStateDto;

use super::{PaneStateChange, PaneStateRecord};

pub(super) fn retained_record_bytes(record: &PaneStateRecord) -> usize {
    size_of::<PaneStateRecord>().saturating_add(change_heap_bytes(&record.change))
}

fn change_heap_bytes(change: &PaneStateChange) -> usize {
    match change {
        PaneStateChange::TitleChanged { old, new } => {
            string_bytes(old).saturating_add(string_bytes(new))
        }
        PaneStateChange::OptionSet { name, old, new } => string_bytes(name)
            .saturating_add(optional_string_bytes(old))
            .saturating_add(string_bytes(new)),
        PaneStateChange::OptionUnset { name, old } => {
            string_bytes(name).saturating_add(optional_string_bytes(old))
        }
        PaneStateChange::ForegroundChanged { old, new } => {
            foreground_heap_bytes(old).saturating_add(foreground_heap_bytes(new))
        }
        PaneStateChange::Closed { .. } => 0,
    }
}

fn string_bytes(value: &String) -> usize {
    value.capacity()
}

fn optional_string_bytes(value: &Option<String>) -> usize {
    value.as_ref().map_or(0, string_bytes)
}

fn foreground_heap_bytes(state: &ForegroundStateDto) -> usize {
    optional_string_bytes(&state.command)
        .saturating_add(optional_string_bytes(&state.cwd))
        .saturating_add(optional_string_bytes(&state.exe))
}
