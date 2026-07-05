//! Protocol option names and mutation modes.

use serde::{Deserialize, Serialize};

/// Supported option names for `set-option`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OptionName {
    /// The `status` option.
    Status,
    /// The `default-terminal` option.
    DefaultTerminal,
    /// The `terminal-features` option.
    TerminalFeatures,
    /// The `pane-border-style` option.
    PaneBorderStyle,
    /// The `pane-active-border-style` option.
    PaneActiveBorderStyle,
    /// The `buffer-limit` option.
    BufferLimit,
    /// The `base-index` option.
    BaseIndex,
    /// The `history-limit` option.
    HistoryLimit,
    /// The `display-panes-time` option.
    DisplayPanesTime,
    /// The `display-time` option.
    DisplayTime,
    /// The `status-left` option.
    StatusLeft,
    /// The `status-right` option.
    StatusRight,
    /// The `status-position` option.
    StatusPosition,
    /// The `status-style` option.
    StatusStyle,
    /// The `status-left-length` option.
    StatusLeftLength,
    /// The `status-right-length` option.
    StatusRightLength,
    /// The `status-left-style` option.
    StatusLeftStyle,
    /// The `status-right-style` option.
    StatusRightStyle,
    /// The `status-interval` option.
    StatusInterval,
    /// The `status-justify` option.
    StatusJustify,
    /// The `window-status-current-format` option.
    WindowStatusCurrentFormat,
    /// The `window-status-format` option.
    WindowStatusFormat,
    /// The `window-status-current-style` option.
    WindowStatusCurrentStyle,
    /// The `window-status-style` option.
    WindowStatusStyle,
    /// The `main-pane-width` option.
    MainPaneWidth,
    /// The `main-pane-height` option.
    MainPaneHeight,
    /// The `pane-base-index` option.
    PaneBaseIndex,
    /// The `mode-keys` option.
    ModeKeys,
    /// The `automatic-rename` option.
    AutomaticRename,
    /// The `window-style` option.
    WindowStyle,
    /// The `backspace` option.
    Backspace,
    /// The `command-alias` option.
    CommandAlias,
    /// The `codepoint-widths` option.
    CodepointWidths,
    /// The `copy-command` option.
    CopyCommand,
    /// The `cursor-colour` option.
    CursorColour,
    /// The `cursor-style` option.
    CursorStyle,
    /// The `default-client-command` option.
    DefaultClientCommand,
    /// The `editor` option.
    Editor,
    /// The `escape-time` option.
    EscapeTime,
    /// The `exit-empty` option.
    ExitEmpty,
    /// The `exit-unattached` option.
    ExitUnattached,
    /// The `extended-keys` option.
    ExtendedKeys,
    /// The `extended-keys-format` option.
    ExtendedKeysFormat,
    /// The `focus-events` option.
    FocusEvents,
    /// The `get-clipboard` option.
    GetClipboard,
    /// The `history-file` option.
    HistoryFile,
    /// The `input-buffer-size` option.
    InputBufferSize,
    /// The `menu-style` option.
    MenuStyle,
    /// The `menu-selected-style` option.
    MenuSelectedStyle,
    /// The `menu-border-style` option.
    MenuBorderStyle,
    /// The `menu-border-lines` option.
    MenuBorderLines,
    /// The `message-limit` option.
    MessageLimit,
    /// The `prefix-timeout` option.
    PrefixTimeout,
    /// The `prompt-history-limit` option.
    PromptHistoryLimit,
    /// The `set-clipboard` option.
    SetClipboard,
    /// The `terminal-overrides` option.
    TerminalOverrides,
    /// The `user-keys` option.
    UserKeys,
    /// The `variation-selector-always-wide` option.
    VariationSelectorAlwaysWide,
    /// The `activity-action` option.
    ActivityAction,
    /// The `assume-paste-time` option.
    AssumePasteTime,
    /// The `bell-action` option.
    BellAction,
    /// The `default-command` option.
    DefaultCommand,
    /// The `default-shell` option.
    DefaultShell,
    /// The `default-size` option.
    DefaultSize,
    /// The `destroy-unattached` option.
    DestroyUnattached,
    /// The `detach-on-destroy` option.
    DetachOnDestroy,
    /// The `display-panes-active-colour` option.
    DisplayPanesActiveColour,
    /// The `display-panes-colour` option.
    DisplayPanesColour,
    /// The `focus-follows-mouse` option.
    FocusFollowsMouse,
    /// The `initial-repeat-time` option.
    InitialRepeatTime,
    /// The `key-table` option.
    KeyTable,
    /// The `lock-after-time` option.
    LockAfterTime,
    /// The `lock-command` option.
    LockCommand,
    /// The `message-command-style` option.
    MessageCommandStyle,
    /// The `message-format` option.
    MessageFormat,
    /// The `message-line` option.
    MessageLine,
    /// The `message-style` option.
    MessageStyle,
    /// The `mouse` option.
    Mouse,
    /// The `prefix` option.
    Prefix,
    /// The `prefix2` option.
    Prefix2,
    /// The `renumber-windows` option.
    RenumberWindows,
    /// The `repeat-time` option.
    RepeatTime,
    /// The `set-titles` option.
    SetTitles,
    /// The `set-titles-string` option.
    SetTitlesString,
    /// The `silence-action` option.
    SilenceAction,
    /// The `status-bg` option.
    StatusBg,
    /// The `status-fg` option.
    StatusFg,
    /// The `status-format` option.
    StatusFormat,
    /// The `status-keys` option.
    StatusKeys,
    /// The `pane-status-current-style` option.
    PaneStatusCurrentStyle,
    /// The `pane-status-style` option.
    PaneStatusStyle,
    /// The `prompt-cursor-colour` option.
    PromptCursorColour,
    /// The `prompt-cursor-style` option.
    PromptCursorStyle,
    /// The `prompt-command-cursor-style` option.
    PromptCommandCursorStyle,
    /// The `session-status-current-style` option.
    SessionStatusCurrentStyle,
    /// The `session-status-style` option.
    SessionStatusStyle,
    /// The `update-environment` option.
    UpdateEnvironment,
    /// The `visual-activity` option.
    VisualActivity,
    /// The `visual-bell` option.
    VisualBell,
    /// The `visual-silence` option.
    VisualSilence,
    /// The `word-separators` option.
    WordSeparators,
    /// The `aggressive-resize` option.
    AggressiveResize,
    /// The `allow-passthrough` option.
    AllowPassthrough,
    /// The `allow-rename` option.
    AllowRename,
    /// The `allow-set-title` option.
    AllowSetTitle,
    /// The `alternate-screen` option.
    AlternateScreen,
    /// The `automatic-rename-format` option.
    AutomaticRenameFormat,
    /// The `clock-mode-colour` option.
    ClockModeColour,
    /// The `clock-mode-style` option.
    ClockModeStyle,
    /// The `copy-mode-match-style` option.
    CopyModeMatchStyle,
    /// The `copy-mode-current-match-style` option.
    CopyModeCurrentMatchStyle,
    /// The `copy-mode-mark-style` option.
    CopyModeMarkStyle,
    /// The `copy-mode-position-format` option.
    CopyModePositionFormat,
    /// The `copy-mode-position-style` option.
    CopyModePositionStyle,
    /// The `copy-mode-selection-style` option.
    CopyModeSelectionStyle,
    /// The `fill-character` option.
    FillCharacter,
    /// The `mode-style` option.
    ModeStyle,
    /// The `monitor-activity` option.
    MonitorActivity,
    /// The `monitor-bell` option.
    MonitorBell,
    /// The `monitor-silence` option.
    MonitorSilence,
    /// The `other-pane-height` option.
    OtherPaneHeight,
    /// The `other-pane-width` option.
    OtherPaneWidth,
    /// The `pane-border-format` option.
    PaneBorderFormat,
    /// The `pane-border-indicators` option.
    PaneBorderIndicators,
    /// The `pane-border-lines` option.
    PaneBorderLines,
    /// The `pane-border-status` option.
    PaneBorderStatus,
    /// The `pane-colours` option.
    PaneColours,
    /// The `pane-scrollbars` option.
    PaneScrollbars,
    /// The `pane-scrollbars-style` option.
    PaneScrollbarsStyle,
    /// The `pane-scrollbars-position` option.
    PaneScrollbarsPosition,
    /// The `popup-style` option.
    PopupStyle,
    /// The `popup-border-style` option.
    PopupBorderStyle,
    /// The `popup-border-lines` option.
    PopupBorderLines,
    /// The `remain-on-exit` option.
    RemainOnExit,
    /// The `remain-on-exit-format` option.
    RemainOnExitFormat,
    /// The `scroll-on-clear` option.
    ScrollOnClear,
    /// The `synchronize-panes` option.
    SynchronizePanes,
    /// The `tiled-layout-max-columns` option.
    TiledLayoutMaxColumns,
    /// The `window-active-style` option.
    WindowActiveStyle,
    /// The `window-size` option.
    WindowSize,
    /// The `window-status-activity-style` option.
    WindowStatusActivityStyle,
    /// The `window-status-bell-style` option.
    WindowStatusBellStyle,
    /// The `window-status-last-style` option.
    WindowStatusLastStyle,
    /// The `window-status-separator` option.
    WindowStatusSeparator,
    /// The `wrap-search` option.
    WrapSearch,
    /// The `xterm-keys` option.
    XtermKeys,
    /// The `copy-mode-line-numbers` option.
    CopyModeLineNumbers,
    /// The `copy-mode-line-number-style` option.
    CopyModeLineNumberStyle,
    /// The `copy-mode-current-line-number-style` option.
    CopyModeCurrentLineNumberStyle,
}

/// The mutation mode for `set-option`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetOptionMode {
    /// Replaces the effective option value.
    Replace,
    /// Appends to the effective option value.
    Append,
}
