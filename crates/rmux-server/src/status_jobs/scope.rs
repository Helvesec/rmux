use std::sync::atomic::{AtomicU64, Ordering};

/// Execution ownership is independent of the client used for format variables.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum StatusJobOwner {
    Status(Option<(bool, u64)>),
    Command {
        control: bool,
        pid: u32,
        generation: u64,
    },
    Ephemeral(u64),
    Subscription(u64),
}

pub(crate) fn new_job_owner_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct StatusJobScope {
    pub(crate) owner: StatusJobOwner,
    pub(crate) session: Option<u32>,
    pub(crate) window: Option<u32>,
    /// Session-local occurrence of `window`. Linked windows share a WindowId
    /// but may expand index-dependent commands differently.
    pub(crate) window_index: Option<u32>,
    pub(crate) pane: Option<u32>,
    pub(crate) client: Option<(bool, u64)>,
}
