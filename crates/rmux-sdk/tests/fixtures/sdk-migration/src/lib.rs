//! Combined 0.10-to-pre-1.0 consumer. The RMUX dependency is only rmux-sdk.
//!
//! Async functions are real application examples, compiled but never polled by
//! this fixture's tests. Calling them can start a daemon or mutate a session.
//! See the SDK README for migration details and the three feature-mode commands.
//!
//! A surface owner survives cancellation of a wait. Drop it when finished:
//! ```no_run
//! use rmux_sdk::{Pane, PaneSurfaceState};
//! use sdk_migration_consumer::{AppResult, streams};
//! use std::time::Duration;
//!
//! async fn view(pane: &Pane) -> AppResult<()> {
//!     let mut stream = streams::open_surface(pane).await?;
//!     let mut state = PaneSurfaceState::default();
//!     let outcome = streams::observe_surface(
//!         &mut stream, &mut state, tokio::time::sleep(Duration::from_secs(1)),
//!         |frame| println!("revision={} title={:?}", frame.revision, frame.snapshot.title),
//!     ).await;
//!     // Inspect outcome before deciding to retry a projection error after a
//!     // pane-state change. This example chooses to finish observation.
//!     drop(stream);
//!     println!("{:?}", outcome?);
//!     Ok(())
//! }
//! ```

use std::error::Error;
use std::time::Duration;

use rmux_sdk::{
    protocol_diagnostic, AttachSessionSpec, ClientTerminalSpec, EnsureSession, ForegroundSource,
    LayoutName, NewSessionSpec, Pane, PaneId, PaneRef, PaneStateClosedReason, PaneStateEvent,
    PaneStateEventsOptions, ProcessSpec, ProtocolError, RefreshClientSpec, Rmux, RmuxCommand,
    RmuxEndpoint, RmuxError, Session, SessionName, SessionNameError, SplitDirectionSpec, SplitSpec,
    SplitTargetSpec, TargetRef, TerminalSizeSpec, Window,
};

pub mod streams;

/// An application error boundary accepts SDK, layout-parse and reducer errors.
pub type AppResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// Opt into daemon startup explicitly; use builder.connect() for connection only.
pub async fn start_or_join(endpoint: RmuxEndpoint) -> rmux_sdk::Result<Rmux> {
    Rmux::builder()
        .endpoint(endpoint)
        .default_timeout(Duration::from_secs(5))
        .connect_or_start()
        .await
}

/// Validation has its own semantic error; it also supports `?` into SDK errors.
pub fn validated_name(raw: &str) -> Result<SessionName, SessionNameError> {
    SessionName::new(raw)
}

pub async fn create_or_reuse(rmux: &Rmux, raw: &str) -> rmux_sdk::Result<Session> {
    let name = validated_name(raw)?;
    rmux.ensure_session(EnsureSession::named(name).create_or_reuse().detached(true))
        .await
}

/// Keep a stable identity instead of reconstructing a pane from a slot after moves.
pub async fn tracked_pane(rmux: &Rmux, session: &Session, id: PaneId) -> rmux_sdk::Result<Pane> {
    rmux.pane_by_id(session.name().clone(), id).await
}

pub async fn arrange(window: &Window, name: &str) -> AppResult<()> {
    // ParseLayoutNameError is not implicitly convertible into rmux_sdk::RmuxError.
    let layout: LayoutName = name.parse()?;
    window.select_layout(layout).await?;
    Ok(())
}

/// Value construction does not execute these commands or convert them to wire DTOs.
pub fn commands(name: SessionName) -> Vec<RmuxCommand> {
    let size = TerminalSizeSpec::new(100, 30);
    let slot = PaneRef::new(name.clone(), 0, 1);
    vec![
        RmuxCommand::new(NewSessionSpec {
            session_name: Some(name),
            size: Some(size),
            process: ProcessSpec::argv(["tool", "argument with spaces"]),
            ..Default::default()
        }),
        RmuxCommand::new(AttachSessionSpec {
            client_terminal: ClientTerminalSpec {
                terminal_features: vec!["RGB".into()],
                utf8: true,
            },
            ..Default::default()
        }),
        RmuxCommand::new(SplitSpec {
            target: SplitTargetSpec::Pane(slot),
            direction: SplitDirectionSpec::Horizontal,
            before: false,
            process: ProcessSpec::shell("printf 'ready\\n'"),
        }),
        RmuxCommand::new(RefreshClientSpec::default()),
    ]
}

pub fn selector_label(target: &TargetRef) -> String {
    target.to_string() // Replaces logging target.to_proto().
}

/// The supported CLI escape hatch returns process status separately from SDK errors.
pub async fn display_message(rmux: &Rmux) -> rmux_sdk::Result<rmux_sdk::CommandRun> {
    rmux.cmd(["display-message", "-p", "literal text"]).await
}

/// Diagnostics consume SDK errors. Matching the private protocol enum is unnecessary.
pub fn describe(error: &RmuxError) -> String {
    let diagnostic = protocol_diagnostic(error);
    let detail = match error {
        RmuxError::Protocol { source, .. } => {
            if let Some(name) = source.session_not_found() {
                format!("missing session {name}")
            } else if let Some((value, reason)) = source.invalid_target() {
                format!("invalid target {value}: {reason}")
            } else {
                source.to_string()
            }
        }
        _ => diagnostic.message().to_owned(),
    };
    match rmux_sdk::unsupported_feature_id(error) {
        Some(feature) => format!("{feature}: {detail}; hint={:?}", diagnostic.hint()),
        None => detail,
    }
}

pub fn application_diagnostic(message: &str) -> RmuxError {
    RmuxError::protocol(ProtocolError::new(message))
}

/// Pane-state closure is distinct from recovery/surface logical termination.
pub async fn inspect_pane_state(pane: &Pane) -> rmux_sdk::Result<()> {
    let mut events = pane
        .state_events(PaneStateEventsOptions {
            include_foreground: true,
            ..Default::default()
        })
        .await?;
    while let Some(event) = events.next().await? {
        match event {
            PaneStateEvent::Snapshot { foreground, .. } => {
                let source: Option<ForegroundSource> =
                    foreground.and_then(|state| state.sources.cwd);
                println!("cwd source={source:?}");
            }
            PaneStateEvent::Closed { reason, .. } => {
                let reason: PaneStateClosedReason = reason;
                println!("metadata stream closed: {reason:?}");
                break;
            }
            _ => {}
        }
    }
    drop(events);
    Ok(())
}

#[cfg(feature = "web")]
pub mod web {
    use rmux_sdk::{
        Pane, Session, WebShareHandle, WebSharePaneTarget, WebShareScope, WebShareSummary,
        WebTerminalPalette, WebTerminalTheme,
    };

    fn palette() -> WebTerminalPalette {
        WebTerminalPalette {
            foreground: "#ffffff".into(),
            background: "#000000".into(),
            cursor: "#ffffff".into(),
            ansi: std::array::from_fn(|_| "#000000".into()),
        }
    }

    /// Calling this creates shares; fixture tests do not call it.
    pub async fn share(
        session: &Session,
        pane: &Pane,
    ) -> rmux_sdk::Result<(WebShareHandle, WebShareHandle)> {
        let session_share = session
            .share()
            .terminal_theme(WebTerminalTheme::User)
            .terminal_palette(palette())
            .await?;
        let pane_share = pane
            .share()
            .theme(WebTerminalTheme::Dark)
            .terminal_palette(palette())
            .await?;
        Ok((session_share, pane_share))
    }

    pub fn scope_label(summary: &WebShareSummary) -> String {
        match &summary.scope {
            WebShareScope::Session(name) => name.to_string(),
            WebShareScope::Pane(WebSharePaneTarget::Slot(slot)) => slot.to_string(),
            WebShareScope::Pane(WebSharePaneTarget::Id {
                session_name,
                pane_id,
            }) => {
                format!("{session_name}/{pane_id}")
            }
        }
    }

    pub fn share_metadata(handle: &WebShareHandle) -> (bool, Option<&str>, Option<&str>) {
        (
            handle.controls(),
            handle.tunnel_provider(),
            handle.tunnel_public_url(),
        )
    }
}
