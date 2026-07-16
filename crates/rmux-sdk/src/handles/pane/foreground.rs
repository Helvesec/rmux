use crate::handles::session::unexpected_response;
use crate::{Pane, PaneId, Result};
use rmux_proto::{
    ForegroundFieldSource, ForegroundSourcesDto, ForegroundStateDto, PaneForegroundStateRequest,
    Request, Response, CAPABILITY_SDK_PANE_FOREGROUND,
};

/// Source labels for best-effort foreground fields.
pub use rmux_proto::ForegroundFieldSource as ForegroundSource;

/// Per-field source report for best-effort foreground state.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ForegroundSources {
    /// Source for `pid`.
    pub pid: Option<ForegroundFieldSource>,
    /// Source for `command`.
    pub command: Option<ForegroundFieldSource>,
    /// Source for `cwd`.
    pub cwd: Option<ForegroundFieldSource>,
    /// Source for `exe`.
    pub exe: Option<ForegroundFieldSource>,
}

/// Best-effort pane foreground process state.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ForegroundState {
    /// Foreground or root process id, when knowable.
    pub pid: Option<u32>,
    /// Executable command name, when knowable.
    pub command: Option<String>,
    /// Current working directory, when knowable.
    pub cwd: Option<String>,
    /// Executable path, when knowable.
    pub exe: Option<String>,
    /// Per-field source labels.
    pub sources: ForegroundSources,
}

pub(super) async fn foreground_state(
    pane: &Pane,
) -> Result<Option<(PaneId, u64, ForegroundState)>> {
    crate::capabilities::require(pane.transport(), &[CAPABILITY_SDK_PANE_FOREGROUND]).await?;
    let response = pane
        .transport()
        .request(Request::PaneForegroundState(PaneForegroundStateRequest {
            target: pane.proto_target_ref(),
        }))
        .await?;

    match response {
        Response::PaneForegroundState(response) => {
            let response = *response;
            Ok(response.state.map(|state| {
                (
                    response.pane_id,
                    response.revision,
                    ForegroundState::from(state),
                )
            }))
        }
        response => Err(unexpected_response("pane-foreground-state", response)),
    }
}

impl From<ForegroundStateDto> for ForegroundState {
    fn from(value: ForegroundStateDto) -> Self {
        Self {
            pid: value.pid,
            command: value.command,
            cwd: value.cwd,
            exe: value.exe,
            sources: ForegroundSources::from(value.sources),
        }
    }
}

impl From<ForegroundSourcesDto> for ForegroundSources {
    fn from(value: ForegroundSourcesDto) -> Self {
        Self {
            pid: value.pid,
            command: value.command,
            cwd: value.cwd,
            exe: value.exe,
        }
    }
}
