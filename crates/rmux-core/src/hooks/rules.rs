use rmux_proto::{HookName, RmuxError, ScopeSelector};

use super::types::{HookClass, HookGlobalRoot};

/// Validates that a hook may be stored at the requested scope.
pub fn validate_hook_scope(hook: HookName, scope: &ScopeSelector) -> Result<(), RmuxError> {
    let is_valid = matches!(
        (hook_class(hook), scope),
        (
            HookClass::Session,
            ScopeSelector::Global | ScopeSelector::Session(_)
        ) | (
            HookClass::Window,
            ScopeSelector::Global | ScopeSelector::Window(_)
        ) | (
            HookClass::Pane,
            ScopeSelector::Global
                | ScopeSelector::Session(_)
                | ScopeSelector::Window(_)
                | ScopeSelector::Pane(_),
        )
    );

    if is_valid {
        Ok(())
    } else {
        Err(RmuxError::Server(format!(
            "{} does not support {} scope",
            hook_name(hook),
            scope_name(scope)
        )))
    }
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
        HookName::ClientLightTheme
            | HookName::ClientDarkTheme
            | HookName::CommandError
            | HookName::PasteBufferChanged
            | HookName::PasteBufferDeleted
    )
}

const fn hook_is_supported_for_registration(hook: HookName) -> bool {
    !matches!(
        hook,
        HookName::ClientLightTheme
            | HookName::ClientDarkTheme
            | HookName::CommandError
            | HookName::PasteBufferChanged
            | HookName::PasteBufferDeleted
    )
}

const fn hook_name(hook: HookName) -> &'static str {
    hook.as_str()
}

const fn scope_name(scope: &ScopeSelector) -> &'static str {
    match scope {
        ScopeSelector::Global => "global",
        ScopeSelector::Session(_) => "session",
        ScopeSelector::Window(_) => "window",
        ScopeSelector::Pane(_) => "pane",
    }
}
