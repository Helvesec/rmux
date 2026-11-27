use super::{escape_pending_bytes, PaneCaptureRequest};
use crate::pane_terminal_lookup::{missing_pane_terminal, pane_id_for_target};
use crate::pane_terminals::HandlerState;
use crate::pane_transcript::PaneTranscript;
use rmux_core::LineFormatCapture;
use rmux_proto::{PaneTarget, RmuxError};

impl HandlerState {
    pub(crate) fn capture_transcript(
        &self,
        target: &PaneTarget,
        request: PaneCaptureRequest,
    ) -> Result<Vec<u8>, RmuxError> {
        self.capture_transcript_with_line_format(target, request, false)
            .map(|(content, _)| content)
    }

    pub(crate) fn capture_transcript_with_line_format(
        &self,
        target: &PaneTarget,
        request: PaneCaptureRequest,
        include_line_format: bool,
    ) -> Result<(Vec<u8>, Option<LineFormatCapture>), RmuxError> {
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
        let transcript = transcript.lock();

        if include_line_format && !request.pending_input && !request.hyperlinks {
            // Keep the rendering policy and physical row boundaries together,
            // from the selected screen through the later prefix/join pass.
            let mode_screen = request
                .reads_mode_screen()
                .then(|| transcript.copy_mode_capture_screen())
                .flatten();
            let screen = mode_screen.unwrap_or_else(|| transcript.screen());
            let captured = if request.alternate {
                screen.capture_saved_transcript_with_line_format(request.range, request.options)
            } else {
                Some(screen.capture_transcript_with_line_format(request.range, request.options))
            };
            return captured.map_or_else(
                || {
                    if request.quiet {
                        Ok((Vec::new(), Some(LineFormatCapture::default())))
                    } else {
                        Err(RmuxError::Server("no alternate screen".to_owned()))
                    }
                },
                |(content, physical)| Ok((content, Some(physical))),
            );
        }

        // Metadata and bytes must describe the same selected screen and range.
        let line_format = include_line_format
            .then(|| capture_line_format(&transcript, request))
            .transpose()?;
        let content = capture_bytes(&transcript, request)?;
        Ok((content, line_format))
    }
}

fn capture_bytes(
    transcript: &PaneTranscript,
    request: PaneCaptureRequest,
) -> Result<Vec<u8>, RmuxError> {
    if request.pending_input {
        let pending = transcript.pending_bytes();
        return Ok(if request.escape_pending {
            escape_pending_bytes(&pending)
        } else {
            pending
        });
    }
    // The screen is chosen here, once, under the same lock, and the links
    // of THAT screen are captured from its own cells and store rather than
    // read back out of what it rendered.
    let join_wrapped = request.options.join_wrapped;
    if request.reads_mode_screen() {
        let captured = if request.hyperlinks {
            transcript.capture_copy_mode_hyperlinks(request.range, join_wrapped)
        } else {
            transcript.capture_copy_mode(request.range, request.options)
        };
        if let Some(captured) = captured {
            return Ok(captured);
        }
    }
    if request.alternate {
        let captured = if request.hyperlinks {
            transcript.capture_saved_hyperlinks(request.range, join_wrapped)
        } else {
            transcript.capture_saved(request.range, request.options)
        };
        return captured.map_or_else(
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

    if request.hyperlinks {
        return Ok(transcript.capture_main_hyperlinks(request.range, join_wrapped));
    }
    Ok(transcript.capture_main(request.range, request.options))
}

fn capture_line_format(
    transcript: &PaneTranscript,
    request: PaneCaptureRequest,
) -> Result<LineFormatCapture, RmuxError> {
    if request.pending_input {
        return Ok(LineFormatCapture::default());
    }
    if request.reads_mode_screen() {
        if let Some(line_format) = transcript.capture_copy_mode_line_format(request.range) {
            return Ok(line_format);
        }
    }
    if request.alternate {
        return transcript
            .capture_saved_line_format(request.range)
            .map_or_else(
                || {
                    if request.quiet {
                        Ok(LineFormatCapture::default())
                    } else {
                        Err(RmuxError::Server("no alternate screen".to_owned()))
                    }
                },
                Ok,
            );
    }

    Ok(transcript.capture_main_line_format(request.range))
}
