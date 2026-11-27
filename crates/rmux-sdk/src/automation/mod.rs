//! Internal automation primitives shared with the CLI. Not a public SDK facade.
#![allow(missing_docs)]

mod broadcast;
mod snapshot;
mod stream;

pub use broadcast::validate_broadcast_response;
pub use snapshot::{project_snapshot, render_wire_cells};
pub use stream::{split_lines_bounded, validate_subscription_id, LINE_BUFFER_MAX};

pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);
