use std::cell::RefCell;
use std::future::Future;

use rmux_core::{PaneId, WindowId};
use rmux_proto::{HookName, ScopeSelector, SessionId, Target};

use crate::pane_terminals::WindowLinkOccurrenceId;

tokio::task_local! {
    static HOOK_EXECUTION: HookExecutionContext;
}

tokio::task_local! {
    static HOOK_FORMATS: Vec<(String, String)>;
}

tokio::task_local! {
    static PENDING_INLINE_HOOKS: RefCell<Vec<PendingInlineHook>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingInlineHookFormat {
    HookOnly,
    AfterCommand,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingInlineHook {
    pub(crate) hook: HookName,
    pub(crate) scope: ScopeSelector,
    pub(crate) current_target: Option<Target>,
    pub(crate) exact_pane_target: Option<ExactPaneHookTarget>,
    pub(crate) exact_session_id: Option<SessionId>,
    pub(crate) skip_dispatch: bool,
    pub(crate) format_mode: PendingInlineHookFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExactPaneHookTarget {
    pub(crate) session_id: SessionId,
    pub(crate) window_id: WindowId,
    pub(crate) pane_id: PaneId,
    pub(crate) preferred_window_index: u32,
    pub(crate) window_occurrence_id: Option<WindowLinkOccurrenceId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookExecutionKind {
    /// An inline, after-command, or command-error hook.
    ///
    /// tmux may deliver lifecycle notifications deferred by this command once
    /// the command queue advances.
    Command,
    /// A lifecycle notification hook.
    ///
    /// Commands run by lifecycle hooks use tmux's no-hooks state, so they must
    /// not enqueue another lifecycle generation.
    Lifecycle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HookExecutionContext {
    hook: HookName,
    kind: HookExecutionKind,
}

impl HookExecutionContext {
    pub(crate) const fn command(hook: HookName) -> Self {
        Self {
            hook,
            kind: HookExecutionKind::Command,
        }
    }

    pub(crate) const fn lifecycle(hook: HookName) -> Self {
        Self {
            hook,
            kind: HookExecutionKind::Lifecycle,
        }
    }

    pub(crate) const fn hook(self) -> HookName {
        self.hook
    }

    fn allows_lifecycle_hook(self) -> bool {
        matches!(self.kind, HookExecutionKind::Command)
    }
}

pub(crate) fn hooks_disabled() -> bool {
    HOOK_EXECUTION.try_with(|_| ()).is_ok()
}

pub(crate) fn lifecycle_hooks_disabled() -> bool {
    // A command hook may release one lifecycle generation. Lifecycle contexts
    // bound the chain after that first generation, including same-hook events.
    HOOK_EXECUTION
        .try_with(|execution| !execution.allows_lifecycle_hook())
        .unwrap_or(false)
}

pub(crate) fn current_hook_execution() -> Option<HookExecutionContext> {
    HOOK_EXECUTION.try_with(|execution| *execution).ok()
}

pub(crate) fn current_hook_format_value(name: &str) -> Option<String> {
    HOOK_FORMATS
        .try_with(|formats| {
            formats
                .iter()
                .rev()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, value)| value.clone())
        })
        .ok()
        .flatten()
}

pub(crate) fn current_hook_formats() -> Vec<(String, String)> {
    HOOK_FORMATS.try_with(Clone::clone).unwrap_or_default()
}

pub(crate) fn queue_inline_hook(hook: PendingInlineHook) {
    if hooks_disabled() {
        return;
    }
    let _ = PENDING_INLINE_HOOKS.try_with(|pending| pending.borrow_mut().push(hook));
}

pub(crate) async fn capture_inline_hooks<T, F>(future: F) -> (T, Vec<PendingInlineHook>)
where
    F: Future<Output = T>,
{
    PENDING_INLINE_HOOKS
        .scope(RefCell::new(Vec::new()), async {
            let output = future.await;
            let hooks =
                PENDING_INLINE_HOOKS.with(|pending| std::mem::take(&mut *pending.borrow_mut()));
            (output, hooks)
        })
        .await
}

pub(crate) async fn with_hook_execution<T, F>(
    execution: HookExecutionContext,
    formats: Vec<(String, String)>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    HOOK_EXECUTION
        .scope(execution, async move {
            HOOK_FORMATS.scope(formats, future).await
        })
        .await
}

pub(crate) async fn with_optional_hook_execution<T, F>(
    execution: Option<HookExecutionContext>,
    formats: Vec<(String, String)>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    match execution {
        Some(execution) => with_hook_execution(execution, formats, future).await,
        None => future.await,
    }
}
