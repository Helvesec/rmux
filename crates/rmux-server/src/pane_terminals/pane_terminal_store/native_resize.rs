use rmux_proto::{PaneTarget, RmuxError, SessionName, TerminalPixels, WindowTarget};

use super::{missing_session_terminals, PaneTerminalStore};
use crate::pane_terminal_lookup::SessionPane;
use crate::pane_terminal_process::pty_geometry_from_layout;

/// The supplied order and actual native map owner, not a runtime epoch lease.
#[derive(Debug)]
pub(crate) struct NativeResizePlan {
    pub(in crate::pane_terminals) runtime_session: SessionName,
    pub(in crate::pane_terminals) panes: Vec<SessionPane>,
    pub(in crate::pane_terminals) terminal_pixels: Option<TerminalPixels>,
    /// Prepared children not installed yet. Input-drain markers for already
    /// running children do not authorize a missing terminal.
    #[cfg(windows)]
    pub(in crate::pane_terminals) pending_panes: std::collections::HashSet<rmux_proto::PaneId>,
}

#[derive(Debug)]
pub(in crate::pane_terminals) enum NativeResizeStop {
    /// No call in this batch, including the existing test pre-loop failure.
    BeforeBatch(RmuxError),
    /// The entry at completed_prefix and all subsequent entries were not called.
    MissingPane,
    /// The failing call may have applied; only subsequent entries are unattempted.
    MayHaveApplied(rmux_pty::PtyError),
}

#[derive(Debug)]
pub(crate) struct NativeResizeFailure {
    pub(in crate::pane_terminals) plan: NativeResizePlan,
    /// Completed entries in this plan's order: native calls returning Ok or
    /// processless or pending panes requiring no native call. This is not OS agreement.
    pub(in crate::pane_terminals) completed_prefix: usize,
    pub(in crate::pane_terminals) stop: NativeResizeStop,
}

impl From<NativeResizeFailure> for RmuxError {
    fn from(failure: NativeResizeFailure) -> Self {
        match failure.stop {
            NativeResizeStop::BeforeBatch(error) => error,
            stop => {
                let pane = &failure.plan.panes[failure.completed_prefix];
                let target = PaneTarget::with_window(
                    failure.plan.runtime_session,
                    pane.window_index,
                    pane.index,
                );
                match stop {
                    NativeResizeStop::MissingPane => {
                        Self::Server(format!("missing pane terminal for {target}"))
                    }
                    NativeResizeStop::MayHaveApplied(error) => Self::Server(format!(
                        "failed to resize pane terminal for {target}: {error}"
                    )),
                    NativeResizeStop::BeforeBatch(_) => unreachable!(),
                }
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum WindowRuntimeResizeError {
    Preparation(RmuxError),
    Native(Box<NativeResizeFailure>),
}

impl From<RmuxError> for WindowRuntimeResizeError {
    fn from(error: RmuxError) -> Self {
        Self::Preparation(error)
    }
}

impl From<Box<NativeResizeFailure>> for WindowRuntimeResizeError {
    fn from(error: Box<NativeResizeFailure>) -> Self {
        Self::Native(error)
    }
}

impl From<WindowRuntimeResizeError> for RmuxError {
    fn from(error: WindowRuntimeResizeError) -> Self {
        match error {
            WindowRuntimeResizeError::Preparation(error) => error,
            WindowRuntimeResizeError::Native(error) => (*error).into(),
        }
    }
}

/// Snapshot restoration has occurred, but neither attempt promises OS rollback.
#[derive(Debug)]
pub(crate) struct WindowResizeCompensation {
    pub(in crate::pane_terminals) target: WindowTarget,
    pub(in crate::pane_terminals) original: WindowRuntimeResizeError,
    pub(in crate::pane_terminals) compensation: Result<NativeResizePlan, WindowRuntimeResizeError>,
}

#[derive(Debug)]
pub(crate) enum WindowMutationResizeError {
    BeforeResize(RmuxError),
    Compensation(Box<WindowResizeCompensation>),
}

impl From<RmuxError> for WindowMutationResizeError {
    fn from(error: RmuxError) -> Self {
        Self::BeforeResize(error)
    }
}

impl From<WindowMutationResizeError> for RmuxError {
    fn from(error: WindowMutationResizeError) -> Self {
        match error {
            WindowMutationResizeError::BeforeResize(error) => error,
            WindowMutationResizeError::Compensation(attempts) => {
                let original = RmuxError::from(attempts.original);
                match attempts.compensation {
                    Ok(_) => original,
                    Err(error) => Self::Server(format!(
                        "failed to roll back window runtime for {} after {original}: {}",
                        attempts.target,
                        RmuxError::from(error),
                    )),
                }
            }
        }
    }
}

impl PaneTerminalStore {
    #[cfg(test)]
    pub(in crate::pane_terminals) fn fail_resizes_for_test(&mut self, count: usize) {
        eprintln!("resize-test arm count={count}");
        self.fail_next_resizes = count;
    }

    pub(in crate::pane_terminals) fn resize_session(
        &mut self,
        plan: NativeResizePlan,
    ) -> Result<NativeResizePlan, Box<NativeResizeFailure>> {
        #[cfg(test)]
        eprintln!(
            "resize-test batch session={} panes={:?} armed={}",
            plan.runtime_session, plan.panes, self.fail_next_resizes
        );
        #[cfg(test)]
        if self.fail_next_resizes > 0 {
            self.fail_next_resizes -= 1;
            eprintln!("resize-test consume remaining={}", self.fail_next_resizes);
            return Err(Box::new(NativeResizeFailure {
                plan,
                completed_prefix: 0,
                stop: NativeResizeStop::BeforeBatch(RmuxError::Server(
                    "injected pane terminal resize failure".to_owned(),
                )),
            }));
        }

        let Some(session_panes) = self.sessions.get_mut(&plan.runtime_session) else {
            let error = missing_session_terminals(&plan.runtime_session);
            return Err(Box::new(NativeResizeFailure {
                plan,
                completed_prefix: 0,
                stop: NativeResizeStop::BeforeBatch(error),
            }));
        };

        for (completed_prefix, pane) in plan.panes.iter().enumerate() {
            let Some(terminal) = session_panes.get(&pane.id) else {
                #[cfg(windows)]
                if plan.pending_panes.contains(&pane.id) {
                    // Deferred installation applies the latest logical geometry.
                    // Keep this pane in the plan for transcript resizing below.
                    continue;
                }
                return Err(Box::new(NativeResizeFailure {
                    plan,
                    completed_prefix,
                    stop: NativeResizeStop::MissingPane,
                }));
            };

            // Empty panes still participate in transcript/layout resizing, but
            // there is no native call (or native resize receipt) for them.
            if terminal.is_empty() {
                continue;
            }

            let resize = || {
                terminal.resize(pty_geometry_from_layout(
                    pane.geometry,
                    pane.window_size,
                    plan.terminal_pixels,
                ))
            };
            // Counted where the call is really made, before any result is known.
            #[cfg(test)]
            {
                *self.resize_calls.entry(pane.id).or_default() += 1;
            }
            #[cfg(all(test, target_os = "macos"))]
            let resized = match self
                .resize_probe
                .as_mut()
                .and_then(|probe| probe.before_call(&plan, pane, terminal))
            {
                Some(error) => Err(error),
                None => resize(),
            };
            #[cfg(not(all(test, target_os = "macos")))]
            let resized = resize();
            #[cfg(all(test, target_os = "macos"))]
            if let Some(probe) = self.resize_probe.as_mut() {
                probe.after_call(terminal, resized.is_ok());
            }
            if let Err(error) = resized {
                return Err(Box::new(NativeResizeFailure {
                    plan,
                    completed_prefix,
                    stop: NativeResizeStop::MayHaveApplied(error),
                }));
            }
        }

        Ok(plan)
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "../native_resize_tests.rs"]
pub(in crate::pane_terminals) mod tests;
