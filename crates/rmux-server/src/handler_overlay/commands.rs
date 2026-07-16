use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use rmux_proto::{OptionName, RmuxError, Target};

use super::super::scripting_support::{
    format_context_for_target, QueueCommandAction, QueueExecutionContext,
};
use super::super::RequestHandler;
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
use crate::copy_mode::{CopyModeCommandContext, CopyModeState, ModeKeys};
use crate::format_runtime::render_runtime_template;
use crate::format_runtime::RuntimeFormatContext;
use crate::handler_support::attached_client_required;
use crate::mouse::{copy_mode_mouse_context, AttachedMouseEvent};
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
        let attach_pid = self
            .resolve_overlay_client(
                requester_pid,
                command.target_client.as_deref(),
                "display-menu",
            )
            .await?;
        if self.mode_tree_active(attach_pid).await {
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            });
        }

        let current_overlay = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .and_then(|active| active.overlay.clone())
        };

        let target = self
            .resolve_overlay_target(
                attach_pid,
                command.target_pane.clone(),
                context.current_target().cloned(),
            )
            .await?;
        let built = self
            .build_display_menu_state(attach_pid, requester_pid, command, target)
            .await?;

        {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .ok_or_else(|| attached_client_required("display-menu"))?;
            active.overlay_state_id = active.overlay_state_id.saturating_add(1);
            let overlay_id = active.overlay_state_id;
            let mut built = built;
            built.id = overlay_id;
            match current_overlay {
                Some(ClientOverlayState::Popup(mut popup)) => {
                    if popup.nested_menu.is_some() {
                        return Ok(QueueCommandAction::Normal {
                            output: None,
                            error: None,
                            source_file_error: None,
                            exit_status: None,
                        });
                    }
                    popup.nested_menu = Some(built);
                    active.overlay = Some(ClientOverlayState::Popup(popup));
                }
                Some(ClientOverlayState::Menu(_)) => {
                    return Ok(QueueCommandAction::Normal {
                        output: None,
                        error: None,
                        source_file_error: None,
                        exit_status: None,
                    });
                }
                None => {
                    active.overlay = Some(ClientOverlayState::Menu(Box::new(built)));
                }
            }
        }

        self.refresh_interactive_overlay_if_active(attach_pid)
            .await?;
        Ok(QueueCommandAction::Normal {
            output: None,
            error: None,
            source_file_error: None,
            exit_status: None,
        })
    }

    pub(super) async fn execute_queued_display_popup(
        &self,
        requester_pid: u32,
        command: ParsedDisplayPopupCommand,
        context: &QueueExecutionContext,
    ) -> Result<QueueCommandAction, RmuxError> {
        if command.close_existing {
            if let Ok(attach_pid) = self
                .resolve_overlay_client(
                    requester_pid,
                    command.target_client.as_deref(),
                    "display-popup",
                )
                .await
            {
                let _ = self.clear_interactive_overlay(attach_pid, true).await;
            }
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            });
        }

        let attach_pid = match self
            .resolve_overlay_client(
                requester_pid,
                command.target_client.as_deref(),
                "display-popup",
            )
            .await
        {
            Ok(attach_pid) => attach_pid,
            Err(error) => return Err(error),
        };
        if self.mode_tree_active(attach_pid).await {
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            });
        }

        let target = self
            .resolve_overlay_target(
                attach_pid,
                command.target_pane.clone(),
                context.current_target().cloned(),
            )
            .await?;

        let existing_overlay_is_menu = {
            let active_attach = self.active_attach.lock().await;
            matches!(
                active_attach
                    .by_pid
                    .get(&attach_pid)
                    .and_then(|active| active.overlay.as_ref()),
                Some(ClientOverlayState::Menu(_))
            )
        };
        if existing_overlay_is_menu {
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            });
        }

        let mut popup = self
            .build_display_popup_state(attach_pid, requester_pid, command, target)
            .await?;

        let (popup_identity, replaced_popup_job) = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .ok_or_else(|| attached_client_required("display-popup"))?;
            active.overlay_state_id = active.overlay_state_id.saturating_add(1);
            popup.id = active.overlay_state_id;
            let replaced_popup_job = match active
                .overlay
                .replace(ClientOverlayState::Popup(Box::new(popup.clone())))
            {
                Some(ClientOverlayState::Popup(replaced)) => replaced.job,
                Some(ClientOverlayState::Menu(_)) | None => None,
            };
            (active.identity(attach_pid), replaced_popup_job)
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

        self.refresh_interactive_overlay_if_active(attach_pid)
            .await?;
        Ok(QueueCommandAction::Normal {
            output: None,
            error: None,
            source_file_error: None,
            exit_status: None,
        })
    }

    async fn build_display_menu_state(
        &self,
        attach_pid: u32,
        requester_pid: u32,
        command: ParsedDisplayMenuCommand,
        target: Target,
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
        let context =
            overlay_position_context(&state, &session_name, &target, client_size, mouse.as_ref());
        let rect = resolve_overlay_rect(
            runtime,
            context,
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
            requester_pid,
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
        requester_pid: u32,
        command: ParsedDisplayPopupCommand,
        target: Target,
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
            requester_pid,
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
    let Some(mouse_context) = copy_mode_mouse_context(mouse, pane.geometry(), -1) else {
        return runtime;
    };
    let Ok(transcript) = state.transcript_handle(pane_target) else {
        return runtime;
    };
    let screen = transcript
        .lock()
        .expect("pane transcript mutex must not be poisoned")
        .clone_screen();
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
