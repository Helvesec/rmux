use super::variable::{FormatVariable, FORMAT_VARIABLES};

/// A semantic context in which a format variable can have a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormatVariableContext {
    /// Server-global state.
    Server,
    /// Session-local state.
    Session,
    /// Window-local state.
    Window,
    /// Pane-local state.
    Pane,
    /// Attached-client state.
    Client,
    /// Paste buffer state.
    Buffer,
    /// Mouse event state.
    Mouse,
    /// Copy-mode state.
    CopyMode,
    /// Loop-local state.
    Loop,
    /// Current command state.
    Command,
}

impl FormatVariableContext {
    const fn bit(self) -> u16 {
        match self {
            Self::Server => 1 << 0,
            Self::Session => 1 << 1,
            Self::Window => 1 << 2,
            Self::Pane => 1 << 3,
            Self::Client => 1 << 4,
            Self::Buffer => 1 << 5,
            Self::Mouse => 1 << 6,
            Self::CopyMode => 1 << 7,
            Self::Loop => 1 << 8,
            Self::Command => 1 << 9,
        }
    }
}

/// The set of semantic contexts attached to a format descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FormatVariableContexts(u16);

impl FormatVariableContexts {
    const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    /// Returns whether the set contains a context.
    #[must_use]
    pub const fn contains(self, context: FormatVariableContext) -> bool {
        self.0 & context.bit() != 0
    }

    /// Returns whether the descriptor has no declared context.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// The source class that owns a registered format variable value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormatVariableProvider {
    /// Supplied by the core [`FormatVariable`] API or explicit format context.
    Core,
    /// Computed by the server runtime format context.
    Runtime,
    /// Computed from build-time server capability state.
    BuildCapability,
    /// Resolved from a named option with existing option precedence.
    Option,
    /// Supplied by the current command context.
    Command,
    /// Known to the inventory but currently unresolved unless dynamic data wins.
    KnownEmpty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FormatVariableInventories(u8);

impl FormatVariableInventories {
    const NONE: Self = Self(0);
    const TMUX_FORMAT_TABLE: Self = Self(1 << 0);
    const DISPLAY_ALL_EXTRA: Self = Self(1 << 1);

    const fn contains(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

/// A registered format variable descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatVariableDescriptor {
    key: FormatVariableKey,
    name: &'static str,
    contexts: FormatVariableContexts,
    provider: FormatVariableProvider,
    inventories: FormatVariableInventories,
    format_variable: Option<FormatVariable>,
}

impl FormatVariableDescriptor {
    /// Returns the typed registry key.
    #[must_use]
    pub const fn key(self) -> FormatVariableKey {
        self.key
    }

    /// Returns the canonical format variable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Returns the semantic contexts where the variable can be populated.
    #[must_use]
    pub const fn contexts(self) -> FormatVariableContexts {
        self.contexts
    }

    /// Returns the provider class that owns this registered key.
    #[must_use]
    pub const fn provider(self) -> FormatVariableProvider {
        self.provider
    }

    /// Returns whether this descriptor is part of the tmux format table inventory.
    #[must_use]
    pub const fn is_tmux_table_entry(self) -> bool {
        self.inventories
            .contains(FormatVariableInventories::TMUX_FORMAT_TABLE)
    }

    /// Returns whether this descriptor is an extra emitted by `display-message -a`.
    #[must_use]
    pub const fn is_display_all_extra(self) -> bool {
        self.inventories
            .contains(FormatVariableInventories::DISPLAY_ALL_EXTRA)
    }

    /// Returns whether this descriptor is in tmux's time-variable inventory.
    #[must_use]
    pub fn is_time(self) -> bool {
        TMUX_TIME_FORMAT_VARIABLE_NAMES
            .binary_search(&self.name)
            .is_ok()
    }

    /// Returns whether absence resolves to the historical known-name empty value.
    #[must_use]
    pub const fn has_known_empty_fallback(self) -> bool {
        self.is_tmux_table_entry() || self.format_variable.is_some()
    }

    /// Returns the enum-backed API variable for descriptors that have one.
    #[must_use]
    pub const fn format_variable(self) -> Option<FormatVariable> {
        self.format_variable
    }
}

macro_rules! contexts {
    ($first:ident $(| $rest:ident)*) => {
        FormatVariableContexts::from_bits(
            FormatVariableContext::$first.bit() $(| FormatVariableContext::$rest.bit())*
        )
    };
}

macro_rules! format_registry {
    (
        time: [$($time_key:ident),+ $(,)?],
        tmux: [$($tmux_key:ident => ($tmux_name:literal, $tmux_contexts:expr, $tmux_provider:ident, $tmux_format_variable:expr);)+],
        extensions: [$($extension_key:ident => ($extension_name:literal, $extension_contexts:expr, $extension_provider:ident, $extension_format_variable:expr, $display_all_extra:expr);)*],
    ) => {
        /// A closed typed key for a registered format variable descriptor.
        #[allow(missing_docs)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum FormatVariableKey {
            $($tmux_key,)+
            $($extension_key,)*
        }

        impl FormatVariableKey {
            /// Returns the canonical name owned by this key's descriptor.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$tmux_key => $tmux_name,)+
                    $(Self::$extension_key => $extension_name,)*
                }
            }

            /// Looks up a typed registry key by canonical name.
            #[must_use]
            pub fn from_name(name: &str) -> Option<Self> {
                format_variable_descriptor(name).map(FormatVariableDescriptor::key)
            }
        }

        const FORMAT_VARIABLE_DESCRIPTORS: &[FormatVariableDescriptor] = &[
            $(FormatVariableDescriptor {
                key: FormatVariableKey::$tmux_key,
                name: $tmux_name,
                contexts: $tmux_contexts,
                provider: FormatVariableProvider::$tmux_provider,
                inventories: FormatVariableInventories::TMUX_FORMAT_TABLE,
                format_variable: $tmux_format_variable,
            },)+
            $(FormatVariableDescriptor {
                key: FormatVariableKey::$extension_key,
                name: $extension_name,
                contexts: $extension_contexts,
                provider: FormatVariableProvider::$extension_provider,
                inventories: if $display_all_extra {
                    FormatVariableInventories::DISPLAY_ALL_EXTRA
                } else {
                    FormatVariableInventories::NONE
                },
                format_variable: $extension_format_variable,
            },)*
        ];

        // `format_variable_descriptor` indexes the table by key discriminant.
        const _: () = {
            let mut position = 0;
            while position < FORMAT_VARIABLE_DESCRIPTORS.len() {
                assert!(FORMAT_VARIABLE_DESCRIPTORS[position].key as usize == position);
                position += 1;
            }
        };

        /// Looks up a registered format variable descriptor by canonical name.
        ///
        /// Every by-name format read resolves its name here, more than once,
        /// and each hook firing captures the whole tmux table at parse time.
        /// A literal `match` compiles to direct comparisons; scanning the
        /// descriptor table cost about 42 µs per hook firing.
        #[must_use]
        pub fn format_variable_descriptor(name: &str) -> Option<FormatVariableDescriptor> {
            let key = match name {
                $($tmux_name => FormatVariableKey::$tmux_key,)+
                $($extension_name => FormatVariableKey::$extension_key,)*
                _ => return None,
            };
            Some(FORMAT_VARIABLE_DESCRIPTORS[key as usize])
        }

        /// The frozen tmux `format_table[]` variable inventory from the reference source.
        pub const TMUX_FORMAT_TABLE_NAMES: [&str; 195] = [$($tmux_name,)+];

        /// Frozen tmux `FORMAT_TABLE_TIME` variable names.
        pub const TMUX_TIME_FORMAT_VARIABLE_NAMES: [&str; 9] = [
            $(FormatVariableKey::$time_key.name(),)+
        ];
    };
}

format_registry! {
    time: [
        BufferCreated, ClientActivity, ClientCreated, PaneDeadTime, SessionActivity,
        SessionCreated, SessionLastAttached, StartTime, WindowActivity,
    ],
    tmux: [
        ActiveWindowIndex => ("active_window_index", contexts!(Session | Window), Runtime, None);
        AlternateOn => ("alternate_on", contexts!(Pane), Runtime, None);
        AlternateSavedX => ("alternate_saved_x", contexts!(Pane), Runtime, None);
        AlternateSavedY => ("alternate_saved_y", contexts!(Pane), Runtime, None);
        BracketPasteFlag => ("bracket_paste_flag", contexts!(Pane), Runtime, None);
        BufferCreated => ("buffer_created", contexts!(Buffer), KnownEmpty, None);
        BufferFull => ("buffer_full", contexts!(Buffer), Runtime, None);
        BufferModeFormat => ("buffer_mode_format", contexts!(Buffer), Runtime, None);
        BufferName => ("buffer_name", contexts!(Buffer), Runtime, None);
        BufferSample => ("buffer_sample", contexts!(Buffer), Runtime, None);
        BufferSize => ("buffer_size", contexts!(Buffer), Runtime, None);
        ClientActivity => ("client_activity", contexts!(Client), KnownEmpty, None);
        ClientCellHeight => ("client_cell_height", contexts!(Client), KnownEmpty, None);
        ClientCellWidth => ("client_cell_width", contexts!(Client), KnownEmpty, None);
        ClientControlMode => ("client_control_mode", contexts!(Client), KnownEmpty, None);
        ClientCreated => ("client_created", contexts!(Client), KnownEmpty, None);
        ClientDiscarded => ("client_discarded", contexts!(Client), KnownEmpty, None);
        ClientFlags => ("client_flags", contexts!(Client), KnownEmpty, None);
        ClientHeight => ("client_height", contexts!(Client), Runtime, None);
        ClientKeyTable => ("client_key_table", contexts!(Client), KnownEmpty, None);
        ClientLastSession => ("client_last_session", contexts!(Client | Session), KnownEmpty, None);
        ClientModeFormat => ("client_mode_format", contexts!(Client), Runtime, None);
        ClientName => ("client_name", contexts!(Client), KnownEmpty, None);
        ClientPid => ("client_pid", contexts!(Client), KnownEmpty, None);
        ClientPrefix => ("client_prefix", contexts!(Client), KnownEmpty, None);
        ClientReadonly => ("client_readonly", contexts!(Client), KnownEmpty, None);
        ClientSession => ("client_session", contexts!(Client | Session), KnownEmpty, None);
        ClientTermfeatures => ("client_termfeatures", contexts!(Client), KnownEmpty, None);
        ClientTermname => ("client_termname", contexts!(Client), KnownEmpty, None);
        ClientTermtype => ("client_termtype", contexts!(Client), KnownEmpty, None);
        ClientTheme => ("client_theme", contexts!(Client), KnownEmpty, None);
        ClientTty => ("client_tty", contexts!(Client), KnownEmpty, None);
        ClientUid => ("client_uid", contexts!(Client), KnownEmpty, None);
        ClientUser => ("client_user", contexts!(Client), KnownEmpty, None);
        ClientUtf8 => ("client_utf8", contexts!(Client), KnownEmpty, None);
        ClientWidth => ("client_width", contexts!(Client), Runtime, None);
        ClientWritten => ("client_written", contexts!(Client), KnownEmpty, None);
        ConfigFiles => ("config_files", contexts!(Server), Runtime, None);
        CursorBlinking => ("cursor_blinking", contexts!(Pane), KnownEmpty, None);
        CursorCharacter => ("cursor_character", contexts!(Pane), Runtime, None);
        CursorColour => ("cursor_colour", contexts!(Pane), KnownEmpty, None);
        CursorFlag => ("cursor_flag", contexts!(Pane), Runtime, None);
        CursorShape => ("cursor_shape", contexts!(Pane), KnownEmpty, None);
        CursorVeryVisible => ("cursor_very_visible", contexts!(Pane), KnownEmpty, None);
        CursorX => ("cursor_x", contexts!(Pane), Runtime, None);
        CursorY => ("cursor_y", contexts!(Pane), Runtime, None);
        HistoryAllBytes => ("history_all_bytes", contexts!(Pane), Runtime, None);
        HistoryBytes => ("history_bytes", contexts!(Pane), Runtime, None);
        HistoryLimit => ("history_limit", contexts!(Pane), Runtime, None);
        HistorySize => ("history_size", contexts!(Pane), Runtime, None);
        Host => ("host", contexts!(Server), Runtime, None);
        HostShort => ("host_short", contexts!(Server), Runtime, None);
        InsertFlag => ("insert_flag", contexts!(Pane), Runtime, None);
        KeypadCursorFlag => ("keypad_cursor_flag", contexts!(Pane), Runtime, None);
        KeypadFlag => ("keypad_flag", contexts!(Pane), Runtime, None);
        LastWindowIndex => ("last_window_index", contexts!(Session | Window), Runtime, None);
        LoopLastFlag => ("loop_last_flag", contexts!(Loop), KnownEmpty, None);
        MouseAllFlag => ("mouse_all_flag", contexts!(Pane | Mouse), Runtime, None);
        MouseAnyFlag => ("mouse_any_flag", contexts!(Pane | Mouse), Runtime, None);
        MouseButtonFlag => ("mouse_button_flag", contexts!(Pane | Mouse), Runtime, None);
        MouseHyperlink => ("mouse_hyperlink", contexts!(Pane | Mouse), KnownEmpty, None);
        MouseLine => ("mouse_line", contexts!(Pane | Mouse), KnownEmpty, None);
        MousePane => ("mouse_pane", contexts!(Pane | Mouse), KnownEmpty, None);
        MouseSgrFlag => ("mouse_sgr_flag", contexts!(Pane | Mouse), Runtime, None);
        MouseStandardFlag => ("mouse_standard_flag", contexts!(Pane | Mouse), Runtime, None);
        MouseStatusLine => ("mouse_status_line", contexts!(Mouse), KnownEmpty, None);
        MouseStatusRange => ("mouse_status_range", contexts!(Mouse), KnownEmpty, None);
        MouseUtf8Flag => ("mouse_utf8_flag", contexts!(Pane | Mouse), Runtime, None);
        MouseWord => ("mouse_word", contexts!(Pane | Mouse), KnownEmpty, None);
        MouseX => ("mouse_x", contexts!(Pane | Mouse), KnownEmpty, None);
        MouseY => ("mouse_y", contexts!(Pane | Mouse), KnownEmpty, None);
        NextSessionId => ("next_session_id", contexts!(Server | Session), Runtime, None);
        OriginFlag => ("origin_flag", contexts!(Pane), Runtime, None);
        PaneActive => ("pane_active", contexts!(Pane), Core, Some(FormatVariable::PaneActive));
        PaneAtBottom => ("pane_at_bottom", contexts!(Pane | Window), Runtime, None);
        PaneAtLeft => ("pane_at_left", contexts!(Pane | Window), Runtime, None);
        PaneAtRight => ("pane_at_right", contexts!(Pane | Window), Runtime, None);
        PaneAtTop => ("pane_at_top", contexts!(Pane | Window), Runtime, None);
        PaneBg => ("pane_bg", contexts!(Pane | Window), Runtime, None);
        PaneBottom => ("pane_bottom", contexts!(Pane | Window), Runtime, None);
        PaneCurrentCommand => ("pane_current_command", contexts!(Pane), Runtime, None);
        PaneCurrentPath => ("pane_current_path", contexts!(Pane), Runtime, None);
        PaneDead => ("pane_dead", contexts!(Pane), Runtime, None);
        PaneDeadSignal => ("pane_dead_signal", contexts!(Pane), Runtime, None);
        PaneDeadStatus => ("pane_dead_status", contexts!(Pane), Runtime, None);
        PaneDeadTime => ("pane_dead_time", contexts!(Pane), Runtime, None);
        PaneFg => ("pane_fg", contexts!(Pane | Window), Runtime, None);
        PaneFlags => ("pane_flags", contexts!(Pane), Runtime, None);
        PaneFloatingFlag => ("pane_floating_flag", contexts!(Pane), Runtime, None);
        PaneFormat => ("pane_format", contexts!(Pane), Runtime, None);
        PaneHeight => ("pane_height", contexts!(Pane), Runtime, Some(FormatVariable::PaneHeight));
        PaneId => ("pane_id", contexts!(Pane), Core, Some(FormatVariable::PaneId));
        PaneInMode => ("pane_in_mode", contexts!(Pane | CopyMode), Runtime, None);
        PaneIndex => ("pane_index", contexts!(Pane), Runtime, Some(FormatVariable::PaneIndex));
        PaneInputOff => ("pane_input_off", contexts!(Pane), Runtime, None);
        PaneKeyMode => ("pane_key_mode", contexts!(Pane), KnownEmpty, None);
        PaneLast => ("pane_last", contexts!(Pane | Window), Runtime, None);
        PaneLeft => ("pane_left", contexts!(Pane | Window), Runtime, None);
        PaneMarked => ("pane_marked", contexts!(Pane), Runtime, None);
        PaneMarkedSet => ("pane_marked_set", contexts!(Pane), Runtime, None);
        PaneMode => ("pane_mode", contexts!(Pane | CopyMode), Runtime, None);
        PanePath => ("pane_path", contexts!(Pane), Runtime, None);
        PanePbProgress => ("pane_pb_progress", contexts!(Pane), KnownEmpty, None);
        PanePbState => ("pane_pb_state", contexts!(Pane), KnownEmpty, None);
        PanePid => ("pane_pid", contexts!(Pane), Runtime, None);
        PanePipe => ("pane_pipe", contexts!(Pane), Runtime, None);
        PanePipePid => ("pane_pipe_pid", contexts!(Pane), KnownEmpty, None);
        PaneRight => ("pane_right", contexts!(Pane | Window), Runtime, None);
        PaneSearchString => ("pane_search_string", contexts!(Pane | CopyMode), Runtime, None);
        PaneStartCommand => ("pane_start_command", contexts!(Pane), Runtime, None);
        PaneStartPath => ("pane_start_path", contexts!(Pane), Runtime, None);
        PaneSynchronized => ("pane_synchronized", contexts!(Pane | Window), Runtime, None);
        PaneTabs => ("pane_tabs", contexts!(Pane), Runtime, None);
        PaneTitle => ("pane_title", contexts!(Pane), Runtime, None);
        PaneTop => ("pane_top", contexts!(Pane | Window), Runtime, None);
        PaneTty => ("pane_tty", contexts!(Pane), Runtime, None);
        PaneUnseenChanges => ("pane_unseen_changes", contexts!(Pane), Runtime, None);
        PaneWidth => ("pane_width", contexts!(Pane), Runtime, Some(FormatVariable::PaneWidth));
        PaneX => ("pane_x", contexts!(Pane | Window), Runtime, None);
        PaneY => ("pane_y", contexts!(Pane | Window), Runtime, None);
        PaneZ => ("pane_z", contexts!(Pane | Window), Runtime, None);
        PaneZoomedFlag => ("pane_zoomed_flag", contexts!(Pane | Window), Runtime, None);
        Pid => ("pid", contexts!(Server), Runtime, None);
        ScrollRegionLower => ("scroll_region_lower", contexts!(Pane), Runtime, None);
        ScrollRegionUpper => ("scroll_region_upper", contexts!(Pane), Runtime, None);
        ServerSessions => ("server_sessions", contexts!(Server | Session), Runtime, None);
        SessionActive => ("session_active", contexts!(Session), Runtime, None);
        SessionActivity => ("session_activity", contexts!(Session), Runtime, None);
        SessionActivityFlag => ("session_activity_flag", contexts!(Session), Runtime, None);
        SessionAlert => ("session_alert", contexts!(Session), Runtime, None);
        SessionAlerts => ("session_alerts", contexts!(Session), Runtime, None);
        SessionAttached => ("session_attached", contexts!(Session), Core, Some(FormatVariable::SessionAttached));
        SessionAttachedList => ("session_attached_list", contexts!(Session | Client), KnownEmpty, None);
        SessionBellFlag => ("session_bell_flag", contexts!(Session), Runtime, None);
        SessionCreated => ("session_created", contexts!(Session), Runtime, None);
        SessionFormat => ("session_format", contexts!(Session), Runtime, None);
        SessionGroup => ("session_group", contexts!(Session), Runtime, None);
        SessionGroupAttached => ("session_group_attached", contexts!(Session | Client), Runtime, None);
        SessionGroupAttachedList => ("session_group_attached_list", contexts!(Session | Client), Runtime, None);
        SessionGroupList => ("session_group_list", contexts!(Session), Runtime, None);
        SessionGroupManyAttached => ("session_group_many_attached", contexts!(Session | Client), Runtime, None);
        SessionGroupSize => ("session_group_size", contexts!(Session), Runtime, None);
        SessionGrouped => ("session_grouped", contexts!(Session), Runtime, None);
        SessionId => ("session_id", contexts!(Session), Runtime, None);
        SessionLastAttached => ("session_last_attached", contexts!(Session | Client), Runtime, None);
        SessionManyAttached => ("session_many_attached", contexts!(Session | Client), Runtime, None);
        SessionMarked => ("session_marked", contexts!(Session | Pane), Runtime, None);
        SessionName => ("session_name", contexts!(Session), Core, Some(FormatVariable::SessionName));
        SessionPath => ("session_path", contexts!(Session), Runtime, None);
        SessionSilenceFlag => ("session_silence_flag", contexts!(Session), Runtime, None);
        SessionStack => ("session_stack", contexts!(Session | Window), Runtime, None);
        SessionWindows => ("session_windows", contexts!(Session), Core, Some(FormatVariable::SessionWindows));
        SixelSupport => ("sixel_support", contexts!(Server), BuildCapability, None);
        SocketPath => ("socket_path", contexts!(Server), Runtime, None);
        StartTime => ("start_time", contexts!(Server), Runtime, None);
        SynchronizedOutputFlag => ("synchronized_output_flag", contexts!(Pane), KnownEmpty, None);
        TreeModeFormat => ("tree_mode_format", contexts!(Server | Session | Window | Pane), Runtime, None);
        Uid => ("uid", contexts!(Server), Runtime, None);
        User => ("user", contexts!(Server), Runtime, None);
        Version => ("version", contexts!(Server), Runtime, None);
        WindowActive => ("window_active", contexts!(Window), Core, Some(FormatVariable::WindowActive));
        WindowActiveClients => ("window_active_clients", contexts!(Window | Client), Runtime, None);
        WindowActiveClientsList => ("window_active_clients_list", contexts!(Window | Client), Runtime, None);
        WindowActiveSessions => ("window_active_sessions", contexts!(Window | Session), Runtime, None);
        WindowActiveSessionsList => ("window_active_sessions_list", contexts!(Window | Session), Runtime, None);
        WindowActivity => ("window_activity", contexts!(Window), Runtime, None);
        WindowActivityFlag => ("window_activity_flag", contexts!(Window), Runtime, None);
        WindowBellFlag => ("window_bell_flag", contexts!(Window), Runtime, None);
        WindowBigger => ("window_bigger", contexts!(Window), Runtime, None);
        WindowCellHeight => ("window_cell_height", contexts!(Window), Runtime, None);
        WindowCellWidth => ("window_cell_width", contexts!(Window), Runtime, None);
        WindowEndFlag => ("window_end_flag", contexts!(Window | Session), Runtime, None);
        WindowFlags => ("window_flags", contexts!(Window), Runtime, None);
        WindowFormat => ("window_format", contexts!(Window), Runtime, None);
        WindowHeight => ("window_height", contexts!(Window), Runtime, Some(FormatVariable::WindowHeight));
        WindowId => ("window_id", contexts!(Window), Core, Some(FormatVariable::WindowId));
        WindowIndex => ("window_index", contexts!(Window), Core, Some(FormatVariable::WindowIndex));
        WindowLastFlag => ("window_last_flag", contexts!(Window), Core, Some(FormatVariable::WindowLastFlag));
        WindowLayout => ("window_layout", contexts!(Window), Runtime, Some(FormatVariable::WindowLayout));
        WindowLinked => ("window_linked", contexts!(Window | Session), Runtime, None);
        WindowLinkedSessions => ("window_linked_sessions", contexts!(Window | Session), Runtime, None);
        WindowLinkedSessionsList => ("window_linked_sessions_list", contexts!(Window | Session), Runtime, None);
        WindowMarkedFlag => ("window_marked_flag", contexts!(Window | Pane), Runtime, None);
        WindowName => ("window_name", contexts!(Window), Runtime, Some(FormatVariable::WindowName));
        WindowOffsetX => ("window_offset_x", contexts!(Window), Runtime, None);
        WindowOffsetY => ("window_offset_y", contexts!(Window), Runtime, None);
        WindowPanes => ("window_panes", contexts!(Window | Pane), Core, Some(FormatVariable::WindowPanes));
        WindowRawFlags => ("window_raw_flags", contexts!(Window), Runtime, Some(FormatVariable::WindowRawFlags));
        WindowSilenceFlag => ("window_silence_flag", contexts!(Window), Runtime, None);
        WindowStackIndex => ("window_stack_index", contexts!(Window | Session), Runtime, None);
        WindowStartFlag => ("window_start_flag", contexts!(Window | Session), Runtime, None);
        WindowVisibleLayout => ("window_visible_layout", contexts!(Window), Runtime, None);
        WindowWidth => ("window_width", contexts!(Window), Runtime, Some(FormatVariable::WindowWidth));
        WindowZoomedFlag => ("window_zoomed_flag", contexts!(Window), Runtime, None);
        WrapFlag => ("wrap_flag", contexts!(Pane), Runtime, None);
    ],
    extensions: [
        Command => ("command", contexts!(Command), Command, None, true);
        CopyCursorHyperlink => ("copy_cursor_hyperlink", contexts!(Pane | CopyMode), Runtime, None, false);
        CopyCursorLine => ("copy_cursor_line", contexts!(Pane | CopyMode), Runtime, None, false);
        CopyCursorWord => ("copy_cursor_word", contexts!(Pane | CopyMode), Runtime, None, false);
        CopyCursorX => ("copy_cursor_x", contexts!(Pane | CopyMode), Runtime, None, false);
        CopyCursorY => ("copy_cursor_y", contexts!(Pane | CopyMode), Runtime, None, false);
        CopyPosition => ("copy_position", contexts!(Pane | CopyMode), Runtime, None, false);
        CopyPositionLimit => ("copy_position_limit", contexts!(Pane | CopyMode), Runtime, None, false);
        DefaultShell => ("default-shell", contexts!(Server | Session), Option, None, false);
        PaneActivity => ("pane_activity", contexts!(Pane), Runtime, None, false);
        PaneCreated => ("pane_created", contexts!(Pane), Runtime, None, false);
        PaneGeneration => ("pane_generation", contexts!(Pane), Runtime, None, false);
        PaneLifecycleGeneration => ("pane_lifecycle_generation", contexts!(Pane), Runtime, None, false);
        PaneLifecycleRevision => ("pane_lifecycle_revision", contexts!(Pane), Runtime, None, false);
        PaneOutputSequence => ("pane_output_sequence", contexts!(Pane), Runtime, None, false);
        PaneProgramStatus => ("pane_program_status", contexts!(Pane), Runtime, None, false);
        PaneProgramStatusApp => ("pane_program_status_app", contexts!(Pane), Runtime, None, false);
        PaneProgramStatusId => ("pane_program_status_id", contexts!(Pane), Runtime, None, false);
        PaneProgramStatusJson => ("pane_program_status_json", contexts!(Pane), Runtime, None, false);
        PaneProgramStatusKind => ("pane_program_status_kind", contexts!(Pane), Runtime, None, false);
        PaneProgramStatusMessage => ("pane_program_status_message", contexts!(Pane), Runtime, None, false);
        PaneProgramStatusProgress => ("pane_program_status_progress", contexts!(Pane), Runtime, None, false);
        PaneProgramStatusTitle => ("pane_program_status_title", contexts!(Pane), Runtime, None, false);
        PaneRevision => ("pane_revision", contexts!(Pane), Runtime, None, false);
        RectangleToggle => ("rectangle_toggle", contexts!(Pane | CopyMode), Runtime, None, false);
        ScrollPosition => ("scroll_position", contexts!(Pane | CopyMode), Runtime, None, false);
        SearchCount => ("search_count", contexts!(Pane | CopyMode), Runtime, None, false);
        SearchCountPartial => ("search_count_partial", contexts!(Pane | CopyMode), Runtime, None, false);
        SearchMatch => ("search_match", contexts!(Pane | CopyMode), Runtime, None, false);
        SearchPresent => ("search_present", contexts!(Pane | CopyMode), Runtime, None, false);
        SearchTimedOut => ("search_timed_out", contexts!(Pane | CopyMode), Runtime, None, false);
        SelectionActive => ("selection_active", contexts!(Pane | CopyMode), Runtime, None, false);
        SelectionEndX => ("selection_end_x", contexts!(Pane | CopyMode), Runtime, None, false);
        SelectionEndY => ("selection_end_y", contexts!(Pane | CopyMode), Runtime, None, false);
        SelectionMode => ("selection_mode", contexts!(Pane | CopyMode), Runtime, None, false);
        SelectionPresent => ("selection_present", contexts!(Pane | CopyMode), Runtime, None, false);
        SelectionStartX => ("selection_start_x", contexts!(Pane | CopyMode), Runtime, None, false);
        SelectionStartY => ("selection_start_y", contexts!(Pane | CopyMode), Runtime, None, false);
        SessionHeight => ("session_height", contexts!(Session), Runtime, Some(FormatVariable::SessionHeight), false);
        SessionProgramStatusJson => ("session_program_status_json", contexts!(Session), Runtime, None, false);
        SessionWidth => ("session_width", contexts!(Session), Runtime, Some(FormatVariable::SessionWidth), false);
        TopLineTime => ("top_line_time", contexts!(Pane | CopyMode), Runtime, None, false);
        WindowCreated => ("window_created", contexts!(Window), Runtime, None, false);
    ],
}

/// Returns every registered descriptor.
pub fn format_variable_descriptors() -> impl Iterator<Item = FormatVariableDescriptor> {
    FORMAT_VARIABLE_DESCRIPTORS.iter().copied()
}

/// Returns descriptors that belong to the frozen tmux format table inventory.
pub fn tmux_format_variable_descriptors() -> impl Iterator<Item = FormatVariableDescriptor> {
    format_variable_descriptors().filter(|descriptor| descriptor.is_tmux_table_entry())
}

/// Returns descriptors included in `display-message -a`, in output order.
pub fn display_all_format_variable_descriptors() -> impl Iterator<Item = FormatVariableDescriptor> {
    tmux_format_variable_descriptors()
        .chain(format_variable_descriptors().filter(|descriptor| descriptor.is_display_all_extra()))
}

/// Returns descriptors captured for parse-time `%if` and command expansion.
///
/// The output intentionally preserves the previous capture order: enum-backed
/// API variables first, followed by the frozen tmux table inventory. Duplicate
/// names therefore keep the existing parser precedence because later entries
/// still override earlier entries in the parser's parse-time format context.
pub fn parse_time_format_variable_descriptors() -> impl Iterator<Item = FormatVariableDescriptor> {
    FORMAT_VARIABLES
        .into_iter()
        .filter_map(|variable| format_variable_descriptor(variable.name()))
        .chain(tmux_format_variable_descriptors())
}

/// Returns descriptors that belong to tmux's time-variable inventory.
pub fn tmux_time_format_variable_descriptors() -> impl Iterator<Item = FormatVariableDescriptor> {
    TMUX_TIME_FORMAT_VARIABLE_NAMES
        .into_iter()
        .filter_map(format_variable_descriptor)
}

/// Returns whether a variable name is part of the registered format inventory.
#[must_use]
pub fn is_known_format_variable_name(name: &str) -> bool {
    format_variable_descriptor(name).is_some()
}
