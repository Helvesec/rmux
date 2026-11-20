use std::collections::BTreeSet;

use rmux_core::{
    alternate_screen_enter_sequence, alternate_screen_exit_sequence, cursor_style_from_option,
    input::mode, parse_colour, OptionStore,
};
#[cfg(test)]
use rmux_core::{COLOUR_DEFAULT, COLOUR_FLAG_256, COLOUR_NONE, COLOUR_TERMINAL};
use rmux_proto::OptionName;

use crate::pane_screen_state::PaneScreenState;

mod capabilities;
mod client_title;
mod colours;
mod context;
mod defaults;
mod features;
mod report;
mod templates;

#[cfg(test)]
use capabilities::{decode_capability_string, parse_capability_override, split_override_segments};
pub(crate) use client_title::{
    ClientPathUpdate, ClientTitleState, ClientTitleUpdate, RenderedClientTitle,
};
#[cfg(test)]
use colours::colour_to_rgb;
use colours::colour_to_rgb_string;
pub(crate) use context::OuterTerminalContext;
pub(crate) use templates::encode_base64;
#[cfg(test)]
use templates::sanitize_osc_payload;
use templates::{
    render_clipboard_template, render_int_template, render_open_close, render_string_template,
    render_sync_template, sync_toggle,
};

const ATTACH_SCREEN_RESET_SEQUENCE: &[u8] = b"\x1b[0m\x1b[?25l\x1b[H\x1b[2J";
const THEME_REPORT_ENABLE_SEQUENCE: &[u8] = b"\x1b[?2031h";
const THEME_REPORT_DISABLE_SEQUENCE: &[u8] = b"\x1b[?2031l";
const THEME_REPORT_QUERY_SEQUENCE: &[u8] = b"\x1b[?996n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorScope {
    Pane,
    Prompt,
    CommandPrompt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
enum MouseTrackingMode {
    #[default]
    Off,
    Standard,
    Button,
    All,
}

impl MouseTrackingMode {
    fn from_pane_mode(mode_bits: u32) -> Self {
        if mode_bits & mode::MODE_MOUSE_ALL != 0 {
            Self::All
        } else if mode_bits & mode::MODE_MOUSE_BUTTON != 0 {
            Self::Button
        } else if mode_bits & mode::MODE_MOUSE_STANDARD != 0 {
            Self::Standard
        } else {
            Self::Off
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct OuterTerminal {
    context: OuterTerminalContext,
    reported_features: BTreeSet<String>,
    default_colours: bool,
    supports_256: bool,
    supports_rgb: bool,
    supports_hyperlinks: bool,
    supports_sixel: bool,
    supports_kitty_graphics: bool,
    supports_rectfill: bool,
    supports_margins: bool,
    supports_strikethrough: bool,
    supports_overline: bool,
    supports_usstyle: bool,
    supports_mouse: bool,
    mouse_tracking_mode: MouseTrackingMode,
    ignore_function_keys: bool,
    no_bright: bool,
    bidi: bool,
    vt100_like: bool,
    focus_events_enabled: bool,
    extended_keys_enabled: bool,
    // `set-clipboard on|external`: forwards tmux's own selections (copy-mode
    // yank, `set-buffer -w`) to the outer terminal.
    clipboard_writes_enabled: bool,
    // `set-clipboard on` only: relays an application's inbound OSC 52 to the
    // outer terminal. tmux gates this stricter than its own selections
    // (input.c input_osc_52 requires set-clipboard == 2), so untrusted pane
    // output cannot drive the system clipboard under the `external` default.
    app_clipboard_writes_enabled: bool,
    xt_flag: bool,
    tc_flag: bool,
    title_open: Option<String>,
    title_close: Option<String>,
    path_open: Option<String>,
    path_close: Option<String>,
    clipboard_template: Option<String>,
    hyperlink_template: Option<String>,
    cursor_style_template: Option<String>,
    cursor_style_reset: Option<String>,
    cursor_colour_template: Option<String>,
    cursor_colour_reset: Option<String>,
    enable_bpaste: Option<String>,
    disable_bpaste: Option<String>,
    enable_focus: Option<String>,
    disable_focus: Option<String>,
    enable_extkeys: Option<String>,
    disable_extkeys: Option<String>,
    enable_margins: Option<String>,
    disable_margins: Option<String>,
    sync_template: Option<String>,
}

impl OuterTerminal {
    pub(crate) fn has_bracketed_paste(&self) -> bool {
        self.enable_bpaste.is_some()
    }
    pub(crate) fn context(&self) -> &OuterTerminalContext {
        &self.context
    }

    pub(crate) const fn supports_kitty_graphics(&self) -> bool {
        self.supports_kitty_graphics
    }

    pub(crate) const fn supports_sixel(&self) -> bool {
        self.supports_sixel
    }

    pub(crate) fn attach_start_sequence(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(alternate_screen_enter_sequence(self.context.term_name()));
        bytes.extend_from_slice(ATTACH_SCREEN_RESET_SEQUENCE);
        bytes.extend_from_slice(THEME_REPORT_ENABLE_SEQUENCE);
        bytes.extend_from_slice(THEME_REPORT_QUERY_SEQUENCE);
        if let Some(sequence) = &self.enable_bpaste {
            bytes.extend_from_slice(sequence.as_bytes());
        }
        if let Some(sequence) = self.mouse_sequence().as_deref() {
            bytes.extend_from_slice(sequence.as_bytes());
        }
        if self.focus_events_enabled {
            if let Some(sequence) = &self.enable_focus {
                bytes.extend_from_slice(sequence.as_bytes());
            }
        }
        if self.extended_keys_enabled {
            if let Some(sequence) = &self.enable_extkeys {
                bytes.extend_from_slice(sequence.as_bytes());
            }
        }
        if self.supports_margins {
            if let Some(sequence) = &self.enable_margins {
                bytes.extend_from_slice(sequence.as_bytes());
            }
        }
        bytes
    }

    pub(crate) fn attach_stop_sequence(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        if let Some(sequence) = &self.cursor_colour_reset {
            bytes.extend_from_slice(sequence.as_bytes());
        }
        if let Some(sequence) = self.render_cursor_style_reset().as_deref() {
            bytes.extend_from_slice(sequence.as_bytes());
        }
        if self.focus_events_enabled {
            if let Some(sequence) = &self.disable_focus {
                bytes.extend_from_slice(sequence.as_bytes());
            }
        }
        if self.extended_keys_enabled {
            if let Some(sequence) = &self.disable_extkeys {
                bytes.extend_from_slice(sequence.as_bytes());
            }
        }
        if self.supports_margins {
            if let Some(sequence) = &self.disable_margins {
                bytes.extend_from_slice(sequence.as_bytes());
            }
        }
        if let Some(sequence) = self.disable_mouse_sequence().as_deref() {
            bytes.extend_from_slice(sequence.as_bytes());
        }
        bytes.extend_from_slice(THEME_REPORT_DISABLE_SEQUENCE);
        if let Some(sequence) = &self.disable_bpaste {
            bytes.extend_from_slice(sequence.as_bytes());
        }
        bytes.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");
        bytes.extend_from_slice(alternate_screen_exit_sequence(self.context.term_name()));
        // The shell-facing cursor must be visible after alternate-screen state is restored.
        bytes.extend_from_slice(b"\x1b[?25h");
        bytes
    }

    pub(crate) fn transition_sequence_from(&self, previous: &Self) -> Vec<u8> {
        let mut bytes = Vec::new();
        sync_toggle(
            &mut bytes,
            previous.enable_bpaste.as_ref(),
            self.enable_bpaste.as_ref(),
            previous.disable_bpaste.as_ref(),
        );
        sync_toggle(
            &mut bytes,
            previous.mouse_sequence().as_ref(),
            self.mouse_sequence().as_ref(),
            previous.disable_mouse_sequence().as_ref(),
        );
        sync_toggle(
            &mut bytes,
            previous.focus_sequence().as_ref(),
            self.focus_sequence().as_ref(),
            previous.disable_focus.as_ref(),
        );
        sync_toggle(
            &mut bytes,
            previous.extkeys_sequence().as_ref(),
            self.extkeys_sequence().as_ref(),
            previous.disable_extkeys.as_ref(),
        );
        sync_toggle(
            &mut bytes,
            previous.margin_sequence().as_ref(),
            self.margin_sequence().as_ref(),
            previous.disable_margins.as_ref(),
        );
        bytes
    }

    pub(crate) fn wrap_render_frame(&self, frame: &[u8]) -> Vec<u8> {
        if frame.is_empty() {
            return Vec::new();
        }

        let Some(start) = self.render_sync_sequence(1) else {
            return frame.to_vec();
        };
        let Some(end) = self.render_sync_sequence(2) else {
            return frame.to_vec();
        };

        let mut wrapped = Vec::with_capacity(start.len() + frame.len() + end.len());
        wrapped.extend_from_slice(start.as_bytes());
        wrapped.extend_from_slice(frame);
        wrapped.extend_from_slice(end.as_bytes());
        wrapped
    }

    /// The OSC 0 title and OSC 7 path this client's outer terminal still owes,
    /// gated on `set-titles` and deduplicated against what it already shows.
    pub(crate) fn render_client_title(&self, title: ClientTitleUpdate<'_>) -> Vec<u8> {
        let mut bytes = Vec::new();
        // The outer terminal's title and working directory are both driven from
        // tmux's `set-titles` gate, and the title carries the expanded
        // `set-titles-string` rather than the bare pane title (issue #182).
        if let Some(pending) = title.pending_title() {
            bytes.extend_from_slice(self.render_title(pending).as_bytes());
        }
        if let Some(pending) = title.pending_path() {
            bytes.extend_from_slice(self.render_path(pending).as_bytes());
        }
        bytes
    }

    /// What a client-title render leaves behind: the values this outer terminal
    /// now shows, and the OSC bytes the frame carries for them.
    ///
    /// A terminal that advertised no title template renders nothing, so its
    /// frame stays replaceable in the attach-control queue even while
    /// `set-titles` is on. The values are remembered either way, exactly as
    /// tmux assigns `c->title` before `tty_set_title()` decides it has no
    /// TSL/FSL to write with.
    pub(crate) fn rendered_client_title(
        &self,
        title: ClientTitleUpdate<'_>,
    ) -> Option<RenderedClientTitle> {
        title.rendered(self.render_client_title(title))
    }

    pub(crate) fn render_prelude(
        &self,
        session: rmux_core::ManagedSessionView<'_>,
        options: &OptionStore,
        cursor_scope: CursorScope,
        title: ClientTitleUpdate<'_>,
    ) -> Vec<u8> {
        let mut bytes = self.render_client_title(title);

        let session_name = session.name();
        let active_window = session.active_window_index();
        let active_pane = session.active_pane_index();
        let cursor_colour = match cursor_scope {
            CursorScope::Pane => options.resolve_for_pane(
                session_name,
                active_window,
                active_pane,
                OptionName::CursorColour,
            ),
            CursorScope::Prompt | CursorScope::CommandPrompt => {
                options.resolve(Some(session_name), OptionName::PromptCursorColour)
            }
        };
        if let Some(sequence) = self.render_cursor_colour(cursor_colour) {
            bytes.extend_from_slice(sequence.as_bytes());
        }

        bytes
    }

    pub(crate) fn resolve_cursor_style_state(
        &self,
        session: rmux_core::ManagedSessionView<'_>,
        options: &OptionStore,
        pane_state: Option<&PaneScreenState>,
        cursor_scope: CursorScope,
    ) -> rmux_core::ResolvedCursorStyle {
        let session_name = session.name();
        let active_window = session.active_window_index();
        let active_pane = session.active_pane_index();
        match cursor_scope {
            CursorScope::Pane => {
                let pane_cursor = pane_state.map_or(0, |state| state.cursor_style);
                let default_cursor = cursor_style_from_option(options.resolve_for_pane(
                    session_name,
                    active_window,
                    active_pane,
                    OptionName::CursorStyle,
                ));
                rmux_core::resolve_cursor_style_state(
                    pane_cursor,
                    pane_state.map_or(0, |state| state.mode),
                    default_cursor,
                )
            }
            CursorScope::Prompt => cursor_style_from_option(
                options.resolve(Some(session_name), OptionName::PromptCursorStyle),
            )
            .into(),
            CursorScope::CommandPrompt => cursor_style_from_option(
                options.resolve(Some(session_name), OptionName::PromptCommandCursorStyle),
            )
            .into(),
        }
    }

    #[cfg(test)]
    pub(crate) fn resolve_cursor_style(
        &self,
        session: rmux_core::ManagedSessionView<'_>,
        options: &OptionStore,
        pane_state: Option<&PaneScreenState>,
        cursor_scope: CursorScope,
    ) -> u32 {
        self.resolve_cursor_style_state(session, options, pane_state, cursor_scope)
            .style()
    }

    pub(crate) fn encode_forced_clipboard_set(&self, bytes: &[u8]) -> Option<Vec<u8>> {
        if bytes.is_empty() {
            return None;
        }
        self.encode_clipboard_payload(bytes)
    }

    pub(crate) fn encode_clipboard_set(&self, bytes: &[u8]) -> Option<Vec<u8>> {
        if bytes.is_empty() || !self.clipboard_writes_enabled {
            return None;
        }
        self.encode_clipboard_payload(bytes)
    }

    fn encode_clipboard_payload(&self, bytes: &[u8]) -> Option<Vec<u8>> {
        let template = self.clipboard_template.as_deref()?;
        let encoded = encode_base64(bytes);
        let rendered = render_clipboard_template(template, &encoded)?;
        Some(rendered.into_bytes())
    }

    pub(crate) fn clipboard_passthrough_enabled(&self) -> bool {
        self.app_clipboard_writes_enabled && self.clipboard_template.is_some()
    }

    fn focus_sequence(&self) -> Option<String> {
        self.focus_events_enabled
            .then(|| self.enable_focus.clone())
            .flatten()
    }

    /// Folds the active pane's exact mouse-tracking request into the outer
    /// mouse decision. The `mouse` option requires button tracking, while an
    /// application may request standard, button, or all-motion tracking. Keep
    /// the strongest requested mode so DECSET 1003 is not degraded to 1002.
    pub(crate) fn with_active_pane_mouse_mode(mut self, pane_mode_bits: u32) -> Self {
        self.mouse_tracking_mode = self
            .mouse_tracking_mode
            .max(MouseTrackingMode::from_pane_mode(pane_mode_bits));
        self
    }

    fn mouse_sequence(&self) -> Option<String> {
        if !self.supports_mouse {
            return None;
        }
        let sequence = match self.mouse_tracking_mode {
            MouseTrackingMode::Off => return None,
            MouseTrackingMode::Standard => "\x1b[?1006h\x1b[?1000h",
            // Some terminals require 1000 before the stronger tracking mode.
            MouseTrackingMode::Button => "\x1b[?1006h\x1b[?1000h\x1b[?1002h",
            MouseTrackingMode::All => "\x1b[?1006h\x1b[?1000h\x1b[?1003h",
        };
        Some(sequence.to_owned())
    }

    fn disable_mouse_sequence(&self) -> Option<String> {
        // Disable in reverse order.
        self.supports_mouse
            .then_some("\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l".to_owned())
    }

    fn extkeys_sequence(&self) -> Option<String> {
        self.extended_keys_enabled
            .then(|| self.enable_extkeys.clone())
            .flatten()
    }

    fn margin_sequence(&self) -> Option<String> {
        self.supports_margins
            .then(|| self.enable_margins.clone())
            .flatten()
    }

    fn render_title(&self, title: &str) -> String {
        render_open_close(&self.title_open, &self.title_close, title)
    }

    fn render_path(&self, path: &str) -> String {
        render_open_close(&self.path_open, &self.path_close, path)
    }

    fn render_cursor_style(&self, style: u32) -> Option<String> {
        if style == 0 {
            return self.render_cursor_style_reset();
        }
        let template = self.cursor_style_template.as_deref()?;
        Some(render_int_template(template, style))
    }

    #[cfg(test)]
    pub(crate) fn render_cursor_style_transition(
        &self,
        previous: Option<u32>,
        current: u32,
    ) -> Option<String> {
        self.render_cursor_state_transition(previous.map(Into::into), current.into())
    }

    pub(crate) fn render_cursor_state_transition(
        &self,
        previous: Option<rmux_core::ResolvedCursorStyle>,
        current: rmux_core::ResolvedCursorStyle,
    ) -> Option<String> {
        if previous == Some(current) {
            return None;
        }
        if current.style() != 0 {
            return self.render_cursor_style(current.style()).or_else(|| {
                previous
                    .is_some_and(|state| state.default_blink().is_some())
                    .then(|| "\x1b[0 q".to_owned())
            });
        }

        let mut sequence = String::new();
        let reset_needed = previous.is_some_and(|state| {
            state.style() != 0
                || (state.default_blink().is_some() && current.default_blink().is_none())
        });
        if reset_needed {
            if let Some(reset) = self.render_cursor_style_reset() {
                sequence.push_str(reset.as_str());
            } else {
                // DEC mode 12 is usable without the terminfo cursor-style
                // capabilities. DECSCUSR 0 is then the only shape-neutral way
                // to relinquish cursor presentation authority.
                sequence.push_str("\x1b[0 q");
            }
        }
        if let Some(blink) = current.default_blink() {
            sequence.push_str(if blink { "\x1b[?12h" } else { "\x1b[?12l" });
        }
        (!sequence.is_empty()).then_some(sequence)
    }

    fn render_cursor_style_reset(&self) -> Option<String> {
        self.cursor_style_reset.clone().or_else(|| {
            self.cursor_style_template
                .as_deref()
                .map(|template| render_int_template(template, 0))
        })
    }

    fn render_cursor_colour(&self, value: Option<&str>) -> Option<String> {
        let Some(value) = value.filter(|value| !value.is_empty()) else {
            return self.cursor_colour_reset.clone();
        };
        let Ok(colour) = parse_colour(value) else {
            return self.cursor_colour_reset.clone();
        };
        let rgb = colour_to_rgb_string(colour)?;
        Some(render_string_template(
            self.cursor_colour_template.as_deref()?,
            &rgb,
        ))
    }

    fn render_sync_sequence(&self, mode: u32) -> Option<String> {
        let template = self.sync_template.as_deref()?;
        Some(render_sync_template(template, mode))
    }
}

#[cfg(test)]
mod tests;
