use std::collections::HashMap;

use rmux_core::{input::mode, ManagedSessionPresentation, OptionStore, PaneId};
use rmux_proto::{PaneTarget, RmuxError};

use crate::pane_screen_state::PaneScreenState;
use crate::pane_terminals::HandlerState;

mod scrollback;
use crate::pane_transcript::SharedPaneTranscript;
use crate::renderer::{WebPaneProjectionContext, WebSceneProjection};
use crate::web::WebSessionTarget;
use scrollback::PaneScrollbackView;

use super::snapshot::{overlay_pane_lines, session_content_geometry, WebSessionView};
use super::{session_not_found_web, WebSessionWindowSelection};
use super::{WebSessionPaneFrame, WebSessionPaneView, WebSessionSnapshot};

/// Only presentation metadata and transcript identities cross the global lock.
/// Pane recovery limits and rendering remain owned by their existing paths.
pub(super) struct WebSessionScene {
    session: ManagedSessionPresentation<'static>,
    options: OptionStore,
    view: WebSessionView,
    transcripts: HashMap<PaneId, SharedPaneTranscript>,
}

impl WebSessionScene {
    pub(super) fn capture(
        state: &HandlerState,
        target: &WebSessionTarget,
        selected_window: Option<WebSessionWindowSelection>,
    ) -> Result<Self, RmuxError> {
        let session = state
            .sessions
            .session_by_id(target.id())
            .ok_or_else(|| session_not_found_web(target.name()))?;
        let session = web_session_view_session(session, selected_window)?;
        let mut view = WebSessionView::new(session.view().window().size());
        for (index, window) in session.view().windows() {
            view.add_window(
                index,
                window.name(),
                index == session.view().active_window_index(),
            );
        }
        let session = session.into_owned_window();
        let mut transcripts = HashMap::new();
        for pane in session.view().window().panes() {
            let target = PaneTarget::with_window(
                session.view().name().clone(),
                session.view().active_window_index(),
                pane.index(),
            );
            // Only panes selected by the projection need a transcript. Keep
            // the existing render-time error for a visible missing pane; an
            // occluded pane must not make an otherwise valid frame fail.
            if let Ok(transcript) = state.transcript_handle(&target) {
                transcripts.insert(pane.id(), transcript);
            }
        }
        let options = state.options.snapshot_for_selected_window(session.view());
        Ok(Self {
            session,
            options,
            view,
            transcripts,
        })
    }

    fn pane_view(
        transcripts: &HashMap<PaneId, SharedPaneTranscript>,
        pane_id: PaneId,
        top_line: Option<usize>,
    ) -> Result<(PaneScreenState, PaneScrollbackView), RmuxError> {
        let transcript = transcripts
            .get(&pane_id)
            .ok_or_else(|| RmuxError::Server(format!("missing pane transcript: {pane_id}")))?
            .lock();
        let screen = PaneScreenState::from_screen(transcript.screen());
        let scrollback = PaneScrollbackView::capture(&transcript, |history_size, alternate_on| {
            if alternate_on {
                0
            } else {
                top_line.map_or(0, |top| history_size.saturating_sub(top.min(history_size)))
            }
        })?;
        Ok((screen, scrollback))
    }

    pub(super) fn render(
        self,
        mut frame: Vec<u8>,
        scrolls: &HashMap<PaneId, usize>,
    ) -> Result<WebSessionSnapshot, RmuxError> {
        let session = self.session.view();
        let window = session.window();
        let presentation = WebSceneProjection::resolve(session, &self.options);
        let has_floating_panes = presentation.has_floating_panes();
        let mut view = self.view;
        if has_floating_panes {
            view.scene = Some(presentation.snapshot(session)?);
        }
        let active_pane = window.active_pane_index();
        let panes = if has_floating_panes {
            presentation
                .pane_ids_rear_to_front()
                .into_iter()
                .filter_map(|pane_id| window.panes().iter().find(|pane| pane.id() == pane_id))
                .collect::<Vec<_>>()
        } else if window.is_zoomed() {
            window.active_pane().into_iter().collect::<Vec<_>>()
        } else {
            window.panes().iter().collect::<Vec<_>>()
        };

        // Default to post-reset values so render_dec_modes emits nothing if the
        // active pane's screen is unavailable.
        let mut active_mode_bits = mode::MODE_CURSOR | mode::MODE_WRAP;
        let mut active_cursor_style = 0u32;
        let mut active_cursor_default_style = 0u32;

        for pane in panes {
            let (screen, scrollback) = Self::pane_view(
                &self.transcripts,
                pane.id(),
                scrolls.get(&pane.id()).copied(),
            )?;
            let mode_bits = Some({
                if pane.index() == active_pane {
                    active_mode_bits = screen.mode;
                    active_cursor_style = screen.cursor_style;
                    active_cursor_default_style =
                        rmux_core::cursor_style_from_option(self.options.resolve_for_pane(
                            session.name(),
                            session.active_window_index(),
                            pane.index(),
                            rmux_proto::OptionName::CursorStyle,
                        ));
                }
                screen.mode
            });
            let projection_context = WebPaneProjectionContext::new(
                scrollback.history_size,
                scrollback.alternate_on,
                scrollback.scroll_offset,
            );
            let geometry = if has_floating_panes {
                presentation.pane_geometry(session, pane, &self.options, projection_context)
            } else {
                session_content_geometry(pane.geometry(), window.size())
            };
            let Some(geometry) = geometry else {
                continue;
            };
            if scrollback.scroll_offset > 0 {
                if has_floating_panes {
                    presentation.overlay_scrollback(
                        &mut frame,
                        session,
                        pane,
                        &scrollback.ansi_lines,
                        &self.options,
                        projection_context,
                        window.size().cols,
                        crate::web::WEB_RECOVERY_CONTENT_BYTES_MAX,
                    )?;
                } else {
                    overlay_pane_lines(&mut frame, geometry, &scrollback.ansi_lines)?;
                }
            }
            if !scrollback.metadata_complete {
                view.mark_metadata_incomplete();
            }
            view.push_pane(WebSessionPaneView::new(
                pane.id(),
                geometry,
                pane.index() == active_pane,
                scrollback.history_size,
                scrollback.scroll_offset,
                scrollback.alternate_on,
                mode_bits.is_some_and(|bits| bits & mode::ALL_MOUSE_MODES != 0),
            ));
        }

        WebSessionSnapshot::new_with_projection(
            window.size(),
            frame,
            view,
            active_mode_bits,
            active_cursor_style,
            active_cursor_default_style,
            presentation,
        )
    }

    pub(super) fn pane_scroll_frame(
        self,
        pane_id: PaneId,
        top_line: usize,
    ) -> Result<Option<WebSessionPaneFrame>, RmuxError> {
        let session = self.session.view();
        let window = session.window();
        let presentation = WebSceneProjection::resolve(session, &self.options);
        if presentation.has_floating_panes() {
            // A pane-local rectangle cannot prove equivalence once scene
            // clipping or occlusion participates. The caller will send the
            // existing complete session keyframe instead.
            return Ok(None);
        }
        let active_pane = window.active_pane_index();
        let panes = if window.is_zoomed() {
            window.active_pane().into_iter().collect::<Vec<_>>()
        } else {
            window.panes().iter().collect::<Vec<_>>()
        };
        let Some(pane) = panes.into_iter().find(|pane| pane.id() == pane_id) else {
            return Ok(None);
        };
        let Some(geometry) = session_content_geometry(pane.geometry(), window.size()) else {
            return Ok(None);
        };
        let (screen, scrollback) = Self::pane_view(&self.transcripts, pane.id(), Some(top_line))?;
        if scrollback.scroll_offset == 0 {
            return Ok(None);
        }
        let mouse_on = screen.mode & mode::ALL_MOUSE_MODES != 0;
        if !scrollback.metadata_complete {
            // Over-budget recovery metadata is a fidelity degrade, not a
            // protocol error. The pane-frame opcode carries no completeness
            // flag, so defer to the full snapshot, which renders the same
            // scrolled rows and reports the gap through the session view's
            // `metadata_complete`. Failing here would tear the viewer socket
            // down with no close code.
            return Ok(None);
        }
        let mut frame = Vec::new();
        overlay_pane_lines(&mut frame, geometry, &scrollback.ansi_lines)?;
        let pane = WebSessionPaneView::new(
            pane.id(),
            geometry,
            pane.index() == active_pane,
            scrollback.history_size,
            scrollback.scroll_offset,
            scrollback.alternate_on,
            mouse_on,
        );
        Ok(Some(WebSessionPaneFrame::new_with_projection(
            window.size(),
            pane,
            frame,
            presentation,
        )?))
    }
}

pub(super) fn web_session_view_session(
    session: rmux_core::ManagedSessionView<'_>,
    selected_window: Option<WebSessionWindowSelection>,
) -> Result<rmux_core::ManagedSessionPresentation<'_>, RmuxError> {
    let mut selected = rmux_core::ManagedSessionPresentation::new(session);
    if let Some(index) = selected_web_session_window_index(session, selected_window)?
        .filter(|index| *index != session.active_window_index())
    {
        selected
            .select_window(index)
            .expect("validated render window");
    }
    Ok(selected)
}

pub(super) fn selected_web_session_window_index(
    session: rmux_core::ManagedSessionView<'_>,
    selected_window: Option<WebSessionWindowSelection>,
) -> Result<Option<u32>, RmuxError> {
    let Some(selection) = selected_window else {
        return Ok(None);
    };
    let window_index = selection.window_index();
    let Some(window) = session.window_at(window_index) else {
        return Err(RmuxError::Server(format!(
            "selected Web window {window_index} disappeared"
        )));
    };
    if window.id() != selection.window_id() {
        return Err(RmuxError::Server(format!(
            "selected Web window {window_index} changed identity"
        )));
    }
    Ok(Some(window_index))
}
