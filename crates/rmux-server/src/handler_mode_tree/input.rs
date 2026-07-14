use rmux_core::formats::FormatContext;
use rmux_proto::RmuxError;

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::input_keys::MouseForwardEvent;

use super::super::prompt_support::PromptInputEvent;
use super::super::RequestHandler;
use super::mode_tree_model::{
    ModeTreeAction, ModeTreeActionIdentity, ModeTreeBuild, ModeTreeClientState,
    ModeTreeDeferredAction, ModeTreeKind, ModeTreePromptCallback, SearchDirection,
};
use super::mode_tree_render::{mode_tree_list_rows, sanitize_overlay_text};
use super::mode_tree_selection::{
    collapse_or_parent, current_selected_item, current_tree_kill_prompt, cycle_sort,
    expand_or_child, move_selection, repeat_search, select_edge, selected_items, tag_all,
    tagged_tree_kill_prompt, toggle_tag,
};

impl RequestHandler {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(in crate::handler) async fn handle_mode_tree_key_event(
        &self,
        attach_pid: u32,
        event: PromptInputEvent,
    ) -> Result<bool, RmuxError> {
        self.handle_mode_tree_key_event_with_identity(attach_pid, None, event)
            .await
    }

    pub(in crate::handler) async fn handle_mode_tree_key_event_for_identity(
        &self,
        identity: super::super::attach_support::ActiveAttachIdentity,
        event: PromptInputEvent,
    ) -> Result<bool, RmuxError> {
        self.handle_mode_tree_key_event_with_identity(identity.attach_pid(), Some(identity), event)
            .await
    }

    async fn handle_mode_tree_key_event_with_identity(
        &self,
        attach_pid: u32,
        identity: Option<super::super::attach_support::ActiveAttachIdentity>,
        event: PromptInputEvent,
    ) -> Result<bool, RmuxError> {
        let (mut mode, action_identity) = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    identity.is_none_or(|identity| identity.matches_active(active))
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            let Some(mode) = active.mode_tree.clone() else {
                return Ok(false);
            };
            (
                mode,
                ModeTreeActionIdentity::new(attach_pid, active.id, active.mode_tree_state_id),
            )
        };
        let had_tagged_items_before_rebuild = !mode.tagged.is_empty();
        let selected_id_before_rebuild = mode.selected_id.clone();
        let build = self.build_mode_tree(&mut mode, attach_pid).await?;
        if build.visible.is_empty() {
            if matches!(
                event,
                PromptInputEvent::Escape | PromptInputEvent::Char('q')
            ) {
                self.dismiss_mode_tree_with_refresh(attach_pid).await?;
                return Ok(true);
            }
            return Ok(false);
        }
        let all_tagged_items_disappeared =
            had_tagged_items_before_rebuild && mode.tagged.is_empty();
        let current_selection_disappeared = selected_id_before_rebuild
            .as_ref()
            .is_some_and(|selected_id| !build.items.contains_key(selected_id));
        let event_uses_current_selection = matches!(
            (&event, mode.kind),
            (PromptInputEvent::Char('x'), ModeTreeKind::Tree)
        ) || (!had_tagged_items_before_rebuild
            && event_uses_tagged_or_current_selection(&event, mode.kind));
        if (all_tagged_items_disappeared
            || (current_selection_disappeared && event_uses_current_selection))
            && event_uses_tagged_or_current_selection(&event, mode.kind)
        {
            self.store_mode_tree_state(attach_pid, mode).await?;
            self.refresh_mode_tree_overlay_if_active(attach_pid).await?;
            return Ok(true);
        }

        match event {
            PromptInputEvent::Up | PromptInputEvent::Ctrl('p') | PromptInputEvent::Char('k') => {
                move_selection(&mut mode, &build, -1, true);
            }
            PromptInputEvent::Down | PromptInputEvent::Ctrl('n') | PromptInputEvent::Char('j') => {
                move_selection(&mut mode, &build, 1, true);
            }
            PromptInputEvent::Home => select_edge(&mut mode, &build, false),
            PromptInputEvent::End => select_edge(&mut mode, &build, true),
            PromptInputEvent::KeyName(name) if name == "PageUp" => {
                move_selection(&mut mode, &build, -10, false);
            }
            PromptInputEvent::KeyName(name) if name == "PageDown" => {
                move_selection(&mut mode, &build, 10, false);
            }
            PromptInputEvent::Left => collapse_or_parent(&mut mode, &build),
            PromptInputEvent::Right => expand_or_child(&mut mode, &build),
            PromptInputEvent::Enter => {
                self.store_mode_tree_state(attach_pid, mode).await?;
                self.accept_mode_tree_selection(attach_pid).await?;
                return Ok(true);
            }
            PromptInputEvent::Escape | PromptInputEvent::Char('q') => {
                self.dismiss_mode_tree_with_refresh(attach_pid).await?;
                return Ok(true);
            }
            PromptInputEvent::Char('t') => toggle_tag(&mut mode, &build),
            PromptInputEvent::Ctrl('t') => tag_all(&mut mode, &build),
            PromptInputEvent::Ctrl('s') => {
                self.store_mode_tree_state(attach_pid, mode).await?;
                self.start_mode_tree_prompt(
                    attach_pid,
                    ModeTreePromptCallback::Search(SearchDirection::Forward),
                )
                .await?;
                return Ok(true);
            }
            PromptInputEvent::Ctrl('r') => {
                self.store_mode_tree_state(attach_pid, mode).await?;
                self.start_mode_tree_prompt(
                    attach_pid,
                    ModeTreePromptCallback::Search(SearchDirection::Backward),
                )
                .await?;
                return Ok(true);
            }
            PromptInputEvent::Char('n') => repeat_search(&mut mode, &build, false),
            PromptInputEvent::Char('N') => repeat_search(&mut mode, &build, true),
            PromptInputEvent::Char('f') => {
                self.store_mode_tree_state(attach_pid, mode).await?;
                self.start_mode_tree_prompt(attach_pid, ModeTreePromptCallback::Filter)
                    .await?;
                return Ok(true);
            }
            PromptInputEvent::Char('o') | PromptInputEvent::Char('O') => cycle_sort(&mut mode),
            PromptInputEvent::Char('r') => mode.reversed = !mode.reversed,
            PromptInputEvent::Char('v') => {
                mode.preview_mode = mode.preview_mode.cycle();
                mode.preview_scroll = 0;
            }
            PromptInputEvent::Char(':') => {
                self.store_mode_tree_state(attach_pid, mode).await?;
                self.start_mode_tree_prompt(attach_pid, ModeTreePromptCallback::Command)
                    .await?;
                return Ok(true);
            }
            PromptInputEvent::Char('p') | PromptInputEvent::Char('P')
                if matches!(mode.kind, ModeTreeKind::Buffer) =>
            {
                let delete_after = matches!(event, PromptInputEvent::Char('P'));
                let action_identity = self
                    .store_mode_tree_state_for_action_identity(action_identity, mode)
                    .await?;
                self.perform_buffer_paste_for_identity(action_identity, delete_after)
                    .await?;
                return Ok(true);
            }
            PromptInputEvent::Char('d') | PromptInputEvent::Char('D')
                if matches!(mode.kind, ModeTreeKind::Buffer) =>
            {
                let action_identity = self
                    .store_mode_tree_state_for_action_identity(action_identity, mode)
                    .await?;
                self.perform_buffer_delete_for_identity(action_identity)
                    .await?;
                return Ok(true);
            }
            PromptInputEvent::Char('d') | PromptInputEvent::Char('D')
                if matches!(mode.kind, ModeTreeKind::Client) =>
            {
                self.store_mode_tree_state(attach_pid, mode).await?;
                self.perform_client_detach(attach_pid).await?;
                return Ok(true);
            }
            PromptInputEvent::Char('x') | PromptInputEvent::Char('X')
                if matches!(mode.kind, ModeTreeKind::Tree) =>
            {
                let prompt = match event {
                    PromptInputEvent::Char('x') => current_tree_kill_prompt(&mode, &build),
                    PromptInputEvent::Char('X') => tagged_tree_kill_prompt(&mode),
                    _ => None,
                };
                let Some(prompt) = prompt else {
                    return Ok(false);
                };
                let action = match event {
                    PromptInputEvent::Char('x') => {
                        let targets = current_selected_item(&mode, &build)
                            .map(|item| vec![item.action.clone()])
                            .unwrap_or_default();
                        ModeTreeDeferredAction::KillCurrentTreeSelection { targets }
                    }
                    PromptInputEvent::Char('X') => {
                        ModeTreeDeferredAction::KillTaggedTreeSelections {
                            targets: selected_mode_tree_actions(&mode, &build),
                        }
                    }
                    _ => unreachable!("tree kill prompt only binds x/X"),
                };
                self.store_mode_tree_state(attach_pid, mode).await?;
                self.confirm_mode_tree_action(attach_pid, prompt, action)
                    .await?;
                return Ok(true);
            }
            PromptInputEvent::Char('x') | PromptInputEvent::Char('X')
                if matches!(mode.kind, ModeTreeKind::Buffer) =>
            {
                let targets = selected_mode_tree_actions(&mode, &build);
                self.store_mode_tree_state(attach_pid, mode).await?;
                self.confirm_mode_tree_action(
                    attach_pid,
                    "delete selected buffers?".to_owned(),
                    ModeTreeDeferredAction::DeleteBuffers { targets },
                )
                .await?;
                return Ok(true);
            }
            PromptInputEvent::Char('x') | PromptInputEvent::Char('X')
                if matches!(mode.kind, ModeTreeKind::Client) =>
            {
                let targets = selected_mode_tree_actions(&mode, &build);
                self.store_mode_tree_state(attach_pid, mode).await?;
                self.confirm_mode_tree_action(
                    attach_pid,
                    "detach selected clients?".to_owned(),
                    ModeTreeDeferredAction::DetachClients { targets },
                )
                .await?;
                return Ok(true);
            }
            PromptInputEvent::Char('s') if matches!(mode.kind, ModeTreeKind::Customize) => {
                self.store_mode_tree_state(attach_pid, mode).await?;
                self.start_customize_set_prompt(attach_pid).await?;
                return Ok(true);
            }
            PromptInputEvent::Char('u') if matches!(mode.kind, ModeTreeKind::Customize) => {
                self.store_mode_tree_state(attach_pid, mode).await?;
                self.perform_customize_unset(attach_pid).await?;
                return Ok(true);
            }
            PromptInputEvent::Ctrl('x') if matches!(mode.kind, ModeTreeKind::Customize) => {
                self.store_mode_tree_state(attach_pid, mode).await?;
                self.perform_customize_reset(attach_pid).await?;
                return Ok(true);
            }
            PromptInputEvent::KeyName(name) if name == "F1" => {
                self.show_mode_tree_help(attach_pid).await?;
                return Ok(true);
            }
            PromptInputEvent::Ctrl('h') => {
                self.show_mode_tree_help(attach_pid).await?;
                return Ok(true);
            }
            _ => {
                // Shortcut keys are checked last so navigation keys take priority
                // (matching tmux's mode_tree_key dispatch order).
                if let Some(shortcut_id) = shortcut_match(&mode, &build, &event) {
                    mode.selected_id = Some(shortcut_id);
                    self.store_mode_tree_state(attach_pid, mode).await?;
                    self.accept_mode_tree_selection(attach_pid).await?;
                    return Ok(true);
                }
                return Ok(false);
            }
        }

        self.store_mode_tree_state(attach_pid, mode).await?;
        self.refresh_mode_tree_overlay_if_active(attach_pid).await?;
        Ok(true)
    }

    async fn store_mode_tree_state_for_action_identity(
        &self,
        identity: ModeTreeActionIdentity,
        mode: ModeTreeClientState,
    ) -> Result<ModeTreeActionIdentity, RmuxError> {
        let mut active_attach = self.active_attach.lock().await;
        let requester_is_current = active_attach
            .by_pid
            .get(&identity.attach_pid())
            .is_some_and(|active| {
                active.id == identity.attach_id()
                    && active.mode_tree_state_id == identity.state_id()
                    && active.mode_tree.is_some()
                    && active.session_name == mode.session_name
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            });
        if !requester_is_current {
            return Err(crate::handler_support::attached_client_required(
                mode.kind.command_name(),
            ));
        }

        let mut requester_state_id = None;
        for (attach_pid, active) in &mut active_attach.by_pid {
            if active.session_name != mode.session_name || active.mode_tree.is_none() {
                continue;
            }
            active.mode_tree_state_id = active.mode_tree_state_id.saturating_add(1);
            active.persistent_overlay_epoch.store(
                active.mode_tree_state_id,
                std::sync::atomic::Ordering::SeqCst,
            );
            active.mode_tree = Some(mode.clone());
            if *attach_pid == identity.attach_pid() {
                requester_state_id = Some(active.mode_tree_state_id);
            }
        }

        requester_state_id
            .map(|state_id| {
                ModeTreeActionIdentity::new(identity.attach_pid(), identity.attach_id(), state_id)
            })
            .ok_or_else(|| {
                crate::handler_support::attached_client_required(mode.kind.command_name())
            })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(in crate::handler) async fn handle_mode_tree_mouse_event(
        &self,
        attach_pid: u32,
        event: MouseForwardEvent,
    ) -> Result<bool, RmuxError> {
        self.handle_mode_tree_mouse_event_with_identity(attach_pid, None, event)
            .await
    }

    pub(in crate::handler) async fn handle_mode_tree_mouse_event_for_identity(
        &self,
        identity: super::super::attach_support::ActiveAttachIdentity,
        event: MouseForwardEvent,
    ) -> Result<bool, RmuxError> {
        self.handle_mode_tree_mouse_event_with_identity(
            identity.attach_pid(),
            Some(identity),
            event,
        )
        .await
    }

    async fn handle_mode_tree_mouse_event_with_identity(
        &self,
        attach_pid: u32,
        identity: Option<super::super::attach_support::ActiveAttachIdentity>,
        event: MouseForwardEvent,
    ) -> Result<bool, RmuxError> {
        let mut mode = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| identity.is_none_or(|identity| identity.matches_active(active)))
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            let Some(mode) = active.mode_tree.clone() else {
                return Ok(false);
            };
            mode
        };
        let build = self.build_mode_tree(&mut mode, attach_pid).await?;
        if build.visible.is_empty() {
            return Ok(false);
        }

        let geometry = self.mode_tree_status_geometry(&mode).await?;
        let rows = geometry.content_rows();
        let list_rows = mode_tree_list_rows(rows, build.visible.len(), mode.preview_mode);
        let Some(content_y) = event
            .y
            .checked_sub(geometry.content_y_offset())
            .filter(|content_y| *content_y < rows)
        else {
            return Ok(false);
        };
        let y = usize::from(content_y);
        if y < usize::from(list_rows) {
            let index = mode.scroll + y;
            if let Some(id) = build.visible.get(index) {
                let changed = mode.selected_id.as_ref() != Some(id);
                mode.selected_id = Some(id.clone());
                if changed {
                    mode.preview_scroll = 0;
                }
                self.store_mode_tree_state(attach_pid, mode).await?;
                self.refresh_mode_tree_overlay_if_active(attach_pid).await?;
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn selected_mode_tree_actions(
    mode: &ModeTreeClientState,
    build: &ModeTreeBuild,
) -> Vec<ModeTreeAction> {
    selected_items(mode, build)
        .into_iter()
        .map(|item| item.action.clone())
        .collect()
}

fn event_uses_tagged_or_current_selection(event: &PromptInputEvent, kind: ModeTreeKind) -> bool {
    match event {
        PromptInputEvent::Enter | PromptInputEvent::Char(':') => true,
        PromptInputEvent::Char('p' | 'P') => matches!(kind, ModeTreeKind::Buffer),
        PromptInputEvent::Char('d' | 'D') => {
            matches!(kind, ModeTreeKind::Buffer | ModeTreeKind::Client)
        }
        PromptInputEvent::Char('x' | 'X') => {
            matches!(
                kind,
                ModeTreeKind::Tree | ModeTreeKind::Buffer | ModeTreeKind::Client
            )
        }
        PromptInputEvent::Char('s' | 'u') | PromptInputEvent::Ctrl('x') => {
            matches!(kind, ModeTreeKind::Customize)
        }
        _ => false,
    }
}

fn shortcut_match(
    mode: &ModeTreeClientState,
    build: &ModeTreeBuild,
    event: &PromptInputEvent,
) -> Option<String> {
    let key = event_key_name(event);
    if key.is_empty() {
        return None;
    }
    for (index, id) in build.visible.iter().enumerate() {
        let rendered = render_runtime_template(
            &mode.key_format,
            &RuntimeFormatContext::new(FormatContext::new())
                .with_named_value("line", index.to_string()),
            false,
        );
        if !rendered.is_empty() && sanitize_overlay_text(&rendered) == key {
            return Some(id.clone());
        }
    }
    None
}

fn event_key_name(event: &PromptInputEvent) -> String {
    match event {
        PromptInputEvent::Char(ch) => ch.to_string(),
        PromptInputEvent::Ctrl(ch) => format!("C-{ch}"),
        PromptInputEvent::KeyName(name) => name.clone(),
        PromptInputEvent::Enter => "Enter".to_owned(),
        PromptInputEvent::Escape => "Escape".to_owned(),
        PromptInputEvent::Left => "Left".to_owned(),
        PromptInputEvent::Right => "Right".to_owned(),
        PromptInputEvent::Up => "Up".to_owned(),
        PromptInputEvent::Down => "Down".to_owned(),
        PromptInputEvent::Home => "Home".to_owned(),
        PromptInputEvent::End => "End".to_owned(),
        PromptInputEvent::Delete => "DC".to_owned(),
        PromptInputEvent::Backspace => "BSpace".to_owned(),
        PromptInputEvent::Tab => "Tab".to_owned(),
    }
}
