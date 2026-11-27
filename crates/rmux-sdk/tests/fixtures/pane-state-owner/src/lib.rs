//! A downstream consumer depending only on the SDK, without protocol imports.

use rmux_sdk::{
    ForegroundSource, ForegroundSources, ForegroundState, Pane, PaneId, PaneStateClosedReason,
    PaneStateEvent, PaneStateEventsOptions,
};

pub fn foreground(source: ForegroundSource) -> ForegroundState {
    ForegroundState {
        pid: Some(42),
        command: Some("shell".into()),
        cwd: None,
        exe: Some("/bin/shell".into()),
        sources: ForegroundSources {
            pid: Some(source),
            command: Some(source),
            cwd: None,
            exe: Some(source),
        },
    }
}

pub fn source_label(source: ForegroundSource) -> &'static str {
    match source {
        ForegroundSource::Process => "process",
        ForegroundSource::RootProcess => "root process",
        ForegroundSource::Osc7 => "OSC 7",
        ForegroundSource::Profile => "profile",
        ForegroundSource::Environment => "environment",
        ForegroundSource::RuntimeName => "runtime name",
        _ => "future source",
    }
}

pub fn closed(pane_id: PaneId, reason: PaneStateClosedReason) -> PaneStateEvent {
    PaneStateEvent::Closed {
        revision: 19,
        pane_id,
        reason,
    }
}

pub fn close_label(event: &PaneStateEvent) -> Option<&'static str> {
    match event {
        PaneStateEvent::Closed { reason, .. } => Some(match reason {
            PaneStateClosedReason::Exited => "exited",
            PaneStateClosedReason::DiedKept => "kept",
            PaneStateClosedReason::Killed => "killed",
            _ => "future reason",
        }),
        _ => None,
    }
}

pub async fn observe(pane: &Pane) -> rmux_sdk::Result<()> {
    let _: Option<ForegroundState> = pane.foreground_state().await?;
    let _: Option<(PaneId, u64, ForegroundState)> = pane.foreground_state_with_revision().await?;
    let mut events = pane
        .state_events(PaneStateEventsOptions {
            include_foreground: true,
            ..PaneStateEventsOptions::default()
        })
        .await?;
    if let Some(PaneStateEvent::ForegroundChanged {
        old_state,
        new_state,
        ..
    }) = events.next().await?
    {
        let _: ForegroundSources = old_state.sources;
        let _: Option<ForegroundSource> = new_state.sources.command;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::type_name;

    #[test]
    fn foreground_source_is_owned_by_sdk() {
        let owner = type_name::<ForegroundSource>();
        assert!(
            owner.starts_with("rmux_sdk::"),
            "foreground source owner: {owner}"
        );
        assert_eq!(
            source_label(foreground(ForegroundSource::Osc7).sources.pid.unwrap()),
            "OSC 7"
        );
    }

    #[test]
    fn close_reason_is_owned_by_sdk() {
        let owner = type_name::<PaneStateClosedReason>();
        assert!(
            owner.starts_with("rmux_sdk::"),
            "close reason owner: {owner}"
        );
        assert_eq!(
            close_label(&closed(PaneId::new(42), PaneStateClosedReason::DiedKept)),
            Some("kept")
        );
    }
}
