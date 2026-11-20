//! Test setup over the same canonical store used by HandlerState.
use rmux_core::{ManagedPaneSplit, ManagedSessionStore, ManagedSessionView, Pane, Window};
use rmux_proto::{RmuxError, SessionName, SplitDirection, TerminalSize, WindowTarget};

pub(crate) struct SessionFixture {
    pub(crate) store: ManagedSessionStore,
    name: SessionName,
}

impl SessionFixture {
    pub(crate) fn new(name: SessionName, size: TerminalSize) -> Self {
        let mut store = ManagedSessionStore::new();
        store.create_session(name.clone(), size).unwrap();
        Self { store, name }
    }
}

macro_rules! setup_methods {
    () => {
        pub(crate) fn view(&self) -> ManagedSessionView<'_> {
            self.store.session(&self.name).unwrap()
        }
        pub(crate) fn name(&self) -> &SessionName {
            &self.name
        }
        pub(crate) fn window(&self) -> &Window {
            self.view().window()
        }
        pub(crate) fn active_pane(&self) -> Option<&Pane> {
            self.view().active_pane()
        }
        pub(crate) fn active_pane_index(&self) -> u32 {
            self.view().active_pane_index()
        }
        pub(crate) fn active_window_index(&self) -> u32 {
            self.view().active_window_index()
        }
        pub(crate) fn terminal_size(&self) -> TerminalSize {
            self.view().terminal_size()
        }
        pub(crate) fn window_at(&self, index: u32) -> Option<&Window> {
            self.view().window_at(index)
        }
        fn target(&self, index: u32) -> WindowTarget {
            WindowTarget::with_window(self.name.clone(), index)
        }
        pub(crate) fn split_active_pane(&mut self) -> Result<u32, RmuxError> {
            self.split_pane(self.active_pane_index())
        }
        pub(crate) fn split_pane(&mut self, pane: u32) -> Result<u32, RmuxError> {
            self.split_pane_with_direction(pane, SplitDirection::Vertical)
        }
        pub(crate) fn split_active_pane_with_direction(
            &mut self,
            direction: SplitDirection,
        ) -> Result<u32, RmuxError> {
            self.split_pane_with_direction(self.active_pane_index(), direction)
        }
        pub(crate) fn split_pane_with_direction(
            &mut self,
            pane: u32,
            direction: SplitDirection,
        ) -> Result<u32, RmuxError> {
            self.store
                .split_pane(
                    &self.target(self.active_window_index()),
                    pane,
                    ManagedPaneSplit {
                        direction,
                        before: false,
                        detached: false,
                        full_size: false,
                    },
                )
                .map(|created| created.pane_index)
        }
        pub(crate) fn select_pane(&mut self, pane: u32) -> Result<u32, RmuxError> {
            self.store
                .window_selection_mut(&self.target(self.active_window_index()))?
                .select(pane, false)
        }
        pub(crate) fn select_window(&mut self, index: u32) -> Result<(), RmuxError> {
            self.store.select_window(&self.target(index))
        }
        pub(crate) fn create_window(
            &mut self,
            size: TerminalSize,
        ) -> Result<(u32, rmux_core::PaneId), RmuxError> {
            self.store
                .create_window(&self.name, size)
                .map(|created| (created.window_index, created.pane_id))
        }
        pub(crate) fn rename_window(&mut self, index: u32, name: String) -> Result<(), RmuxError> {
            self.store
                .rename_window(&self.target(index), name)
                .map(|_| ())
        }
        pub(crate) fn resize_terminal(&mut self, size: TerminalSize) {
            assert_eq!(
                self.view().windows().count(),
                1,
                "terminal fixture has one window"
            );
            self.store
                .window_switch_mut(&self.target(self.active_window_index()))
                .unwrap()
                .resize(size, size);
        }
        pub(crate) fn resize_pane(
            &mut self,
            pane: u32,
            adjustment: rmux_proto::ResizePaneAdjustment,
        ) -> Result<(), RmuxError> {
            self.store
                .window_layout_context_mut(&self.target(self.active_window_index()))?
                .resize_pane(pane, adjustment)
        }

        pub(crate) fn id(&self) -> rmux_core::SessionId {
            self.view().id()
        }
        pub(crate) fn last_window_index(&self) -> Option<u32> {
            self.view().last_window_index()
        }
        pub(crate) fn winlink_alert_flags(&self, index: u32) -> rmux_core::AlertFlags {
            self.view().winlink_alert_flags(index)
        }
        pub(crate) fn touch_attached(&mut self) {
            self.store.touch_attached(&self.name).unwrap();
        }
        pub(crate) fn pin_public_times_for_tests(&mut self, seconds: i64) {
            self.store
                .pin_public_times_for_tests(&self.name, seconds)
                .unwrap();
        }
        pub(crate) fn add_winlink_alert_flags(
            &mut self,
            index: u32,
            flags: rmux_core::AlertFlags,
        ) -> bool {
            self.store
                .add_winlink_alert_flags(&self.target(index), flags)
                .unwrap()
        }
        pub(crate) fn clear_all_winlink_alert_flags(&mut self, index: u32) -> bool {
            self.store
                .clear_winlink_alert_flags(&self.target(index), rmux_core::WINLINK_ALERTFLAGS)
                .unwrap()
        }
        pub(crate) fn select_pane_in_window(
            &mut self,
            index: u32,
            pane: u32,
        ) -> Result<u32, RmuxError> {
            self.store
                .window_selection_mut(&self.target(index))?
                .select(pane, false)
        }
        pub(crate) fn toggle_zoom_in_window(
            &mut self,
            index: u32,
            pane: u32,
        ) -> Result<(), RmuxError> {
            self.store
                .window_layout_context_mut(&self.target(index))?
                .toggle_zoom(pane)
        }
        pub(crate) fn resize_active_window_geometry(
            &mut self,
            terminal: TerminalSize,
            content: TerminalSize,
        ) {
            self.store
                .window_switch_mut(&self.target(self.active_window_index()))
                .unwrap()
                .resize(terminal, content);
        }
        pub(crate) fn insert_window_with_initial_pane(
            &mut self,
            index: u32,
            size: TerminalSize,
        ) -> Result<rmux_core::PaneId, RmuxError> {
            self.store
                .insert_window_with_initial_pane(&self.name, index, size)
                .map(|created| created.pane_id)
        }
        pub(crate) fn remove_window_allowing_empty(
            &mut self,
            index: u32,
        ) -> Result<Option<Box<Window>>, RmuxError> {
            self.store
                .remove_window_occurrence_for_teardown(&self.target(index))
        }
        pub(crate) fn remove_window(
            &mut self,
            index: u32,
        ) -> Result<rmux_core::ManagedWindowRemoval, RmuxError> {
            self.store.remove_window(&self.target(index))
        }
        pub(crate) fn split_pane_in_window(
            &mut self,
            index: u32,
            pane: u32,
        ) -> Result<u32, RmuxError> {
            self.store
                .split_pane(
                    &self.target(index),
                    pane,
                    ManagedPaneSplit {
                        direction: SplitDirection::Vertical,
                        before: false,
                        detached: false,
                        full_size: false,
                    },
                )
                .map(|created| created.pane_index)
        }
        pub(crate) fn kill_pane(
            &mut self,
            pane: u32,
        ) -> Result<rmux_core::ManagedPaneRemoval, RmuxError> {
            self.store
                .kill_pane(&self.target(self.active_window_index()), pane)
        }
        pub(crate) fn swap_panes(
            &mut self,
            source: rmux_core::SessionPaneTarget,
            target: rmux_core::SessionPaneTarget,
            options: rmux_core::PaneSwapOptions,
        ) -> Result<rmux_core::ManagedPaneTransfer, RmuxError> {
            self.store.swap_panes(
                &self.target(source.window_index()),
                source.pane_index(),
                &self.target(target.window_index()),
                target.pane_index(),
                options,
            )
        }
        pub(crate) fn join_pane(
            &mut self,
            source: rmux_core::SessionPaneTarget,
            target: rmux_core::SessionPaneTarget,
            options: rmux_core::PaneJoinOptions,
        ) -> Result<rmux_core::ManagedPaneTransfer, RmuxError> {
            self.store.join_pane(
                &self.target(source.window_index()),
                source.pane_index(),
                &self.target(target.window_index()),
                target.pane_index(),
                options,
            )
        }
        pub(crate) fn resize_window(
            &mut self,
            index: u32,
            size: TerminalSize,
        ) -> Result<(), RmuxError> {
            let terminal_size = self.view().terminal_size();
            self.store
                .window_switch_mut(&self.target(index))?
                .resize(terminal_size, size);
            Ok(())
        }
        pub(crate) fn link_window_id(
            &mut self,
            index: u32,
            id: rmux_core::WindowId,
            kill: bool,
            select: bool,
        ) -> Result<(), RmuxError> {
            assert!(!kill, "fixture only adds vacant aliases");
            self.store.link_window(&self.target(index), id)?;
            if select {
                self.select_window(index)?;
            }
            Ok(())
        }
        pub(crate) fn split_before(
            &mut self,
            index: u32,
            pane: u32,
            direction: SplitDirection,
            before: bool,
            detached: bool,
        ) -> Result<u32, RmuxError> {
            self.store
                .split_pane(
                    &self.target(index),
                    pane,
                    ManagedPaneSplit {
                        direction,
                        before,
                        detached,
                        full_size: false,
                    },
                )
                .map(|created| created.pane_index)
        }
    };
}
// The owned and borrowed fixtures use different subsets of the same setup operations.
#[allow(dead_code)]
impl SessionFixture {
    setup_methods!();
}

pub(crate) struct SessionSetup<'a> {
    pub(crate) store: &'a mut ManagedSessionStore,
    name: SessionName,
}
#[allow(dead_code)]
impl SessionSetup<'_> {
    setup_methods!();
}
pub(crate) trait SessionSetupExt {
    fn session_setup(&mut self, name: &SessionName) -> Option<SessionSetup<'_>>;
}
impl SessionSetupExt for ManagedSessionStore {
    fn session_setup(&mut self, name: &SessionName) -> Option<SessionSetup<'_>> {
        self.contains_session(name).then(|| SessionSetup {
            store: self,
            name: name.clone(),
        })
    }
}

/// Fails a test whose `new-session` setup did not succeed, naming the response
/// that arrived instead so a red under load states its cause.
#[track_caller]
pub(crate) fn assert_new_session(response: &rmux_proto::Response) {
    assert!(
        matches!(response, rmux_proto::Response::NewSession(_)),
        "expected Response::NewSession, got {response:?}"
    );
}
