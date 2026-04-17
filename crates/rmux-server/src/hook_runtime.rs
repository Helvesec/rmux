use std::cell::RefCell;
use std::future::Future;

use rmux_proto::{HookName, ScopeSelector, Target};

tokio::task_local! {
    static HOOKS_DISABLED: bool;
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
    pub(crate) format_mode: PendingInlineHookFormat,
}

pub(crate) fn hooks_disabled() -> bool {
    HOOKS_DISABLED
        .try_with(|disabled| *disabled)
        .unwrap_or(false)
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

pub(crate) async fn with_hook_execution<T, F>(formats: Vec<(String, String)>, future: F) -> T
where
    F: Future<Output = T>,
{
    HOOKS_DISABLED
        .scope(
            true,
            async move { HOOK_FORMATS.scope(formats, future).await },
        )
        .await
}
