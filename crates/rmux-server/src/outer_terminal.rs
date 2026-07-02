use std::collections::HashMap;

use rmux_core::{
    alternate_screen_enter_sequence, alternate_screen_exit_sequence, parse_colour, OptionStore,
    Session,
};
#[cfg(test)]
use rmux_core::{COLOUR_DEFAULT, COLOUR_FLAG_256, COLOUR_NONE, COLOUR_TERMINAL};
use rmux_proto::{ClientTerminalContext, OptionName};

use crate::pane_screen_state::PaneScreenState;

mod capabilities;
mod colours;
mod defaults;
mod features;
mod templates;

#[cfg(test)]
use capabilities::{decode_capability_string, parse_capability_override, split_override_segments};
#[cfg(test)]
use colours::colour_to_rgb;
use colours::colour_to_rgb_string;
#[cfg(test)]
use templates::sanitize_osc_payload;
use templates::{
    encode_base64, render_int_template, render_open_close, render_string_string_template,
    render_string_template, render_sync_template, sync_toggle,
};

const ATTACH_SCREEN_RESET_SEQUENCE: &[u8] = b"\x1b[0m\x1b[?25l\x1b[H\x1b[2J";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorScope {
    Pane,
    Prompt,
    CommandPrompt,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct OuterTerminalContext {
    term: Option<String>,
    colorterm: Option<String>,
    term_program: Option<String>,
    term_program_version: Option<String>,
    terminal_features: Vec<String>,
    utf8: bool,
}

impl OuterTerminalContext {
    pub(crate) fn from_environment(environment: Option<&HashMap<String, String>>) -> Self {
        let Some(environment) = environment else {
            return Self::default();
        };

        Self {
            term: copy_environment_value(environment, "TERM"),
            colorterm: copy_environment_value(environment, "COLORTERM"),
            term_program: copy_environment_value(environment, "TERM_PROGRAM"),
            term_program_version: copy_environment_value(environment, "TERM_PROGRAM_VERSION"),
            terminal_features: Vec::new(),
            utf8: false,
        }
    }

    pub(crate) fn with_client_terminal(mut self, client_terminal: &ClientTerminalContext) -> Self {
        self.terminal_features = client_terminal
            .terminal_features
            .iter()
            .flat_map(|value| value.split(','))
            .map(str::trim)
            .filter(|feature| !feature.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        self.utf8 = client_terminal.utf8;
        self
    }

    pub(crate) fn term_name(&self) -> &str {
        self.term.as_deref().unwrap_or("")
    }

    fn term_program(&self) -> &str {
        self.term_program.as_deref().unwrap_or("")
    }

    fn colorterm(&self) -> &str {
        self.colorterm.as_deref().unwrap_or("")
    }

    pub(crate) fn terminal_features(&self) -> &[String] {
        &self.terminal_features
    }

    pub(crate) fn explicit_features_string(&self) -> String {
        OuterTerminal::from_feature_names(self.terminal_features()).features_string()
    }

    pub(crate) fn utf8(&self) -> bool {
        self.utf8
    }

    #[cfg(test)]
    pub(crate) fn from_pairs(values: &[(&str, &str)]) -> Self {
        let environment = values
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect::<HashMap<_, _>>();
        Self::from_environment(Some(&environment))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct OuterTerminal {
    context: OuterTerminalContext,
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
    mouse_reporting_enabled: bool,
    ignore_function_keys: bool,
    no_bright: bool,
    bidi: bool,
    vt100_like: bool,
    focus_events_enabled: bool,
    extended_keys_enabled: bool,
    clipboard_writes_enabled: bool,
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
        if let Some(sequence) = &self.disable_bpaste {
            bytes.extend_from_slice(sequence.as_bytes());
        }
        bytes.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");
        bytes.extend_from_slice(alternate_screen_exit_sequence(self.context.term_name()));
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

    pub(crate) fn render_prelude(
        &self,
        session: &Session,
        options: &OptionStore,
        pane_state: Option<&PaneScreenState>,
        cursor_scope: CursorScope,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();

        if let Some(title) = pane_state
            .map(|state| state.title.as_str())
            .filter(|title| !title.is_empty())
        {
            bytes.extend_from_slice(self.render_title(title).as_bytes());
        }
        if let Some(path) = pane_state
            .map(|state| state.path.as_str())
            .filter(|path| !path.is_empty())
        {
            bytes.extend_from_slice(self.render_path(path).as_bytes());
        }

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

    pub(crate) fn resolve_cursor_style(
        &self,
        session: &Session,
        options: &OptionStore,
        pane_state: Option<&PaneScreenState>,
        cursor_scope: CursorScope,
    ) -> u32 {
        let session_name = session.name();
        let active_window = session.active_window_index();
        let active_pane = session.active_pane_index();
        match cursor_scope {
            CursorScope::Pane => {
                let pane_cursor = pane_state.map_or(0, |state| state.cursor_style);
                if pane_cursor != 0 {
                    pane_cursor
                } else {
                    parse_cursor_style_option(options.resolve_for_pane(
                        session_name,
                        active_window,
                        active_pane,
                        OptionName::CursorStyle,
                    ))
                }
            }
            CursorScope::Prompt => parse_cursor_style_option(
                options.resolve(Some(session_name), OptionName::PromptCursorStyle),
            ),
            CursorScope::CommandPrompt => parse_cursor_style_option(
                options.resolve(Some(session_name), OptionName::PromptCommandCursorStyle),
            ),
        }
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
        let rendered = render_string_string_template(template, "", &encoded);
        Some(rendered.into_bytes())
    }

    pub(crate) fn clipboard_passthrough_enabled(&self) -> bool {
        self.clipboard_writes_enabled && self.clipboard_template.is_some()
    }

    fn focus_sequence(&self) -> Option<String> {
        self.focus_events_enabled
            .then(|| self.enable_focus.clone())
            .flatten()
    }

    fn mouse_sequence(&self) -> Option<String> {
        // Enable in the same order tmux uses in tty_mouse_mode():
        //   ?1006h  SGR extended coordinates
        //   ?1000h  normal button tracking   ← must come before ?1002h
        //   ?1002h  button-event tracking
        //
        // Some terminals (e.g. Termius) require ?1000h to be set before
        // ?1002h; reversing the order causes them to fall back to sending
        // cursor keys (ESC [ A / ESC [ B) for scroll instead of SGR mouse
        // events, so WheelUpPane / WheelDownPane bindings never fire.
        (self.supports_mouse && self.mouse_reporting_enabled)
            .then_some("\x1b[?1006h\x1b[?1000h\x1b[?1002h".to_owned())
    }

    fn disable_mouse_sequence(&self) -> Option<String> {
        // Disable in reverse order.
        self.supports_mouse
            .then_some("\x1b[?1002l\x1b[?1000l\x1b[?1006l".to_owned())
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

    pub(crate) fn render_cursor_style_transition(
        &self,
        previous: Option<u32>,
        current: u32,
    ) -> Option<String> {
        if previous == Some(current) {
            return None;
        }
        if current == 0 {
            return previous
                .filter(|style| *style != 0)
                .and_then(|_| self.render_cursor_style_reset());
        }
        self.render_cursor_style(current)
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

fn copy_environment_value(environment: &HashMap<String, String>, name: &str) -> Option<String> {
    environment
        .get(name)
        .cloned()
        .filter(|value| !value.is_empty())
}

fn parse_cursor_style_option(value: Option<&str>) -> u32 {
    match value.unwrap_or("default") {
        "blinking-block" => 1,
        "block" => 2,
        "blinking-underline" => 3,
        "underline" => 4,
        "blinking-bar" => 5,
        "bar" => 6,
        _ => 0,
    }
}

#[cfg(test)]
mod tests;
