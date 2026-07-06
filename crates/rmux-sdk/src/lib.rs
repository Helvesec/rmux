#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::invalid_codeblock_attributes)]
#![forbid(unsafe_code)]

//! Public daemon-backed RMUX SDK.
//!
//! v1 exposes live facade handles, session builders, waits, pane streams,
//! snapshots, and command escape hatches over the rmux daemon.
//!
//! `rmux-sdk` is a public integration peer of `rmux-client` and must not
//! depend on `rmux-client`, `rmux-core`, `rmux-server`, or `rmux-pty` as
//! normal dependencies. The authoritative identity newtypes
//! ([`SessionName`], [`SessionId`], [`WindowId`], [`PaneId`]) live in
//! `rmux-proto` and are re-exported here so SDK users import them through
//! `rmux_sdk` without ever depending on those internal crates.
//!
//! # Quickstart
//!
//! The shortest daemon-backed SDK program connects to a daemon, starting one
//! through the platform hidden-daemon path if needed, then ensures a session:
//!
//! ```no_run
//! use std::time::Duration;
//!
//! use rmux_sdk::{
//!     EnsureSession, EnsureSessionPolicy, ProcessSpec, Rmux, RmuxEndpoint, SessionName,
//!     TerminalSizeSpec,
//! };
//!
//! # async fn run() -> rmux_sdk::Result<()> {
//! let rmux = Rmux::builder()
//!     .default_timeout(Duration::from_secs(5))
//!     .connect_or_start()
//!     .await?;
//! assert!(!matches!(rmux.endpoint(), RmuxEndpoint::Default));
//!
//! let session = SessionName::new("quickstart").expect("valid session name");
//! let session = rmux
//!     .ensure_session(
//!         EnsureSession::named(session)
//!             .policy(EnsureSessionPolicy::CreateOrReuse)
//!             .detached(true)
//!             .size(TerminalSizeSpec::new(120, 32))
//!             .process(ProcessSpec::default()),
//!     )
//!     .await?;
//! assert!(session.exists().await?);
//! # Ok(())
//! # }
//! ```
//!
pub mod actions;
pub mod bootstrap;
pub mod broadcast;
pub(crate) mod capabilities;
pub mod capture;
pub mod command;
pub mod diagnostics;
pub mod discovery;
pub mod ensure;
pub mod error;
pub mod events;
pub mod extract;
pub mod handles;
pub mod info;
pub mod input;
pub mod layout;
pub mod load_state;
pub mod locator;
pub mod pane_set;
pub mod snapshot;
pub mod spec;
pub mod trace;
pub mod types;
pub mod wait;
#[cfg(feature = "web")]
pub mod web_share;

#[allow(dead_code)]
pub(crate) mod transport;

pub use actions::{FillStrategy, PaneKeyboard, PaneMouse, PaneSetKeyboard};
pub use broadcast::{
    BroadcastPaneFailure, BroadcastPaneSuccess, BroadcastResult, Input, PartialBroadcastFailure,
};
pub use capture::{CaptureBuilder, CapturedRegion, Rect};
pub use command::{CommandRun, RmuxCommand, RmuxCommandKind};
pub use diagnostics::{
    command_feature_id, protocol_diagnostic, unsupported_feature_id, Diagnostic,
    DiagnosticSeverity, FEATURE_DAEMON_SHUTDOWN, FEATURE_PROTOCOL_CAPABILITIES,
    FEATURE_PROTOCOL_WIRE_VERSION, FEATURE_TRANSPORT_UNIX_SOCKET, FEATURE_TRANSPORT_WINDOWS_PIPE,
};
pub use discovery::{DiscoveredPane, DiscoveredSession, PaneFinder, SessionFinder};
pub use ensure::{EnsureSession, EnsureSessionPolicy};
pub use error::{CollectError, Result, RmuxError};
pub use events::{
    PaneCommandStatus, PaneCommandSummary, PaneDisconnectReason, PaneEvent, PaneExitReason,
    PaneLagNotice, PaneLineItem, PaneLineStream, PaneNotification, PaneOutputChunk,
    PaneOutputStart, PaneOutputStream, PanePermissionScope, PaneRecentOutput, PaneRenderStream,
    RenderUpdate,
};
pub use extract::{CollectedPaneOutput, PaneTextMatch};
pub use handles::{
    CleanupPolicy, ForegroundSource, ForegroundSources, ForegroundState, LeaseState,
    NewWindowBuilder, OwnedSession, OwnedSessionBuilder, OwnedSessionSignalHandlers, Pane,
    PaneCapture, PaneCaptureBuilder, PaneCloseOutcome, PaneOptionMutation, PaneRespawnOptions,
    PaneSpawnBuilder, PaneSplitBuilder, PaneStateEvent, PaneStateEventStream,
    PaneStateEventsOptions, PaneStateOption, Rmux, RmuxBuilder, Session, SplitDirection, Window,
    WindowCloseOutcome, WindowPane,
};
pub use info::{InfoSnapshot, PaneExitState, PaneInfo, PaneProcessState, SessionInfo, WindowInfo};
pub use input::{
    DetachChord, DetachDetector, DetachOutcome, KeyCode, KeyConversionError, KeyEvent, KeyModifiers,
};
pub use layout::{GridLayoutBuilder, LayoutPaneBuilder, SessionLayoutBuilder};
pub use load_state::{TerminalLoadState, TerminalLoadStateWait};
pub use locator::{
    Locator, LocatorAssertion, LocatorExpectation, LocatorFilter, LocatorMatch, LocatorState,
    LocatorText, LocatorWait,
};
pub use pane_set::{
    PaneSet, PaneSetAny, PaneSetBatch, PaneSetExpectation, PaneSetFailure, PaneSetSuccess,
    PaneSetVisibleTextOutcome, PaneSetVisibleTextWait,
};
pub use rmux_proto::LayoutName;
#[cfg(feature = "web")]
pub use rmux_proto::{WebTerminalPalette, WebTerminalTheme};
pub use snapshot::{
    PaneAttributes, PaneCell, PaneColor, PaneCursor, PaneGlyph, PaneSnapshot,
    PaneSnapshotShapeError,
};
pub use spec::{
    AttachSessionReuse, AttachSessionSpec, ClientTerminalSpec, NewSessionReuse, NewSessionSpec,
    ProcessCommandSpec, ProcessSpec, RefreshClientSpec, SplitDirectionSpec, SplitSpec,
    SplitTargetSpec, SubscriptionSpec,
};
pub use trace::{RmuxTraceBuilder, TraceSession};
pub use types::{
    PaneId, PaneRef, RmuxEndpoint, SessionId, SessionName, TargetRef, TerminalSizeSpec, WindowId,
    WindowRef,
};
pub use wait::{ArmedWait, VisibleTextExpectation, VisibleTextWait, WaitTimeoutError};
#[cfg(feature = "web")]
pub use web_share::{
    WebConfigInfo, WebShareBuilder, WebShareHandle, WebShareLookup, WebShareSummary,
};
