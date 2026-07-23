use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use rmux_proto::{OptionName, RmuxError, Target};

use super::super::scripting_support::{
    format_context_for_target, QueueCommandAction, QueueExecutionContext,
};
use super::super::{RequestHandler, RequesterOrigin};
use super::identity::OverlayIdentity;
use super::layout::{
    menu_styles_for_target, menu_width, overlay_position_context, popup_content_size,
    popup_styles_for_target, resolve_popup_size,
};
use super::menu::{
    MenuOverlayItem, MenuOverlayState, OverlayMenuAction, MENU_NOMOUSE, MENU_STAYOPEN,
};
use super::parse::{parse_menu_shortcut, ParsedDisplayMenuCommand, ParsedDisplayPopupCommand};
use super::popup_job::{spawn_popup_job, PopupDragMode, PopupSurface};
use super::state::{ClientOverlayState, PopupOverlayState};
use super::support::popup_shell_command;
use crate::copy_mode::{CopyModeCommandContext, CopyModeLineNumberLayout, CopyModeState, ModeKeys};
use crate::format_runtime::render_runtime_template;
use crate::format_runtime::RuntimeFormatContext;
use crate::handler_support::attached_client_required;
use crate::mouse::{
    copy_mode_mouse_context_with_line_numbers, layout_for_session, AttachedMouseEvent,
};
use crate::pane_terminals::HandlerState;
use crate::renderer::resolve_overlay_rect;
use crate::terminal::TerminalProfile;

impl RequestHandler {
    pub(super) async fn execute_queued_display_menu(
        &self,
        requester_pid: u32,
        command: ParsedDisplayMenuCommand,
        context: &QueueExecutionContext,
    ) -> Result<QueueCommandAction, RmuxError> {
        let origin = self.capture_requester_origin(requester_pid).await;
        let attach_identity = self
            .resolve_overlay_client_identity(
                requester_pid,
                command.target_client.as_deref(),
                "display-menu",
            )
            .await?;
        let attach_pid = attach_identity.attach_pid();
        if self.mode_tree_active_for_identity(attach_identity).await {
            return Ok(normal_action());
        }

        let target = self
            .resolve_overlay_target_for_identity(
                attach_identity,
                command.target_pane.clone(),
                context.current_target().cloned(),
            )
            .await?;
        let overlay_identity = {
            let mut state = self.state.lock().await;
            OverlayIdentity::capture(&mut state, attach_identity, target.clone())?
        };
        let built = self
            .build_display_menu_state(
                attach_pid,
                origin,
                command,
                target,
                context.clone(),
                overlay_identity,
            )
            .await?;

        let attached_session_name = {
            let state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .filter(|active| {
                    built
                        .identity
                        .matches(&state, active, &built.current_target)
                })
                .ok_or_else(|| attached_client_required("display-menu"))?;
            active.overlay_state_id = active.overlay_state_id.saturating_add(1);
            let overlay_id = active.overlay_state_id;
            let mut built = built;
            built.id = overlay_id;
            match active.overlay.as_mut() {
                Some(ClientOverlayState::Popup(popup)) => {
                    if popup.nested_menu.is_some() {
                        return Ok(normal_action());
                    }
                    popup.nested_menu = Some(built);
                }
                Some(ClientOverlayState::Menu(_)) => {
                    return Ok(normal_action());
                }
                None => {
                    active.overlay = Some(ClientOverlayState::Menu(Box::new(built)));
                }
            }
            active.session_name.clone()
        };

        self.refresh_interactive_overlay_for_session_identity(
            attach_identity,
            &attached_session_name,
            attach_identity.session_id(),
        )
        .await?;
        Ok(normal_action())
    }

    pub(super) async fn execute_queued_display_popup(
        &self,
        requester_pid: u32,
        command: ParsedDisplayPopupCommand,
        context: &QueueExecutionContext,
    ) -> Result<QueueCommandAction, RmuxError> {
        if command.close_existing {
            if let Ok(identity) = self
                .resolve_overlay_client_identity(
                    requester_pid,
                    command.target_client.as_deref(),
                    "display-popup",
                )
                .await
            {
                let _ = self
                    .clear_interactive_overlay_for_identity(identity, true)
                    .await;
            }
            return Ok(normal_action());
        }

        let attach_identity = match self
            .resolve_overlay_client_identity(
                requester_pid,
                command.target_client.as_deref(),
                "display-popup",
            )
            .await
        {
            Ok(identity) => identity,
            Err(error) => return Err(error),
        };
        let attach_pid = attach_identity.attach_pid();
        if self.mode_tree_active_for_identity(attach_identity).await {
            return Ok(normal_action());
        }

        let target = self
            .resolve_overlay_target_for_identity(
                attach_identity,
                command.target_pane.clone(),
                context.current_target().cloned(),
            )
            .await?;
        let overlay_identity = {
            let mut state = self.state.lock().await;
            OverlayIdentity::capture(&mut state, attach_identity, target.clone())?
        };

        let existing_overlay_is_menu = {
            let active_attach = self.active_attach.lock().await;
            matches!(
                active_attach
                    .by_pid
                    .get(&attach_pid)
                    .filter(|active| attach_identity.matches_active(active))
                    .and_then(|active| active.overlay.as_ref()),
                Some(ClientOverlayState::Menu(_))
            )
        };
        if existing_overlay_is_menu {
            return Ok(normal_action());
        }

        let mut popup = self
            .build_display_popup_state(attach_pid, command, target, overlay_identity)
            .await?;

        let installation = {
            let state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            if let Some(active) = active_attach.by_pid.get_mut(&attach_pid).filter(|active| {
                popup
                    .identity
                    .matches(&state, active, &popup.current_target)
            }) {
                active.overlay_state_id = active.overlay_state_id.saturating_add(1);
                popup.id = active.overlay_state_id;
                let replaced_popup_job = match active
                    .overlay
                    .replace(ClientOverlayState::Popup(Box::new(popup.clone())))
                {
                    Some(ClientOverlayState::Popup(replaced)) => replaced.job,
                    Some(ClientOverlayState::Menu(_)) | None => None,
                };
                Some((
                    active.identity(attach_pid),
                    active.session_name.clone(),
                    replaced_popup_job,
                ))
            } else {
                None
            }
        };
        let Some((popup_identity, attached_session_name, replaced_popup_job)) = installation else {
            if let Some(job) = popup.job {
                job.terminate();
            }
            return Err(attached_client_required("display-popup"));
        };

        // Termination may touch a PTY or schedule ConPTY teardown, so keep it
        // outside the attach-state mutex. The old popup id can no longer match
        // the replacement, which makes its waiter and reader callbacks stale.
        if let Some(job) = replaced_popup_job {
            job.terminate();
        }

        if let Some(job) = popup.job.clone() {
            self.spawn_popup_waiter(popup_identity, popup.id, job.clone());
            if let Err(error) = self.spawn_popup_reader(
                popup_identity,
                popup.id,
                popup.surface.clone(),
                job.clone(),
            ) {
                job.terminate();
                return Err(error);
            }
        }

        self.refresh_interactive_overlay_for_session_identity(
            popup_identity,
            &attached_session_name,
            popup_identity.session_id(),
        )
        .await?;
        Ok(normal_action())
    }

    async fn build_display_menu_state(
        &self,
        attach_pid: u32,
        origin: RequesterOrigin,
        command: ParsedDisplayMenuCommand,
        target: Target,
        command_context: QueueExecutionContext,
        identity: OverlayIdentity,
    ) -> Result<MenuOverlayState, RmuxError> {
        let attached_count = self.attached_count(target.session_name()).await;
        let (client_size, mouse, session_name) = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .ok_or_else(|| attached_client_required("display-menu"))?;
            (
                active.client_size,
                active.mouse.current_event.clone(),
                active.session_name.clone(),
            )
        };
        let state = self.state.lock().await;
        let runtime = format_context_for_target(&state, &target, attached_count)?;
        let runtime = runtime_with_mouse_values(runtime, &state, &target, mouse.as_ref());
        let title = render_runtime_template(&command.title, &runtime, true);
        let options = menu_styles_for_target(&state, &target, &command, &runtime);

        let items = command
            .items
            .into_iter()
            .map(|item| {
                let rendered_label = render_runtime_template(&item.label, &runtime, true);
                let dynamically_disabled =
                    rendered_label.starts_with('-') && rendered_label != item.label;
                let separator = item.separator || rendered_label.is_empty() || dynamically_disabled;
                if separator {
                    return MenuOverlayItem {
                        label: String::new(),
                        shortcut_label: None,
                        shortcut: None,
                        separator: true,
                        action: None,
                    };
                }
                let rendered_command = render_runtime_template(&item.command, &runtime, false);
                MenuOverlayItem {
                    label: rendered_label,
                    shortcut_label: (!item.shortcut.is_empty()).then_some(item.shortcut.clone()),
                    shortcut: parse_menu_shortcut(&item.shortcut),
                    separator: false,
                    action: Some(OverlayMenuAction::Command(rendered_command)),
                }
            })
            .collect::<Vec<_>>();

        let width = menu_width(&title, &items).saturating_add(4).max(4);
        let height = u16::try_from(items.len())
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .max(2);
        let position_context =
            overlay_position_context(&state, &session_name, &target, client_size, mouse.as_ref());
        let rect = resolve_overlay_rect(
            runtime,
            position_context,
            command.x.as_deref(),
            command.y.as_deref(),
            width.min(client_size.cols.max(1)),
            height.min(client_size.rows.max(1)),
        )
        .ok_or_else(|| {
            RmuxError::Server("display-menu does not fit in attached client".to_owned())
        })?;

        let no_mouse = mouse.is_none() && !command.force_mouse;
        let choice = match command.starting_choice {
            Some(None) => None,
            Some(Some(choice)) => Some(choice),
            None if no_mouse => items.iter().position(|item| !item.separator),
            None => None,
        };

        Ok(MenuOverlayState {
            id: 0,
            command_context: identity.command_context(command_context, target.clone()),
            identity,
            origin,
            current_target: target,
            rect,
            title,
            style: options.style,
            selected_style: options.selected_style,
            border_style: options.border_style,
            border_lines: options.border_lines,
            flags: ((command.stay_open as u8) * MENU_STAYOPEN) | ((no_mouse as u8) * MENU_NOMOUSE),
            choice,
            items,
        })
    }

    pub(super) async fn build_display_popup_state(
        &self,
        attach_pid: u32,
        command: ParsedDisplayPopupCommand,
        target: Target,
        identity: OverlayIdentity,
    ) -> Result<PopupOverlayState, RmuxError> {
        let attached_count = self.attached_count(target.session_name()).await;
        let (client_size, mouse, session_name) = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .ok_or_else(|| attached_client_required("display-popup"))?;
            (
                active.client_size,
                active.mouse.current_event.clone(),
                active.session_name.clone(),
            )
        };

        let state = self.state.lock().await;
        let runtime = format_context_for_target(&state, &target, attached_count)?;
        let title = render_runtime_template(&command.title, &runtime, true);
        let rendered_start_directory = command
            .start_directory
            .as_ref()
            .map(|cwd| render_runtime_template(&cwd.to_string_lossy(), &runtime, false))
            .map(PathBuf::from);
        let command_text = popup_shell_command(&state, &session_name, &command, &runtime)?;
        let styles = popup_styles_for_target(&state, &target, &command, &runtime);
        let width = resolve_popup_size(
            command.width,
            client_size.cols.max(1) / 2,
            client_size.cols.max(1),
        );
        let height = resolve_popup_size(
            command.height,
            client_size.rows.max(1) / 2,
            client_size.rows.max(1),
        );
        let context =
            overlay_position_context(&state, &session_name, &target, client_size, mouse.as_ref());
        let rect = resolve_overlay_rect(
            runtime,
            context,
            command.x.as_deref(),
            command.y.as_deref(),
            width,
            height,
        )
        .ok_or_else(|| {
            RmuxError::Server("display-popup does not fit in attached client".to_owned())
        })?;
        let content_size = popup_content_size(rect, styles.border_lines);
        let surface = Arc::new(StdMutex::new(PopupSurface::new(content_size)));

        let mut popup = PopupOverlayState {
            id: 0,
            identity,
            current_target: target.clone(),
            rect,
            preferred_width: width,
            preferred_height: height,
            title,
            style: styles.style,
            border_style: styles.border_style,
            border_lines: styles.border_lines,
            close_on_exit: command.close_on_exit,
            close_on_zero_exit: command.close_on_zero_exit,
            close_any_key: command.close_any_key,
            no_job: command.no_job,
            surface,
            scrollable_text: None,
            job: None,
            nested_menu: None,
            dragging: PopupDragMode::Off,
        };

        let should_spawn_job =
            !command.no_job && (!command.close_any_key || command_text.is_some());
        if should_spawn_job {
            let profile = TerminalProfile::for_run_shell(
                &state.environment,
                &state.options,
                Some(&session_name),
                state
                    .sessions
                    .session(&session_name)
                    .map(|session| session.id().as_u32()),
                &self.socket_path(),
                !self.config_loading_active(),
                rendered_start_directory.as_deref(),
            )?;
            let (job, initial_bytes) = spawn_popup_job(
                content_size,
                &profile,
                command_text.as_deref(),
                &command.environment,
            )?;
            if !initial_bytes.is_empty() {
                popup
                    .surface
                    .lock()
                    .expect("popup surface")
                    .append(&initial_bytes);
            }
            popup.job = Some(job);
        }

        Ok(popup)
    }
}

fn normal_action() -> QueueCommandAction {
    QueueCommandAction::Normal {
        output: None,
        error: None,
        source_file_error: None,
        exit_status: None,
    }
}

fn runtime_with_mouse_values<'a>(
    mut runtime: RuntimeFormatContext<'a>,
    state: &'a HandlerState,
    target: &Target,
    mouse: Option<&AttachedMouseEvent>,
) -> RuntimeFormatContext<'a> {
    let Some(mouse) = mouse else {
        return runtime;
    };
    runtime = runtime
        .with_named_value("mouse_x", mouse.raw.x.to_string())
        .with_named_value("mouse_y", mouse.raw.y.to_string());

    let Target::Pane(pane_target) = target else {
        return runtime;
    };
    let Some(session) = state.sessions.session(pane_target.session_name()) else {
        return runtime;
    };
    let Some(window) = session.window_at(pane_target.window_index()) else {
        return runtime;
    };
    let Some(pane) = window.pane(pane_target.pane_index()) else {
        return runtime;
    };
    let Ok(transcript) = state.transcript_handle(pane_target) else {
        return runtime;
    };
    let (screen, copy_summary) = {
        let transcript = transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned");
        (transcript.clone_screen(), transcript.copy_mode_summary())
    };
    let line_numbers = copy_summary.as_ref().and_then(|summary| {
        CopyModeLineNumberLayout::resolve(
            state.options.resolve_for_pane(
                pane_target.session_name(),
                pane_target.window_index(),
                pane_target.pane_index(),
                OptionName::CopyModeLineNumbers,
            ),
            summary.line_numbers_enabled,
            summary.history_size,
            summary.backing_rows,
            summary.scroll_position,
            summary.cursor_y,
        )
    });
    let pane_geometry = layout_for_session(state, pane_target.session_name(), 1)
        .and_then(|layout| {
            layout
                .panes
                .into_iter()
                .find(|candidate| candidate.pane_id == pane.id())
                .map(|candidate| candidate.geometry)
        })
        .unwrap_or_else(|| pane.geometry());
    let Some(mouse_context) =
        copy_mode_mouse_context_with_line_numbers(mouse, pane_geometry, -1, line_numbers)
    else {
        return runtime;
    };
    let word_separators = state
        .options
        .resolve(Some(pane_target.session_name()), OptionName::WordSeparators)
        .filter(|value| !value.is_empty())
        .unwrap_or(" -_@")
        .to_owned();
    let context = CopyModeCommandContext {
        mode_keys: ModeKeys::parse(state.options.resolve_for_pane(
            pane_target.session_name(),
            pane_target.window_index(),
            pane_target.pane_index(),
            OptionName::ModeKeys,
        )),
        line_number_mode: crate::copy_mode::CopyModeLineNumberMode::parse(
            state.options.resolve_for_pane(
                pane_target.session_name(),
                pane_target.window_index(),
                pane_target.pane_index(),
                OptionName::CopyModeLineNumbers,
            ),
        ),
        wrap_search: state.options.resolve_for_pane(
            pane_target.session_name(),
            pane_target.window_index(),
            pane_target.pane_index(),
            OptionName::WrapSearch,
        ) != Some("off"),
        word_separators,
        default_shell: String::new(),
        working_directory: None,
        refresh_screen: None,
        mouse: Some(mouse_context),
    };
    let summary = CopyModeState::summary_for_mouse(screen, &context);
    runtime
        .with_named_value("mouse_word", summary.copy_cursor_word)
        .with_named_value("mouse_line", summary.copy_cursor_line)
        .with_named_value("mouse_hyperlink", summary.copy_cursor_hyperlink)
}
