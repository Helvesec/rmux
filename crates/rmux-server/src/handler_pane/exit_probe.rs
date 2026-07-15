use std::time::Instant;

use rmux_proto::{
    decode_internal_pane_exit_probe, CommandOutput, ListPanesRequest, ListPanesResponse, PaneId,
    Response, SessionId,
};

use super::super::RequestHandler;
use crate::pane_terminals::PaneExitMetadata;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PaneExitProbeObservation {
    pane_id: PaneId,
    metadata: Option<PaneExitMetadata>,
    retained: bool,
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_internal_pane_exit_probe(
        &self,
        request: &ListPanesRequest,
    ) -> Option<Response> {
        let (session_id, pane_id) = request
            .format
            .as_deref()
            .and_then(decode_internal_pane_exit_probe)?;

        let observation = self
            .live_pane_exit_probe(session_id, pane_id)
            .await
            .or_else(|| {
                self.retained_exited_pane_output_by_id(pane_id, Instant::now())
                    .map(|retained| PaneExitProbeObservation {
                        pane_id,
                        metadata: Some(retained.metadata()),
                        retained: true,
                    })
            });
        let stdout = observation.map_or_else(Vec::new, render_pane_exit_probe);
        Some(Response::ListPanes(ListPanesResponse {
            output: CommandOutput::from_stdout(stdout),
        }))
    }

    async fn live_pane_exit_probe(
        &self,
        session_id: SessionId,
        pane_id: PaneId,
    ) -> Option<PaneExitProbeObservation> {
        let state = self.state.lock().await;
        let (session_name, session) = state
            .sessions
            .iter()
            .find(|(_name, session)| session.id() == session_id)?;
        session.window_index_for_pane_id(pane_id)?;
        Some(PaneExitProbeObservation {
            pane_id,
            metadata: state.pane_exit_metadata(session_name, pane_id),
            retained: false,
        })
    }
}

fn render_pane_exit_probe(observation: PaneExitProbeObservation) -> Vec<u8> {
    let (dead, status, signal) = match observation.metadata {
        Some(metadata) => (
            1,
            metadata
                .status
                .map_or_else(String::new, |value| value.to_string()),
            metadata
                .signal
                .map_or_else(String::new, |value| value.to_string()),
        ),
        None => (0, String::new(), String::new()),
    };
    let retained = u8::from(observation.retained);
    format!(
        "{}\t{dead}\t{status}\t{signal}\t{retained}\n",
        observation.pane_id
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_probe_has_the_existing_process_state_shape() {
        assert_eq!(
            render_pane_exit_probe(PaneExitProbeObservation {
                pane_id: PaneId::new(9),
                metadata: None,
                retained: false,
            }),
            b"%9\t0\t\t\t0\n"
        );
    }

    #[test]
    fn retained_probe_preserves_process_exit_details() {
        assert_eq!(
            render_pane_exit_probe(PaneExitProbeObservation {
                pane_id: PaneId::new(9),
                metadata: Some(PaneExitMetadata {
                    status: Some(7),
                    signal: None,
                    time: Some(1),
                }),
                retained: true,
            }),
            b"%9\t1\t7\t\t1\n"
        );
    }
}
