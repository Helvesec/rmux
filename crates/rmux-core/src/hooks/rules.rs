use rmux_proto::{
    HookName, PaneTarget, RmuxError, ScopeSelector, SessionName, Target, WindowTarget,
};

use super::types::{HookClass, HookGlobalRoot};

/// Validates that a hook may be stored at the requested scope.
pub fn validate_hook_scope(hook: HookName, scope: &ScopeSelector) -> Result<(), RmuxError> {
    let _ = hook_class(hook);
    let _ = scope;
    Ok(())
}

/// Validates that rmux ships the requested hook and that it may be stored at
/// the requested scope.
pub fn validate_hook_registration(hook: HookName, scope: &ScopeSelector) -> Result<(), RmuxError> {
    if !hook_is_supported_for_registration(hook) {
        return Err(RmuxError::Message(format!(
            "{} is not supported: rmux does not dispatch this hook",
            hook_name(hook)
        )));
    }

    validate_hook_scope(hook, scope)
}

/// Resolves tmux's natural `-t` hook scope without enforcing storage
/// compatibility. Explicit `-w`/`-p` still chooses those scopes directly.
#[must_use]
pub fn hook_natural_scope_for_target(hook: HookName, target: Target) -> ScopeSelector {
    match target {
        Target::Session(session_name) => ScopeSelector::Session(session_name),
        Target::Window(target) => match hook_class(hook) {
            HookClass::Session => ScopeSelector::Session(target.session_name().clone()),
            HookClass::Window | HookClass::Pane => ScopeSelector::Window(target),
        },
        Target::Pane(target) => match hook_class(hook) {
            HookClass::Session => ScopeSelector::Session(target.session_name().clone()),
            HookClass::Window => ScopeSelector::Window(WindowTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
            )),
            HookClass::Pane => ScopeSelector::Pane(target),
        },
    }
}

/// Resolves tmux's natural `-t <session>` storage scope after the session's
/// current window and pane have been looked up by the server.
#[must_use]
pub fn hook_natural_scope_for_session_target(
    hook: HookName,
    session_name: SessionName,
    window_index: u32,
    pane_index: u32,
) -> ScopeSelector {
    match hook_class(hook) {
        HookClass::Session => ScopeSelector::Session(session_name),
        HookClass::Window => {
            ScopeSelector::Window(WindowTarget::with_window(session_name, window_index))
        }
        HookClass::Pane => ScopeSelector::Pane(PaneTarget::with_window(
            session_name,
            window_index,
            pane_index,
        )),
    }
}

pub(super) const fn hook_inventory() -> [HookName; 70] {
    [
        HookName::AfterBindKey,
        HookName::AfterCapturePane,
        HookName::AfterCopyMode,
        HookName::AfterDisplayMessage,
        HookName::AfterDisplayPanes,
        HookName::AfterKillPane,
        HookName::AfterListBuffers,
        HookName::AfterListClients,
        HookName::AfterListKeys,
        HookName::AfterListPanes,
        HookName::AfterListSessions,
        HookName::AfterListWindows,
        HookName::AfterLoadBuffer,
        HookName::AfterLockServer,
        HookName::AfterNewSession,
        HookName::AfterNewWindow,
        HookName::AfterPasteBuffer,
        HookName::AfterPipePane,
        HookName::AfterQueue,
        HookName::AfterRefreshClient,
        HookName::AfterRenameSession,
        HookName::AfterRenameWindow,
        HookName::AfterResizePane,
        HookName::AfterResizeWindow,
        HookName::AfterSaveBuffer,
        HookName::AfterSelectLayout,
        HookName::AfterSelectPane,
        HookName::AfterSelectWindow,
        HookName::AfterSendKeys,
        HookName::AfterSetBuffer,
        HookName::AfterSetEnvironment,
        HookName::AfterSetHook,
        HookName::AfterSetOption,
        HookName::AfterShowEnvironment,
        HookName::AfterShowMessages,
        HookName::AfterShowOptions,
        HookName::AfterSplitWindow,
        HookName::AfterUnbindKey,
        HookName::AlertActivity,
        HookName::AlertBell,
        HookName::AlertSilence,
        HookName::ClientActive,
        HookName::ClientAttached,
        HookName::ClientDetached,
        HookName::ClientFocusIn,
        HookName::ClientFocusOut,
        HookName::ClientResized,
        HookName::ClientSessionChanged,
        HookName::ClientLightTheme,
        HookName::ClientDarkTheme,
        HookName::CommandError,
        HookName::PaneDied,
        HookName::PaneExited,
        HookName::PaneFocusIn,
        HookName::PaneFocusOut,
        HookName::PaneModeChanged,
        HookName::PaneSetClipboard,
        HookName::PaneTitleChanged,
        HookName::SessionClosed,
        HookName::SessionCreated,
        HookName::SessionRenamed,
        HookName::SessionWindowChanged,
        HookName::WindowLayoutChanged,
        HookName::WindowLinked,
        HookName::WindowPaneChanged,
        HookName::WindowRenamed,
        HookName::WindowResized,
        HookName::WindowUnlinked,
        HookName::PasteBufferChanged,
        HookName::PasteBufferDeleted,
    ]
}

pub(super) const fn hook_class(hook: HookName) -> HookClass {
    match hook {
        HookName::WindowLayoutChanged
        | HookName::WindowPaneChanged
        | HookName::WindowRenamed
        | HookName::WindowResized => HookClass::Window,
        HookName::PaneDied
        | HookName::PaneExited
        | HookName::PaneFocusIn
        | HookName::PaneFocusOut
        | HookName::PaneModeChanged
        | HookName::PaneSetClipboard
        | HookName::PaneTitleChanged => HookClass::Pane,
        HookName::AfterBindKey
        | HookName::AfterCapturePane
        | HookName::AfterCopyMode
        | HookName::AfterDisplayMessage
        | HookName::AfterDisplayPanes
        | HookName::AfterKillPane
        | HookName::AfterListBuffers
        | HookName::AfterListClients
        | HookName::AfterListKeys
        | HookName::AfterListPanes
        | HookName::AfterListSessions
        | HookName::AfterListWindows
        | HookName::AfterLoadBuffer
        | HookName::AfterLockServer
        | HookName::AfterNewSession
        | HookName::AfterNewWindow
        | HookName::AfterPasteBuffer
        | HookName::AfterPipePane
        | HookName::AfterQueue
        | HookName::AfterRefreshClient
        | HookName::AfterRenameSession
        | HookName::AfterRenameWindow
        | HookName::AfterResizePane
        | HookName::AfterResizeWindow
        | HookName::AfterSaveBuffer
        | HookName::AfterSelectLayout
        | HookName::AfterSelectPane
        | HookName::AfterSelectWindow
        | HookName::AfterSendKeys
        | HookName::AfterSetBuffer
        | HookName::AfterSetEnvironment
        | HookName::AfterSetHook
        | HookName::AfterSetOption
        | HookName::AfterShowEnvironment
        | HookName::AfterShowMessages
        | HookName::AfterShowOptions
        | HookName::AfterSplitWindow
        | HookName::AfterUnbindKey
        | HookName::AlertActivity
        | HookName::AlertBell
        | HookName::AlertSilence
        | HookName::ClientActive
        | HookName::ClientAttached
        | HookName::ClientDetached
        | HookName::ClientFocusIn
        | HookName::ClientFocusOut
        | HookName::ClientResized
        | HookName::ClientSessionChanged
        | HookName::ClientLightTheme
        | HookName::ClientDarkTheme
        | HookName::CommandError
        | HookName::SessionCreated
        | HookName::SessionClosed
        | HookName::SessionRenamed
        | HookName::SessionWindowChanged
        | HookName::WindowLinked
        | HookName::WindowUnlinked
        | HookName::PasteBufferChanged
        | HookName::PasteBufferDeleted => HookClass::Session,
    }
}

pub(super) const fn root_for_hook(hook: HookName) -> HookGlobalRoot {
    match hook_class(hook) {
        HookClass::Session => HookGlobalRoot::Session,
        HookClass::Window | HookClass::Pane => HookGlobalRoot::Window,
    }
}

/// Returns the global hook root where tmux stores a hook.
#[must_use]
pub const fn hook_global_root(hook: HookName) -> HookGlobalRoot {
    root_for_hook(hook)
}

pub(super) const fn hook_is_visible_in_show_hooks(hook: HookName) -> bool {
    !matches!(
        hook,
        HookName::PasteBufferChanged | HookName::PasteBufferDeleted
    )
}

const fn hook_is_supported_for_registration(hook: HookName) -> bool {
    !matches!(
        hook,
        HookName::PasteBufferChanged | HookName::PasteBufferDeleted
    )
}

const fn hook_name(hook: HookName) -> &'static str {
    hook.as_str()
}
