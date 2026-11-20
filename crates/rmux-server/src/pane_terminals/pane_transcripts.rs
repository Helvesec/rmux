#[cfg(test)]
use rmux_core::Utf8Config;
use rmux_core::{GridRenderOptions, PaneId, ScreenCaptureRange};
use rmux_proto::{OptionName, PaneTarget, RmuxError, SessionName};

use crate::pane_io::PaneInvalidationReason;
use crate::pane_terminal_lookup::{missing_pane_terminal, pane_id_for_target};
use crate::pane_transcript::SharedPaneTranscript;

use super::HandlerState;

mod availability;
mod capture;
mod screen_access;
mod transcript_policy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneHistorySizeStats {
    pub(crate) limit: usize,
    pub(crate) size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneCaptureRequest {
    pub(crate) range: ScreenCaptureRange,
    pub(crate) options: GridRenderOptions,
    pub(crate) alternate: bool,
    pub(crate) use_mode_screen: bool,
    pub(crate) pending_input: bool,
    pub(crate) quiet: bool,
    pub(crate) escape_pending: bool,
    /// Whether this capture publishes the pane's link identities instead of its
    /// text. Internal to the capture path: the request the user wrote carries
    /// it, and every other caller of this structure leaves it false.
    pub(crate) hyperlinks: bool,
}

impl HandlerState {
    pub(crate) fn apply_control_colour_report(&self, report: crate::control_colours::ColourReport) {
        let pane = PaneId::from(report.pane);
        for panes in self.transcripts.values() {
            if let Some(transcript) = panes.get(&pane) {
                transcript
                    .lock()
                    .apply_control_colour(report, &self.control_colour_clients);
                break;
            }
        }
    }

    pub(crate) fn transcript_handle(
        &self,
        target: &PaneTarget,
    ) -> Result<SharedPaneTranscript, RmuxError> {
        let runtime_session_name =
            self.runtime_session_name_for_window(target.session_name(), target.window_index());
        let pane_id = pane_id_for_target(
            &self.sessions,
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )?;
        self.transcripts
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .cloned()
            .ok_or_else(|| {
                missing_pane_terminal(
                    target.session_name(),
                    target.window_index(),
                    target.pane_index(),
                )
            })
    }

    pub(crate) fn clear_history(
        &mut self,
        target: &PaneTarget,
        reset_hyperlinks: bool,
    ) -> Result<(), RmuxError> {
        let runtime_session_name =
            self.runtime_session_name_for_window(target.session_name(), target.window_index());
        let pane_id = pane_id_for_target(
            &self.sessions,
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )?;
        let transcript = self
            .transcripts
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .cloned()
            .ok_or_else(|| {
                missing_pane_terminal(
                    target.session_name(),
                    target.window_index(),
                    target.pane_index(),
                )
            })?;
        let output = self
            .pane_outputs
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .cloned()
            .ok_or_else(|| {
                missing_pane_terminal(
                    target.session_name(),
                    target.window_index(),
                    target.pane_index(),
                )
            })?;
        output.mutate_transcript(
            &transcript,
            PaneInvalidationReason::ClearHistory,
            |transcript| {
                transcript.clear_history(reset_hyperlinks);
                ((), true)
            },
        );
        Ok(())
    }

    pub(crate) fn reset_pane_terminal_state(&self, target: &PaneTarget) -> Result<(), RmuxError> {
        let transcript = self.transcript_handle(target)?;
        let output = self.pane_output_for_target(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )?;
        output.mutate_transcript(
            &transcript,
            PaneInvalidationReason::TerminalReset,
            |transcript| {
                transcript.reset_terminal_state();
                ((), true)
            },
        );
        Ok(())
    }

    pub(crate) fn trim_pane_below_cursor(&self, target: &PaneTarget) -> Result<(), RmuxError> {
        let transcript = self.transcript_handle(target)?;
        let output = self.pane_output_for_target(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )?;
        output.mutate_transcript(
            &transcript,
            PaneInvalidationReason::TranscriptMutation,
            |transcript| {
                let changed = transcript.trim_below_cursor();
                ((), changed)
            },
        );
        Ok(())
    }

    pub(crate) fn set_pane_title(
        &mut self,
        target: &PaneTarget,
        title: &str,
    ) -> Result<Option<(String, String)>, RmuxError> {
        let transcript = self.transcript_handle(target)?;
        let output = self.pane_output_for_target(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )?;
        Ok(output.mutate_transcript(
            &transcript,
            PaneInvalidationReason::TranscriptMutation,
            |transcript| {
                let changed = transcript.set_title(title);
                let did_change = changed.is_some();
                (changed, did_change)
            },
        ))
    }

    pub(crate) fn pane_history_size_stats(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<PaneHistorySizeStats> {
        let transcript = self.pane_transcript_for_id(session_name, pane_id)?;
        let transcript = transcript.lock();

        Some(PaneHistorySizeStats {
            limit: transcript.history_limit(),
            size: transcript.history_size(),
        })
    }

    #[cfg(test)]
    pub(crate) fn colour_query_replies_for_test(
        &self,
        session: &SessionName,
        pane: PaneId,
    ) -> Vec<u8> {
        self.pane_transcript_for_id(session, pane)
            .expect("test pane transcript")
            .lock()
            .append_bytes_and_take_replies(b"\x1b]10;?\x07\x1b]11;?\x1b\\\x1b]12;?\x07")
            .replies
    }

    #[cfg(test)]
    pub(crate) fn transcript_utf8_config(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> Result<Utf8Config, RmuxError> {
        let pane_id = pane_id_for_target(&self.sessions, session_name, window_index, pane_index)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        self.transcripts
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .ok_or_else(|| missing_pane_terminal(session_name, window_index, pane_index))
            .map(|transcript| transcript.lock().utf8_config().clone())
    }

    #[cfg(test)]
    pub(crate) fn append_bytes_to_pane_transcript_for_test(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
        bytes: &[u8],
    ) -> Result<(), RmuxError> {
        let pane_id = pane_id_for_target(&self.sessions, session_name, window_index, pane_index)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let transcript = self
            .transcripts
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .cloned()
            .ok_or_else(|| missing_pane_terminal(session_name, window_index, pane_index))?;
        if let Some(output) = self
            .pane_outputs
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .cloned()
        {
            output.mutate_transcript(
                &transcript,
                PaneInvalidationReason::TranscriptMutation,
                |transcript| {
                    transcript.append_bytes(bytes);
                    ((), !bytes.is_empty())
                },
            );
        } else {
            transcript.lock().append_bytes(bytes);
        }
        Ok(())
    }

    pub(crate) fn all_pane_targets(&self) -> Vec<PaneTarget> {
        self.sessions
            .iter()
            .map(|session| (session.name(), session))
            .flat_map(|(session_name, session)| {
                session.windows().flat_map(move |(window_index, window)| {
                    window.panes().iter().map(move |pane| {
                        PaneTarget::with_window(session_name.clone(), window_index, pane.index())
                    })
                })
            })
            .collect()
    }

    pub(in crate::pane_terminals) fn session_pane_targets(
        &self,
        session_name: &SessionName,
    ) -> Vec<PaneTarget> {
        let Some(session) = self.sessions.session(session_name) else {
            return Vec::new();
        };
        session
            .windows()
            .flat_map(|(window_index, window)| {
                window.panes().iter().map(move |pane| {
                    PaneTarget::with_window(session_name.clone(), window_index, pane.index())
                })
            })
            .collect()
    }

    pub(in crate::pane_terminals) fn window_pane_targets(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Vec<PaneTarget> {
        let Some(window) = self
            .sessions
            .session(session_name)
            .and_then(|session| session.window_at(window_index))
        else {
            return Vec::new();
        };
        window
            .panes()
            .iter()
            .map(|pane| PaneTarget::with_window(session_name.clone(), window_index, pane.index()))
            .collect()
    }

    pub(in crate::pane_terminals) fn input_buffer_limit(&self) -> usize {
        self.options
            .resolve(None, OptionName::InputBufferSize)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1_048_576)
    }

    pub(in crate::pane_terminals) fn resize_transcripts(
        &mut self,
        session_name: &SessionName,
        pane_geometries: &[crate::pane_terminal_lookup::SessionPane],
    ) {
        let Some(transcripts) = self.transcripts.get(session_name) else {
            return;
        };
        let outputs = self.pane_outputs.get(session_name);

        let mut resized_panes = Vec::new();
        for pane in pane_geometries {
            let Some(transcript) = transcripts.get(&pane.id).cloned() else {
                continue;
            };
            let size = rmux_proto::TerminalSize {
                cols: pane.geometry.cols(),
                rows: pane.geometry.rows(),
            };
            // Border drags routinely enqueue repeated copies of the same
            // geometry. Reject those before taking PaneOutputState, then
            // recheck under the atomic output -> transcript critical section
            // for the actual mutation.
            let needs_resize = transcript.needs_resize(size);
            if !needs_resize {
                continue;
            }
            let changed =
                if let Some(output) = outputs.and_then(|outputs| outputs.get(&pane.id)).cloned() {
                    output.mutate_transcript(
                        &transcript,
                        PaneInvalidationReason::Resize,
                        |transcript| {
                            let changed = transcript.screen().size() != size;
                            if changed {
                                transcript.resize(size);
                            }
                            (changed, changed)
                        },
                    )
                } else {
                    let mut transcript = transcript.lock();
                    let changed = transcript.screen().size() != size;
                    if changed {
                        transcript.resize(size);
                    }
                    changed
                };
            if changed {
                resized_panes.push(pane.id);
            }
        }
        for pane_id in resized_panes {
            self.clear_attached_submitted_line(session_name, pane_id);
        }
    }
}

fn escape_pending_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    for &byte in bytes {
        if byte >= b' ' && byte != b'\\' {
            output.push(byte);
        } else {
            output.push(b'\\');
            output.push(b'0' + ((byte >> 6) & 0x7));
            output.push(b'0' + ((byte >> 3) & 0x7));
            output.push(b'0' + (byte & 0x7));
        }
    }
    output
}

#[cfg(test)]
mod history_limit_zero_tests;

#[cfg(test)]
mod transcript_policy_tests;
