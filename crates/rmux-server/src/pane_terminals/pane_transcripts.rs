use rmux_core::{input::mode, GridRenderOptions, PaneId, ScreenCaptureRange, Utf8Config};
use rmux_proto::{OptionName, PaneTarget, RmuxError, ScopeSelector, SessionName};

use crate::pane_screen_state::PaneScreenState;
use crate::pane_terminal_lookup::{missing_pane_terminal, pane_id_for_target};
use crate::pane_transcript::SharedPaneTranscript;

use super::HandlerState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneHistoryStats {
    pub(crate) limit: usize,
    pub(crate) size: usize,
    pub(crate) bytes: usize,
    pub(crate) all_bytes: String,
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
}

impl HandlerState {
    pub(crate) fn capture_transcript(
        &self,
        target: &PaneTarget,
        request: PaneCaptureRequest,
    ) -> Result<Vec<u8>, RmuxError> {
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
            .ok_or_else(|| {
                missing_pane_terminal(
                    target.session_name(),
                    target.window_index(),
                    target.pane_index(),
                )
            })?;
        let transcript = transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned");

        if request.pending_input {
            let pending = transcript.pending_bytes();
            return Ok(if request.escape_pending {
                escape_pending_bytes(&pending)
            } else {
                pending
            });
        }
        if request.use_mode_screen {
            if let Some(captured) = transcript.capture_copy_mode(request.range, request.options) {
                return Ok(captured);
            }
        }
        if request.alternate {
            return transcript
                .capture_saved(request.range, request.options)
                .map_or_else(
                    || {
                        if request.quiet {
                            Ok(Vec::new())
                        } else {
                            Err(RmuxError::Server("no alternate screen".to_owned()))
                        }
                    },
                    Ok,
                );
        }

        Ok(transcript.capture_main(request.range, request.options))
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
            .ok_or_else(|| {
                missing_pane_terminal(
                    target.session_name(),
                    target.window_index(),
                    target.pane_index(),
                )
            })?;
        let mut transcript = transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned");
        transcript.clear_history(reset_hyperlinks);
        Ok(())
    }

    pub(crate) fn set_pane_title(
        &mut self,
        target: &PaneTarget,
        title: &str,
    ) -> Result<(), RmuxError> {
        let transcript = self.transcript_handle(target)?;
        let mut transcript = transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned");
        transcript.set_title(title);
        Ok(())
    }

    pub(crate) fn refresh_transcript_limits_for_scope(
        &mut self,
        scope: &ScopeSelector,
        option: OptionName,
    ) {
        match option {
            OptionName::HistoryLimit => match scope {
                ScopeSelector::Global => {
                    for session_name in self.transcripts.keys().cloned().collect::<Vec<_>>() {
                        self.refresh_transcript_limits_for_session(&session_name);
                    }
                }
                ScopeSelector::Session(session_name) => {
                    self.refresh_transcript_limits_for_session(session_name);
                }
                ScopeSelector::Window(_) | ScopeSelector::Pane(_) => {}
            },
            OptionName::CodepointWidths | OptionName::VariationSelectorAlwaysWide => {
                if matches!(scope, ScopeSelector::Global) {
                    self.refresh_transcript_utf8_config();
                }
            }
            _ => {}
        }
    }

    pub(crate) fn pane_history_stats(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<PaneHistoryStats> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let transcript = self.transcripts.get(&runtime_session_name)?.get(&pane_id)?;
        let transcript = transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned");

        Some(PaneHistoryStats {
            limit: transcript.history_limit(),
            size: transcript.history_size(),
            bytes: transcript.tmux_history_bytes(),
            all_bytes: transcript.tmux_history_all_bytes(),
        })
    }

    pub(crate) fn pane_output_sequence(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<u64> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let transcript = self.transcripts.get(&runtime_session_name)?.get(&pane_id)?;
        Some(
            transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned")
                .output_sequence(),
        )
    }

    pub(crate) fn pane_screen_state(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<PaneScreenState> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let transcript = self.transcripts.get(&runtime_session_name)?.get(&pane_id)?;
        let transcript = transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned");
        let mut mode_bits = transcript.mode();
        if (mode_bits & mode::MODE_KEYS_EXTENDED_2) != 0 {
            mode_bits |= mode::MODE_KEYS_EXTENDED;
        }

        Some(PaneScreenState {
            mode: mode_bits,
            alternate_on: transcript.is_alternate(),
            title: transcript.title().to_owned(),
            path: transcript.path().to_owned(),
            cursor_style: transcript.cursor_style(),
        })
    }

    pub(crate) fn pane_copy_mode_summary(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<crate::copy_mode::CopyModeSummary> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let transcript = self.transcripts.get(&runtime_session_name)?.get(&pane_id)?;
        transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned")
            .copy_mode_summary()
    }

    pub(crate) fn pane_copy_mode_render_screen(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<rmux_core::Screen> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let transcript = self.transcripts.get(&runtime_session_name)?.get(&pane_id)?;
        transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned")
            .copy_mode_render_screen()
    }

    pub(crate) fn pane_render_screen(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<rmux_core::Screen> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let transcript = self.transcripts.get(&runtime_session_name)?.get(&pane_id)?;
        Some(
            transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned")
                .clone_screen(),
        )
    }

    pub(crate) fn pane_in_mode(&self, session_name: &SessionName, pane_id: PaneId) -> bool {
        let Some(window_index) = self
            .sessions
            .session(session_name)
            .and_then(|session| session.window_index_for_pane_id(pane_id))
        else {
            return false;
        };
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        self.transcripts
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .is_some_and(|transcript| {
                transcript
                    .lock()
                    .expect("pane transcript mutex must not be poisoned")
                    .pane_in_mode()
            })
    }

    pub(crate) fn pane_mode_name(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<&'static str> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        self.transcripts
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .and_then(|transcript| {
                transcript
                    .lock()
                    .expect("pane transcript mutex must not be poisoned")
                    .pane_mode_name()
            })
    }

    pub(crate) fn pane_clock_mode_generation(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<u64> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        self.transcripts
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .and_then(|transcript| {
                transcript
                    .lock()
                    .expect("pane transcript mutex must not be poisoned")
                    .clock_mode_generation()
            })
    }

    pub(crate) fn pane_visible_lines(&self, target: &PaneTarget) -> Result<Vec<String>, RmuxError> {
        let transcript = self.transcript_handle(target)?;
        let transcript = transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned");
        let rows = self
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .and_then(|window| window.pane(target.pane_index()))
            .map(|pane| usize::from(pane.geometry().rows()))
            .ok_or_else(|| {
                missing_pane_terminal(
                    target.session_name(),
                    target.window_index(),
                    target.pane_index(),
                )
            })?;

        let render_options = GridRenderOptions {
            with_sequences: true,
            trim_spaces: false,
            ..GridRenderOptions::default()
        };
        let mut lines = Vec::with_capacity(rows);
        for row in 0..rows {
            let screen = transcript.capture_main(
                ScreenCaptureRange::new(Some(row as i64), Some(row as i64)),
                render_options,
            );
            let mut line = String::from_utf8_lossy(&screen).into_owned();
            if line.ends_with('\n') {
                line.pop();
            }
            lines.push(line);
        }
        Ok(lines)
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
            .map(|transcript| {
                transcript
                    .lock()
                    .expect("pane transcript mutex must not be poisoned")
                    .utf8_config()
                    .clone()
            })
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
            .ok_or_else(|| missing_pane_terminal(session_name, window_index, pane_index))?;
        transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned")
            .append_bytes(bytes);
        Ok(())
    }

    pub(in crate::pane_terminals) fn history_limit_for_session(
        &self,
        session_name: &SessionName,
    ) -> usize {
        self.options
            .resolve(Some(session_name), OptionName::HistoryLimit)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(2000)
    }

    pub(in crate::pane_terminals) fn refresh_transcript_limits_for_session(
        &mut self,
        session_name: &SessionName,
    ) {
        let limit = self.history_limit_for_session(session_name);
        let runtime_session_name = self.runtime_session_name(session_name);
        if let Some(transcripts) = self.transcripts.get_mut(&runtime_session_name) {
            for transcript in transcripts.values() {
                transcript
                    .lock()
                    .expect("pane transcript mutex must not be poisoned")
                    .set_limit(limit);
            }
        }
    }

    fn refresh_transcript_utf8_config(&mut self) {
        let utf8_config = Utf8Config::from_options(&self.options);
        for transcripts in self.transcripts.values_mut() {
            for transcript in transcripts.values() {
                transcript
                    .lock()
                    .expect("pane transcript mutex must not be poisoned")
                    .set_utf8_config(utf8_config.clone());
            }
        }
    }

    pub(in crate::pane_terminals) fn resize_transcripts(
        &mut self,
        session_name: &SessionName,
        pane_geometries: &[crate::pane_terminal_lookup::SessionPane],
    ) {
        let Some(transcripts) = self.transcripts.get_mut(session_name) else {
            return;
        };

        let mut resized_panes = Vec::new();
        for pane in pane_geometries {
            let Some(transcript) = transcripts.get(&pane.id) else {
                continue;
            };
            transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned")
                .resize(rmux_proto::TerminalSize {
                    cols: pane.geometry.cols(),
                    rows: pane.geometry.rows(),
                });
            resized_panes.push(pane.id);
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
