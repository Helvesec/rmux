use rmux_proto::{PaneTarget, RmuxError, SessionName, Target};

use crate::{Pane, SessionStore, Window};

use super::matching::{target_for_type, target_not_found, ResolvedParts};
use super::{MissingCurrentTargetFallback, TargetFindContext, TargetFindFlags, TargetFindType};

impl SessionStore {
    pub(super) fn current_target_for_type(
        &self,
        find_type: TargetFindType,
        flags: TargetFindFlags,
        context: &TargetFindContext,
    ) -> Result<Target, RmuxError> {
        if flags.contains(TargetFindFlags::DEFAULT_MARKED) {
            if let Some(marked_target) = context.marked_target() {
                let parts = self.parts_for_target(marked_target)?;
                return Ok(target_for_type(find_type, parts));
            }
        }

        let current = match self.current_parts(context) {
            Ok(current) => current,
            Err(_)
                if context.missing_current_target_fallback()
                    == MissingCurrentTargetFallback::AllowDefaultSession =>
            {
                self.default_session_name(flags)
                    .and_then(|session_name| self.parts_for_session_current(&session_name))?
            }
            Err(error) => return Err(error),
        };
        Ok(target_for_type(find_type, current))
    }

    pub(super) fn mouse_target_for_type(
        &self,
        find_type: TargetFindType,
        context: &TargetFindContext,
    ) -> Result<Target, RmuxError> {
        let mouse_target = context.mouse_target().ok_or_else(|| {
            RmuxError::Server(
                "target form {mouse} is recognized but deferred until mouse event state reaches the command queue".to_owned(),
            )
        })?;
        let parts = self.parts_for_target(mouse_target)?;
        Ok(target_for_type(find_type, parts))
    }

    pub(super) fn marked_target_for_type(
        &self,
        find_type: TargetFindType,
        context: &TargetFindContext,
    ) -> Result<Target, RmuxError> {
        let marked_target = context
            .marked_target()
            .ok_or_else(|| target_not_found("{marked}", "pane"))?;
        let parts = self.parts_for_target(marked_target)?;
        Ok(target_for_type(find_type, parts))
    }

    pub(super) fn target_for_session_current(
        &self,
        find_type: TargetFindType,
        session_name: SessionName,
    ) -> Result<Target, RmuxError> {
        let parts = self.parts_for_session_current(&session_name)?;
        Ok(target_for_type(find_type, parts))
    }

    pub(super) fn target_for_window_current(
        &self,
        find_type: TargetFindType,
        window: ResolvedParts,
    ) -> Result<Target, RmuxError> {
        if find_type != TargetFindType::Pane {
            return Ok(target_for_type(find_type, window));
        }

        let session = self
            .session(&window.session_name)
            .ok_or_else(|| target_not_found(window.session_name.as_str(), "session"))?;
        let pane_index = session
            .window_at(window.window_index)
            .and_then(Window::active_pane)
            .map(Pane::index)
            .ok_or_else(|| target_not_found(&window.to_window_string(), "pane"))?;
        Ok(Target::Pane(PaneTarget::with_window(
            window.session_name,
            window.window_index,
            pane_index,
        )))
    }

    pub(super) fn current_parts(
        &self,
        context: &TargetFindContext,
    ) -> Result<ResolvedParts, RmuxError> {
        let current = context
            .current()
            .ok_or_else(|| RmuxError::Server("no current target".to_owned()))?;
        self.parts_for_target(current)
    }

    pub(super) fn parts_for_target(&self, current: &Target) -> Result<ResolvedParts, RmuxError> {
        let session_name = current.session_name().clone();
        let session = self
            .session(&session_name)
            .ok_or_else(|| target_not_found(session_name.as_str(), "session"))?;
        let window_index = match current {
            Target::Session(_) => session.active_window_index(),
            Target::Window(target) => target.window_index(),
            Target::Pane(target) => target.window_index(),
        };
        let window = session
            .window_at(window_index)
            .ok_or_else(|| target_not_found(&format!("{session_name}:{window_index}"), "window"))?;
        let pane_index = match current {
            Target::Pane(target) => target.pane_index(),
            Target::Session(_) | Target::Window(_) => window.active_pane_index(),
        };
        if window.pane(pane_index).is_none() {
            return Err(target_not_found(
                &format!("{session_name}:{window_index}.{pane_index}"),
                "pane",
            ));
        }
        Ok(ResolvedParts::pane(session_name, window_index, pane_index))
    }

    pub(super) fn parts_for_window(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Result<ResolvedParts, RmuxError> {
        let session = self
            .session(session_name)
            .ok_or_else(|| target_not_found(session_name.as_str(), "session"))?;
        if session.window_at(window_index).is_none() {
            return Err(target_not_found(
                &format!("{session_name}:{window_index}"),
                "window",
            ));
        }
        Ok(ResolvedParts::window(session_name.clone(), window_index))
    }
}
