use rmux_core::{fnmatch, OptionStore};
use rmux_proto::{OptionName, SessionName};

use super::{
    capabilities::{apply_template_override, parse_capability_override, split_override_segments},
    defaults::{
        DEFAULT_CR, DEFAULT_CS, DEFAULT_DSBP, DEFAULT_DSEKS, DEFAULT_DSFCS, DEFAULT_DSMG,
        DEFAULT_ENBP, DEFAULT_ENEKS, DEFAULT_ENFCS, DEFAULT_ENMG, DEFAULT_FOOT_FEATURES,
        DEFAULT_FSL, DEFAULT_HLS, DEFAULT_ITERM2_FEATURES, DEFAULT_KITTY_FEATURES,
        DEFAULT_MINTTY_FEATURES, DEFAULT_MLTERM_FEATURES, DEFAULT_MS,
        DEFAULT_RXVT_UNICODE_FEATURES, DEFAULT_SE, DEFAULT_SS, DEFAULT_SWD, DEFAULT_SYNC,
        DEFAULT_TMUX_FEATURES, DEFAULT_TSL, DEFAULT_XTERM_FEATURES,
    },
    MouseTrackingMode, OuterTerminal, OuterTerminalContext,
};

impl OuterTerminal {
    pub(crate) fn resolve(options: &OptionStore, context: OuterTerminalContext) -> Self {
        Self::resolve_for_session(options, None, context)
    }

    pub(crate) fn resolve_for_session(
        options: &OptionStore,
        session_name: Option<&SessionName>,
        context: OuterTerminalContext,
    ) -> Self {
        let mut terminal = Self {
            context,
            focus_events_enabled: matches!(
                options.resolve(None, OptionName::FocusEvents),
                Some("on")
            ),
            extended_keys_enabled: matches!(
                options.resolve(None, OptionName::ExtendedKeys),
                Some("on" | "always")
            ),
            clipboard_writes_enabled: matches!(
                options.resolve(None, OptionName::SetClipboard),
                Some("on" | "external")
            ),
            app_clipboard_writes_enabled: matches!(
                options.resolve(None, OptionName::SetClipboard),
                Some("on")
            ),
            mouse_tracking_mode: match (
                options.resolve(session_name, OptionName::Mouse),
                options.resolve(session_name, OptionName::FocusFollowsMouse),
            ) {
                (Some("on"), Some("on")) => MouseTrackingMode::All,
                (Some("on"), _) => MouseTrackingMode::Button,
                _ => MouseTrackingMode::Off,
            },
            ..Self::default()
        };

        terminal.apply_default_family_features();
        terminal.apply_terminal_feature_options(options);
        terminal.apply_colorterm_overrides();
        terminal.apply_terminal_override_options(options);

        if terminal.xt_flag || looks_vt100_like(terminal.context.term_name()) {
            terminal.vt100_like = true;
            terminal.apply_feature_name("bpaste");
            terminal.apply_feature_name("focus");
            terminal.apply_feature_name("title");
        }

        if terminal.tc_flag {
            terminal.apply_feature_name("RGB");
        }

        // Re-apply overrides so explicit removals (e.g. Enbp@) win over
        // capabilities that XT/Tc just re-introduced via apply_feature_name.
        // Matches tmux's second tty_term_apply_overrides() pass.
        if terminal.xt_flag || terminal.tc_flag {
            terminal.apply_terminal_override_options(options);
        }

        terminal.apply_context_features();

        terminal
    }

    pub(super) fn from_feature_names(feature_names: &[String]) -> Self {
        let mut terminal = Self::default();
        for feature in feature_names {
            terminal.apply_feature_name(feature);
        }
        terminal
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn features_string(&self) -> String {
        let mut features = Vec::new();
        if self.supports_256 {
            features.push("256");
        }
        if self.enable_bpaste.is_some() && self.disable_bpaste.is_some() {
            features.push("bpaste");
        }
        if self.cursor_colour_template.is_some() && self.cursor_colour_reset.is_some() {
            features.push("ccolour");
        }
        if self.clipboard_template.is_some() {
            features.push("clipboard");
        }
        if self.hyperlink_template.is_some() {
            features.push("hyperlinks");
        }
        if self.supports_kitty_graphics {
            features.push("kitty-graphics");
        }
        if self.cursor_style_template.is_some() && self.cursor_style_reset.is_some() {
            features.push("cstyle");
        }
        if self.enable_extkeys.is_some() && self.disable_extkeys.is_some() {
            features.push("extkeys");
        }
        if self.enable_focus.is_some() && self.disable_focus.is_some() {
            features.push("focus");
        }
        if self.ignore_function_keys {
            features.push("ignorefkeys");
        }
        if self.supports_margins {
            features.push("margins");
        }
        if self.supports_mouse {
            features.push("mouse");
        }
        if self.path_open.is_some() && self.path_close.is_some() {
            features.push("osc7");
        }
        if self.supports_overline {
            features.push("overline");
        }
        if self.supports_rectfill {
            features.push("rectfill");
        }
        if self.supports_rgb {
            features.push("RGB");
        }
        if self.supports_sixel {
            features.push("sixel");
        }
        if self.supports_strikethrough {
            features.push("strikethrough");
        }
        if self.sync_template.is_some() {
            features.push("sync");
        }
        if self.title_open.is_some() && self.title_close.is_some() {
            features.push("title");
        }
        if self.supports_usstyle {
            features.push("usstyle");
        }
        features.join(",")
    }

    fn apply_default_family_features(&mut self) {
        let term = self.context.term_name().to_owned();
        let term_program = self.context.term_program().to_owned();

        if term.contains("mintty") || term_program.eq_ignore_ascii_case("mintty") {
            self.add_features(DEFAULT_MINTTY_FEATURES);
        }
        if term.starts_with("tmux") {
            self.add_features(DEFAULT_TMUX_FEATURES);
        }
        if term.starts_with("rxvt-unicode") {
            self.add_features(DEFAULT_RXVT_UNICODE_FEATURES);
        }
        if term_program.eq_ignore_ascii_case("iTerm.app") || term.starts_with("iterm") {
            self.add_features(DEFAULT_ITERM2_FEATURES);
        }
        if term.starts_with("foot") {
            self.add_features(DEFAULT_FOOT_FEATURES);
        }
        if term.starts_with("mlterm") || term_program.eq_ignore_ascii_case("mlterm") {
            self.add_features(DEFAULT_MLTERM_FEATURES);
        }
        if term.starts_with("xterm-kitty")
            || term.starts_with("xterm-ghostty")
            || term.starts_with("wezterm")
            || term_program.eq_ignore_ascii_case("ghostty")
            || term_program.eq_ignore_ascii_case("wezterm")
        {
            self.add_features(DEFAULT_KITTY_FEATURES);
        }
        if term.starts_with("wezterm") || term_program.eq_ignore_ascii_case("wezterm") {
            self.apply_feature_name("sixel");
        }
        if term.starts_with("xterm") {
            self.add_features(DEFAULT_XTERM_FEATURES);
        }
    }

    fn apply_terminal_feature_options(&mut self, options: &OptionStore) {
        let term = self.context.term_name().to_owned();
        if term.is_empty() {
            return;
        }
        for entry in options.resolve_array_values(None, OptionName::TerminalFeatures) {
            let mut segments = entry.split(':');
            let Some(pattern) = segments
                .next()
                .map(str::trim)
                .filter(|pattern| !pattern.is_empty())
            else {
                continue;
            };
            if !fnmatch(pattern, &term) {
                continue;
            }
            for feature in segments {
                self.apply_feature_name(feature.trim());
            }
        }
    }

    fn apply_colorterm_overrides(&mut self) {
        let colorterm = self.context.colorterm();
        if colorterm.eq_ignore_ascii_case("truecolor") || colorterm.eq_ignore_ascii_case("24bit") {
            self.apply_feature_name("RGB");
        } else if colorterm.contains("256") {
            self.apply_feature_name("256");
        }
    }

    fn apply_terminal_override_options(&mut self, options: &OptionStore) {
        let term = self.context.term_name().to_owned();
        if term.is_empty() {
            return;
        }
        for entry in options.resolve_array_values(None, OptionName::TerminalOverrides) {
            let segments = split_override_segments(&entry);
            let Some((pattern, overrides)) = segments.split_first() else {
                continue;
            };
            if pattern.is_empty() || !fnmatch(pattern, &term) {
                continue;
            }
            for override_spec in overrides {
                self.apply_capability_override(override_spec);
            }
        }
    }

    fn apply_context_features(&mut self) {
        let feature_names = self.context.terminal_features().to_vec();
        for feature in &feature_names {
            self.apply_feature_name(feature);
        }
    }

    fn apply_feature_name(&mut self, feature: &str) {
        match feature.to_ascii_lowercase().as_str() {
            "256" => self.supports_256 = true,
            "bpaste" => {
                self.enable_bpaste
                    .get_or_insert_with(|| DEFAULT_ENBP.to_owned());
                self.disable_bpaste
                    .get_or_insert_with(|| DEFAULT_DSBP.to_owned());
            }
            "ccolour" => {
                self.cursor_colour_template
                    .get_or_insert_with(|| DEFAULT_CS.to_owned());
                self.cursor_colour_reset
                    .get_or_insert_with(|| DEFAULT_CR.to_owned());
            }
            "clipboard" => {
                self.clipboard_template
                    .get_or_insert_with(|| DEFAULT_MS.to_owned());
            }
            "hyperlinks" => {
                self.supports_hyperlinks = true;
                self.hyperlink_template
                    .get_or_insert_with(|| DEFAULT_HLS.to_owned());
            }
            "cstyle" => {
                self.cursor_style_template
                    .get_or_insert_with(|| DEFAULT_SS.to_owned());
                self.cursor_style_reset
                    .get_or_insert_with(|| DEFAULT_SE.to_owned());
            }
            "extkeys" => {
                self.enable_extkeys
                    .get_or_insert_with(|| DEFAULT_ENEKS.to_owned());
                self.disable_extkeys
                    .get_or_insert_with(|| DEFAULT_DSEKS.to_owned());
            }
            "focus" => {
                self.enable_focus
                    .get_or_insert_with(|| DEFAULT_ENFCS.to_owned());
                self.disable_focus
                    .get_or_insert_with(|| DEFAULT_DSFCS.to_owned());
            }
            "ignorefkeys" => self.ignore_function_keys = true,
            "kitty-graphics" | "kitty_graphics" | "kgp" => self.supports_kitty_graphics = true,
            "margins" => {
                self.supports_margins = true;
                self.enable_margins
                    .get_or_insert_with(|| DEFAULT_ENMG.to_owned());
                self.disable_margins
                    .get_or_insert_with(|| DEFAULT_DSMG.to_owned());
            }
            "mouse" => self.supports_mouse = true,
            "osc7" => {
                self.path_open.get_or_insert_with(|| DEFAULT_SWD.to_owned());
                self.path_close
                    .get_or_insert_with(|| DEFAULT_FSL.to_owned());
            }
            "overline" => self.supports_overline = true,
            "rectfill" => self.supports_rectfill = true,
            "rgb" => {
                self.default_colours = true;
                self.supports_256 = true;
                self.supports_rgb = true;
            }
            "sixel" => self.supports_sixel = true,
            "strikethrough" => self.supports_strikethrough = true,
            "sync" => {
                self.sync_template
                    .get_or_insert_with(|| DEFAULT_SYNC.to_owned());
            }
            "title" => {
                self.title_open
                    .get_or_insert_with(|| DEFAULT_TSL.to_owned());
                self.title_close
                    .get_or_insert_with(|| DEFAULT_FSL.to_owned());
            }
            "usstyle" => self.supports_usstyle = true,
            _ => {}
        }
    }

    fn apply_capability_override(&mut self, spec: &str) {
        let Some((name, value, remove)) = parse_capability_override(spec) else {
            return;
        };
        match name.to_ascii_lowercase().as_str() {
            "ax" => self.default_colours = !remove,
            "bidi" => self.bidi = !remove,
            "cs" => apply_template_override(&mut self.cursor_colour_template, value, remove),
            "cr" => apply_template_override(&mut self.cursor_colour_reset, value, remove),
            "cmg" | "clmg" => self.supports_margins = !remove,
            "dsmg" => apply_template_override(&mut self.disable_margins, value, remove),
            "enmg" => {
                self.supports_margins = !remove;
                apply_template_override(&mut self.enable_margins, value, remove);
            }
            "dsbp" => apply_template_override(&mut self.disable_bpaste, value, remove),
            "enbp" => apply_template_override(&mut self.enable_bpaste, value, remove),
            "dseks" => apply_template_override(&mut self.disable_extkeys, value, remove),
            "eneks" => apply_template_override(&mut self.enable_extkeys, value, remove),
            "dsfcs" => apply_template_override(&mut self.disable_focus, value, remove),
            "enfcs" => apply_template_override(&mut self.enable_focus, value, remove),
            "hls" => {
                self.supports_hyperlinks = !remove;
                apply_template_override(&mut self.hyperlink_template, value, remove);
            }
            "nobr" => self.no_bright = !remove,
            "rect" => self.supports_rectfill = !remove,
            "smol" => self.supports_overline = !remove,
            "smulx" | "setulc" | "setulc1" | "ol" => self.supports_usstyle = !remove,
            "ss" => apply_template_override(&mut self.cursor_style_template, value, remove),
            "se" => apply_template_override(&mut self.cursor_style_reset, value, remove),
            "swd" => apply_template_override(&mut self.path_open, value, remove),
            "sxl" => self.supports_sixel = !remove,
            "sync" => apply_template_override(&mut self.sync_template, value, remove),
            "tc" => self.tc_flag = !remove,
            "ms" => apply_template_override(&mut self.clipboard_template, value, remove),
            "xt" => self.xt_flag = !remove,
            _ => {}
        }
    }

    fn add_features(&mut self, features: &[&str]) {
        for feature in features {
            self.apply_feature_name(feature);
        }
    }
}

fn looks_vt100_like(term: &str) -> bool {
    [
        "screen",
        "tmux",
        "xterm",
        "rxvt",
        "foot",
        "linux",
        "alacritty",
        "wezterm",
        "kitty",
        "st",
        "vte",
    ]
    .iter()
    .any(|prefix| term.starts_with(prefix))
}
