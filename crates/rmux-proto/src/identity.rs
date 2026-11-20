//! Compatibility paths for the canonical identity vocabulary.
//!
//! Values and session-name validation are owned by `rmux-types`. Allocation,
//! lookup, and resolution remain in `rmux-core::session`.

pub use rmux_types::{PaneId, SessionId, SessionName, SessionNameError, WindowId};
