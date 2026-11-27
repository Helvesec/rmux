use super::{
    attach_render_session, effective_client_key_table_name, renderer, session_not_found,
    sort_client_loop_snapshots, AttachSessionRenderOptions, AttachTarget, AttachTargetMaster,
    AttachTargetRenderOptions, ClientLoopSnapshot, ClientPathUpdate, ClientTitleUpdate,
    CursorScope, HandlerState, ListClientSnapshot, LivePaneRender, OptionName, OuterTerminal,
    PaneTarget, PresentationCapture,
};

use crate::pane_transcript::{PaneTranscript, PaneTranscriptReach, PaneTranscriptUnavailable};

#[cfg(feature = "web")]
use super::{OuterTerminalContext, Path};

pub(in crate::handler) fn attach_target_for_session(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    options: AttachSessionRenderOptions<'_>,
) -> Result<AttachTarget, rmux_proto::RmuxError> {
    let AttachSessionRenderOptions {
        attached_count,
        terminal_context,
        previous_title,
        client,
        socket_path,
        client_loop_snapshots,
    } = options;
    attach_target_for_session_with_prompt(
        state,
        session_name,
        attached_count,
        AttachTargetRenderOptions {
            prompt: None,
            key_table: None,
            previous_title,
            client,
            terminal_context,
            render_size: None,
            window_index: None,
            selection: None,
            window_size_override: None,
            client_loop_snapshots,
            master: AttachTargetMaster::Clone,
            bounded_recovery: false,
            socket_path,
        },
    )
}

#[cfg(feature = "web")]
pub(in crate::handler) fn attach_target_for_web_session(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    attached_count: usize,
    terminal_context: &OuterTerminalContext,
    socket_path: &Path,
) -> Result<AttachTarget, rmux_proto::RmuxError> {
    attach_target_for_session_with_prompt(
        state,
        session_name,
        attached_count,
        AttachTargetRenderOptions {
            prompt: None,
            key_table: None,
            previous_title: None,
            client: None,
            terminal_context,
            render_size: None,
            window_index: None,
            selection: None,
            window_size_override: None,
            client_loop_snapshots: None,
            master: AttachTargetMaster::Clone,
            bounded_recovery: true,
            socket_path,
        },
    )
}

pub(super) fn attach_target_for_session_with_prompt(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    attached_count: usize,
    options: AttachTargetRenderOptions<'_>,
) -> Result<AttachTarget, rmux_proto::RmuxError> {
    let canonical_session = state
        .sessions
        .session(session_name)
        .ok_or_else(|| session_not_found(session_name))?;
    let session = attach_render_session(
        canonical_session,
        options.render_size,
        options.window_index,
        options.selection,
        options.window_size_override,
    )?;
    let mut session = session;
    if let Some(client) = options.client {
        let viewport = {
            let mut pan = client.pan;
            pan.resolve(state, session.view(), session.view().terminal_size())
        };
        session.set_viewport_offset(viewport.bigger.then_some((viewport.x, viewport.y)));
    }
    let session = session.view();
    let key_table = effective_client_key_table_name(state, session, options.key_table);
    let pane_output_sender = state.pane_output_for_target(
        session_name,
        session.active_window_index(),
        session.active_pane_index(),
    )?;
    // Reserve the live receiver at the same sequence boundary used by the
    // render target. Output emitted before the transport upgrade is then
    // replayable without retaining live-only passthroughs for detached panes.
    let (pane_output_start_sequence, pane_output) = pane_output_sender
        .subscribe_live_from_now_in_sweep()
        .ok_or_else(|| active_pane_unavailable(session))?;
    let active_pane = session.window().active_pane().cloned();
    #[cfg(feature = "web")]
    let pane_state = if options.bounded_recovery {
        session
            .active_pane_id()
            .map(|pane_id| state.pane_recovery_screen_state(session_name, pane_id))
            .transpose()?
            .flatten()
            .map(|(pane_state, _)| pane_state)
    } else {
        session
            .active_pane_id()
            .and_then(|pane_id| state.pane_presentation_screen_state(session_name, pane_id))
    };
    #[cfg(not(feature = "web"))]
    let pane_state = session
        .active_pane_id()
        .and_then(|pane_id| state.pane_presentation_screen_state(session_name, pane_id));
    // A kept-dead pane retains its transcript for capture and rendering, but
    // no live application remains to consume outer mouse reports. Do not let
    // stale DECSET 1000/1002/1003 bits from that transcript keep the client's
    // terminal in mouse-tracking mode after remain-on-exit refreshes it.
    let active_pane_mouse_mode = active_pane.as_ref().map_or(0, |pane| {
        if state.pane_is_dead(session_name, pane.id()) {
            0
        } else {
            pane_state.as_ref().map_or(0, |pane_state| pane_state.mode)
        }
    });
    let outer_terminal = OuterTerminal::resolve_for_session(
        &state.options,
        Some(session_name),
        options.terminal_context.clone(),
    )
    .with_active_pane_mouse_mode(active_pane_mouse_mode);
    let cursor_scope = match options.prompt {
        Some(prompt) if prompt.command_prompt => CursorScope::CommandPrompt,
        Some(_) => CursorScope::Prompt,
        None => CursorScope::Pane,
    };
    let cursor_style = outer_terminal.resolve_cursor_style_state(
        session,
        &state.options,
        pane_state.as_ref(),
        cursor_scope,
    );
    let resolved_client = options.client.map(|client| {
        let mut client = client
            .clone()
            .resolved_for_render(outer_terminal.features_string(), &key_table);
        client.viewport = client.pan.resolve(state, session, session.terminal_size());
        client
    });
    let mut resolved_client_loop_snapshots = options
        .client_loop_snapshots
        .map(<[ClientLoopSnapshot]>::to_vec);
    if let Some(snapshots) = resolved_client_loop_snapshots.as_mut() {
        if let Some(client) = resolved_client.as_ref() {
            if let Some(snapshot) = snapshots
                .iter_mut()
                .find(|snapshot| snapshot.name() == client.name)
            {
                *snapshot = client.client_loop_snapshot();
            }
        }
        sort_client_loop_snapshots(snapshots);
    }
    let client_loop_snapshots = resolved_client_loop_snapshots.as_deref();
    // tmux expands the title through the format tree of the client it is about
    // to write to, so this render's own client supplies every `#{client_*}`.
    // The features come from the terminal just resolved for it rather than from
    // a separate lookup, so the record cannot describe a different terminal
    // than the one the frame is written for.
    let client_bindings = resolved_client
        .as_ref()
        .map(ListClientSnapshot::format_bindings);
    let resolved_title = renderer::expand_client_title(
        session,
        &state.options,
        &renderer::ClientTitleContext {
            state: Some(state),
            attached_count,
            key_table: Some(&key_table),
            socket_path: Some(options.socket_path),
            client: client_bindings.as_ref(),
            client_loop_snapshots,
        },
    );
    let title_update = ClientTitleUpdate {
        resolved: resolved_title.as_deref(),
        // A pane with no screen state was not read at all; one reporting an
        // empty OSC 7 payload is reporting that it has no directory.
        path: pane_state
            .as_ref()
            .map_or(ClientPathUpdate::Unread, |pane_state| {
                ClientPathUpdate::from_pane(pane_state.path.as_str())
            }),
        previous: options.previous_title,
    };
    let client_title = outer_terminal.rendered_client_title(title_update);
    let mut render_frame =
        outer_terminal.render_prelude(session, &state.options, cursor_scope, title_update);
    render_frame.extend_from_slice(
        renderer::render_with_attached_count_prompt_and_pane_title(
            session,
            &state.options,
            attached_count,
            renderer::StatusRenderContext {
                prompt: None,
                client: client_bindings.as_ref(),
                pane_title: pane_state
                    .as_ref()
                    .map(|pane_state| pane_state.title.as_str())
                    .filter(|title| !title.is_empty()),
                state: Some(state),
                key_table: Some(&key_table),
                socket_path: Some(options.socket_path),
                client_loop_snapshots,
            },
        )
        .as_slice(),
    );
    validate_attach_recovery_frame(&render_frame, options.bounded_recovery)?;
    // Retain the active pane's immutable body capture. Its screen builds both
    // the pane bytes and cursor below, then seeds the live renderer's initial
    // diff baseline and delivered-frame provenance without another transcript
    // read in between.
    let mut active_pane_capture = None;
    for pane in session.window().panes() {
        #[cfg(feature = "web")]
        let copy_snapshot = if options.bounded_recovery {
            state.pane_copy_mode_recovery_snapshot(session_name, pane.id())?
        } else {
            state.pane_copy_mode_render_snapshot(session_name, pane.id())
        };
        #[cfg(not(feature = "web"))]
        let copy_snapshot = state.pane_copy_mode_render_snapshot(session_name, pane.id());
        if let Some(snapshot) = copy_snapshot.as_ref() {
            let pane_frame = if options.prompt.is_some() {
                renderer::render_copy_mode_pane_screen_preserving_prompt_cursor(
                    session,
                    &state.options,
                    pane,
                    snapshot,
                )
            } else {
                renderer::render_copy_mode_pane_screen(session, &state.options, pane, snapshot)
            };
            render_frame.extend_from_slice(pane_frame.as_slice());
        } else {
            #[cfg(feature = "web")]
            let capture = if options.bounded_recovery {
                state
                    .pane_recovery_screen(session_name, pane.id())?
                    .map(|(screen, _)| PresentationCapture::converged(screen))
            } else {
                state.pane_presentation_capture(session_name, pane.id())
            };
            #[cfg(not(feature = "web"))]
            let capture = state.pane_presentation_capture(session_name, pane.id());
            let Some(capture) = capture else {
                continue;
            };
            let screen = capture.screen();
            let pane_frame = if options.prompt.is_some() {
                renderer::render_pane_screen_preserving_prompt_cursor(
                    session,
                    &state.options,
                    pane,
                    screen,
                )
            } else {
                renderer::render_pane_screen(session, &state.options, pane, screen)
            };
            render_frame.extend_from_slice(pane_frame.as_slice());
            if pane.index() == session.active_pane_index() {
                active_pane_capture = Some(capture);
            }
        }
        if pane.index() == session.active_pane_index() && copy_snapshot.is_some() {
            if let (Some(summary), Some(stats)) = (
                state.pane_copy_mode_summary(session_name, pane.id()),
                state.pane_history_size_stats(session_name, pane.id()),
            ) {
                render_frame.extend_from_slice(
                    renderer::render_copy_mode_position(
                        session,
                        &state.options,
                        session.active_window_index(),
                        pane,
                        &summary,
                        stats.size,
                        copy_snapshot
                            .as_ref()
                            .is_some_and(|snapshot| snapshot.alternate_on),
                    )
                    .as_slice(),
                );
            }
        }
        validate_attach_recovery_frame(&render_frame, options.bounded_recovery)?;
    }
    render_frame.extend_from_slice(
        renderer::render_pane_border_status_lines(session, &state.options, Some(state)).as_slice(),
    );
    if let Some(prompt) = options.prompt {
        render_frame.extend_from_slice(&renderer::render_prompt(session, &state.options, prompt));
    }
    validate_attach_recovery_frame(&render_frame, options.bounded_recovery)?;
    if options.prompt.is_none() {
        if let Some(active_pane) = active_pane.clone() {
            #[cfg(feature = "web")]
            let copy_snapshot = if options.bounded_recovery {
                state.pane_copy_mode_recovery_snapshot(session_name, active_pane.id())?
            } else {
                state.pane_copy_mode_render_snapshot(session_name, active_pane.id())
            };
            #[cfg(not(feature = "web"))]
            let copy_snapshot =
                state.pane_copy_mode_render_snapshot(session_name, active_pane.id());
            if let Some(snapshot) = copy_snapshot {
                render_frame.extend_from_slice(
                    renderer::render_copy_mode_pane_cursor(
                        session,
                        &state.options,
                        &active_pane,
                        &snapshot,
                    )
                    .as_slice(),
                );
            } else if let Some(capture) = active_pane_capture.as_ref() {
                render_frame.extend_from_slice(
                    renderer::render_pane_cursor(
                        session,
                        &state.options,
                        &active_pane,
                        capture.screen(),
                    )
                    .as_slice(),
                );
            }
        }
    }
    validate_attach_recovery_frame(&render_frame, options.bounded_recovery)?;
    let live_pane = live_pane_render_for_target(
        state,
        session,
        &state.options,
        session_name,
        options.prompt,
        active_pane_capture,
    );

    let active_pane_geometry = active_pane.as_ref().map_or_else(
        || rmux_core::PaneGeometry::new(0, 0, 0, 0),
        |pane| {
            renderer::visible_pane_terminal_geometry(
                session,
                &state.options,
                pane,
                pane_state.as_ref().is_some_and(|state| state.alternate_on),
                rendered_pane_mode(state, session_name, pane, |transcript| {
                    transcript.copy_mode_summary().is_some()
                })
                .unwrap_or(false),
            )
            .unwrap_or_else(|| rmux_core::PaneGeometry::new(0, 0, 0, 0))
        },
    );
    let active_pane_is_starting = {
        #[cfg(windows)]
        {
            state.active_pane_is_starting(session_name)
        }
        #[cfg(not(windows))]
        {
            false
        }
    };
    // Empty owns a live transcript/subscription, but no native terminal to
    // hand over. Use the rendered selection (including switch targets), not
    // the canonical session's previous selection or a failed clone as a proxy.
    let active_pane_is_empty = active_pane
        .as_ref()
        .is_some_and(|pane| state.pane_is_empty(pane.id()));
    let terminal_passthrough_allowed = !active_pane_is_starting
        && active_pane.as_ref().is_some_and(|pane| {
            rendered_pane_mode(state, session_name, pane, |transcript| {
                transcript.pane_in_mode()
            }) == Some(false)
                && pane_passthrough_enabled(session, &state.options, pane)
        });
    let kitty_graphics_passthrough =
        terminal_passthrough_allowed && outer_terminal.supports_kitty_graphics();
    let sixel_passthrough = terminal_passthrough_allowed && outer_terminal.supports_sixel();

    Ok(AttachTarget {
        session_name: session_name.clone(),
        pane_master: match options.master {
            AttachTargetMaster::Clone if !active_pane_is_starting && !active_pane_is_empty => {
                if options.selection.is_some() {
                    Some(state.clone_pane_master(
                        session_name,
                        session.active_window_index(),
                        session.active_pane_index(),
                    )?)
                } else {
                    Some(state.active_pane_master(session_name)?)
                }
            }
            AttachTargetMaster::Clone | AttachTargetMaster::Omit => None,
        },
        pane_output,
        pane_output_start_sequence,
        render_frame,
        outer_terminal,
        client_title,
        cursor_style,
        active_pane_geometry,
        raw_passthrough: terminal_passthrough_allowed,
        kitty_graphics_passthrough,
        sixel_passthrough,
        persistent_overlay_state_id: None,
        live_pane,
    })
}

/// The answer for an active pane whose output state is poisoned: no frame
/// for this client, and the request that refreshed it is still served.
fn active_pane_unavailable(session: rmux_core::ManagedSessionView<'_>) -> rmux_proto::RmuxError {
    let message = session.active_pane_id().map_or_else(
        || "active pane unavailable".to_owned(),
        |pane_id| PaneTranscriptUnavailable::for_pane(pane_id).to_string(),
    );
    rmux_proto::RmuxError::Message(message)
}

/// A mode read for a pane the client shows, which the request refreshing the
/// client never named. A poisoned transcript reads as `None`: no live pane
/// and no passthrough are handed over for it, and its geometry is the one
/// of a pane outside copy mode.
fn rendered_pane_mode<R>(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    pane: &rmux_core::Pane,
    read: impl FnOnce(&PaneTranscript) -> R,
) -> Option<R> {
    state.observe_pane_transcript(session_name, pane.id(), PaneTranscriptReach::Reached, read)
}

fn validate_attach_recovery_frame(
    frame: &[u8],
    bounded_recovery: bool,
) -> Result<(), rmux_proto::RmuxError> {
    #[cfg(feature = "web")]
    if bounded_recovery && frame.len() > crate::web::WEB_RECOVERY_CONTENT_BYTES_MAX {
        return Err(rmux_proto::RmuxError::FrameTooLarge {
            length: frame.len(),
            maximum: crate::web::WEB_RECOVERY_CONTENT_BYTES_MAX,
        });
    }
    #[cfg(not(feature = "web"))]
    let _ = (frame, bounded_recovery);
    Ok(())
}

fn pane_passthrough_enabled(
    session: rmux_core::ManagedSessionView<'_>,
    options: &rmux_core::OptionStore,
    pane: &rmux_core::Pane,
) -> bool {
    matches!(
        options.resolve_for_pane(
            session.name(),
            session.active_window_index(),
            pane.index(),
            OptionName::AllowPassthrough,
        ),
        Some("on" | "all")
    )
}

fn live_pane_render_for_target(
    state: &HandlerState,
    session: rmux_core::ManagedSessionView<'_>,
    options: &rmux_core::OptionStore,
    session_name: &rmux_proto::SessionName,
    prompt: Option<&renderer::RenderedPrompt>,
    initial_capture: Option<PresentationCapture>,
) -> Option<Box<LivePaneRender>> {
    if prompt.is_some() || session.viewport_offset().is_some() {
        return None;
    }
    let pane = session.window().active_pane()?;
    if rendered_pane_mode(state, session_name, pane, |transcript| {
        transcript.pane_in_mode()
    }) != Some(false)
    {
        return None;
    }
    let initial_capture = initial_capture?;
    let target = PaneTarget::with_window(
        session_name.clone(),
        session.active_window_index(),
        pane.index(),
    );
    let transcript = state.transcript_handle(&target).ok()?;
    LivePaneRender::new_from_presentation_capture(
        transcript,
        session,
        options,
        pane,
        initial_capture,
    )
}
