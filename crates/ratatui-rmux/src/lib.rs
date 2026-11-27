#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::invalid_codeblock_attributes)]
#![forbid(unsafe_code)]

//! Public ratatui integration crate for RMUX v1.
//!
//! `ratatui-rmux` provides three intentionally narrow building blocks
//! around `rmux-sdk`:
//!
//! * [`PaneDriver`] is the async owner of pane event I/O and state
//!   mutation. It is the *only* place RMUX behaviour is reached in
//!   this crate; it goes through `rmux-sdk` and never touches
//!   `rmux-client`, `rmux-core`, `rmux-server`, or `rmux-pty`.
//! * [`PaneState`] is the deterministic, sync, plain-data projection
//!   the driver folds events into. The same value renders the same
//!   buffer cells every time.
//! * [`PaneWidget`] is the sync ratatui widget that paints a
//!   `PaneState` into a ratatui [`Buffer`]. It performs no I/O and
//!   has no time/clock dependencies.
//!
//! The async/sync split keeps the widget safe to call from any
//! ratatui draw loop — including non-tokio hosts and unit tests —
//! while still letting the daemon-backed driver advance state
//! between draws.
//!
//! The production source and dependency budget for this crate is enforced by
//! `crates/ratatui-rmux/tests/budget.rs` plus `scripts/ratatui-rmux-budget.sh`.
//!
//! [`Buffer`]: ratatui_core::buffer::Buffer
//!
//! # Inert render quickstart
//!
//! The doctest below builds an in-memory [`rmux_sdk::PaneSnapshot`],
//! folds it into a [`PaneState`], and renders the state into a ratatui
//! [`Buffer`] without an async runtime or daemon in scope. It runs in
//! `cargo test --workspace --doc` so the render path stays compile-tested
//! at the doctest check.
//!
//! ```
//! use ratatui_core::buffer::Buffer;
//! use ratatui_core::layout::Rect;
//! use ratatui_core::widgets::Widget;
//!
//! use ratatui_rmux::{PaneState, PaneWidget};
//! use rmux_sdk::{PaneAttributes, PaneCell, PaneColor, PaneCursor, PaneGlyph, PaneSnapshot};
//!
//! let cells: Vec<PaneCell> = (0..6)
//!     .map(|_| PaneCell {
//!         glyph: PaneGlyph::new(" ".to_owned(), 1),
//!         attributes: PaneAttributes::EMPTY,
//!         foreground: PaneColor::Default,
//!         background: PaneColor::Default,
//!         underline: PaneColor::Default,
//!     })
//!     .collect();
//! let snapshot =
//!     PaneSnapshot::new(3, 2, cells, PaneCursor::new(0, 0, true, 0)).expect("3x2 snapshot");
//!
//! let state = PaneState::from_snapshot(snapshot);
//! let area = Rect::new(0, 0, 3, 2);
//! let mut buffer = Buffer::empty(area);
//! PaneWidget::new(&state).render(area, &mut buffer);
//! ```

pub mod driver;
pub mod state;
pub mod theme;
pub mod widget;

pub use driver::PaneDriver;
pub use state::{PaneLifecycle, PaneState};
pub use theme::{cell_style, color, glyph_symbol, modifier};
pub use widget::PaneWidget;
pub use widget::SceneWidget;
