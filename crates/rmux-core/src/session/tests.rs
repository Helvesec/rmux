use std::collections::BTreeSet;

use super::{
    BreakPaneOptions, PaneJoinOptions, PaneSwapOptions, Session, SessionPaneTarget, SessionStore,
};
use crate::{PaneGeometry, PaneId};
use rmux_proto::{
    LayoutName, PaneSplitSize, ResizePaneAdjustment, RmuxError, RotateWindowDirection, SessionName,
    SplitDirection, TerminalSize,
};

#[path = "window_tests.rs"]
mod window_tests;
fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn layout_string(body: &str) -> String {
    format!("{:04x},{}", crate::layout::layout_checksum(body), body)
}

#[path = "tests/pane_basics.rs"]
mod pane_basics;

#[path = "tests/pane_layout_resize.rs"]
mod pane_layout_resize;

#[path = "tests/pane_transfer.rs"]
mod pane_transfer;

#[path = "tests/store_sessions.rs"]
mod store_sessions;

#[path = "tests/group_sync.rs"]
mod group_sync;

#[path = "tests/pane_transfer_edge_cases.rs"]
mod pane_transfer_edge_cases;
