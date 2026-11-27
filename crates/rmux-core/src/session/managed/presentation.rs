use super::*;

/// Short-lived presentation projection. Unchanged windows remain borrowed from
/// their canonical owners; only windows with render overrides are copied.
/// This value cannot be inserted into the live store or used for rollback.
pub struct ManagedSessionPresentation<'a> {
    metadata: ManagedSession,
    viewport_offset: Option<(u16, u16)>,
    windows: PresentationWindows<'a>,
    overrides: BTreeMap<u32, Window>,
}

enum PresentationWindows<'a> {
    Borrowed(&'a WindowStateStore),
    Owned(Box<WindowStateStore>),
}

impl PresentationWindows<'_> {
    fn as_ref(&self) -> &WindowStateStore {
        match self {
            Self::Borrowed(windows) => windows,
            Self::Owned(windows) => windows,
        }
    }
}

impl<'a> ManagedSessionPresentation<'a> {
    /// Borrows all window owners and copies session-local presentation metadata.
    pub fn new(session: ManagedSessionView<'a>) -> Self {
        Self {
            metadata: session.metadata.clone(),
            viewport_offset: session.viewport_offset,
            windows: PresentationWindows::Borrowed(session.windows),
            overrides: session.overrides.cloned().unwrap_or_default(),
        }
    }

    /// Detaches the selected window and session metadata for rendering after
    /// releasing the caller's state lock. Other windows are deliberately not
    /// retained. This presentation cannot be inserted into the live store.
    #[must_use]
    pub fn into_owned_window(self) -> ManagedSessionPresentation<'static> {
        let mut windows = WindowStateStore::new();
        let link = WindowLink {
            session_id: self.metadata.id,
            window_index: self.metadata.active_window,
        };
        windows
            .introduce(link, self.view().window().clone())
            .expect("one detached presentation window");
        ManagedSessionPresentation {
            metadata: self.metadata,
            viewport_offset: self.viewport_offset,
            windows: PresentationWindows::Owned(Box::new(windows)),
            overrides: BTreeMap::new(),
        }
    }

    /// Read-only view of the projected session.
    pub fn view(&self) -> ManagedSessionView<'_> {
        ManagedSessionView {
            metadata: &self.metadata,
            viewport_offset: self.viewport_offset,
            windows: self.windows.as_ref(),
            overrides: Some(&self.overrides),
        }
    }

    /// Translates the rendered window without changing canonical pane geometry.
    pub fn set_viewport_offset(&mut self, offset: Option<(u16, u16)>) {
        self.viewport_offset = offset;
    }

    /// Changes outer rendering geometry without changing window content.
    pub fn set_terminal_size(&mut self, size: TerminalSize) {
        self.metadata.terminal_size = size;
    }

    /// Projects content geometry for one occurrence only.
    pub fn resize_window(&mut self, index: u32, size: TerminalSize) -> Result<(), RmuxError> {
        self.project_window(index)?.set_size(size);
        Ok(())
    }

    /// Projects session-local navigation without copying any windows.
    pub fn select_window(&mut self, index: u32) -> Result<(), RmuxError> {
        if self.view().window_at(index).is_none() {
            return Err(invalid_window_target(&self.metadata.name, index));
        }
        crate::session::window_ops::navigation::select_existing_window(
            index,
            &mut self.metadata.active_window,
            &mut self.metadata.last_window,
            &mut self.metadata.window_stack,
            &mut self.metadata.group_initial_window_id,
            &mut self.metadata.winlink_alert_flags,
        );
        Ok(())
    }

    /// Borrows only the projected window for the existing switch sequencing.
    pub fn window_switch_mut(&mut self, index: u32) -> Result<WindowSwitch<'_>, RmuxError> {
        self.project_window(index)?;
        Ok(WindowSwitch {
            session_name: &self.metadata.name,
            window_index: index,
            terminal_size: &mut self.metadata.terminal_size,
            window: self.overrides.get_mut(&index).expect("projected window"),
            winlink_alert_flags: &mut self.metadata.winlink_alert_flags,
            active_window: &mut self.metadata.active_window,
            last_window: &mut self.metadata.last_window,
            window_stack: &mut self.metadata.window_stack,
            group_initial_window_id: &mut self.metadata.group_initial_window_id,
        })
    }

    fn project_window(&mut self, index: u32) -> Result<&mut Window, RmuxError> {
        if !self.overrides.contains_key(&index) {
            let window = self
                .view()
                .window_at(index)
                .ok_or_else(|| invalid_window_target(&self.metadata.name, index))?
                .clone();
            self.overrides.insert(index, window);
        }
        Ok(self.overrides.get_mut(&index).expect("projected window"))
    }
}
