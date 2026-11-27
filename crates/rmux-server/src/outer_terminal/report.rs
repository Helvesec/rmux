//! Display the effective templates RMUX actually uses for an attached terminal.

use std::fmt::Write as _;

use super::OuterTerminal;

impl OuterTerminal {
    pub(crate) fn capability_report(&self) -> Vec<String> {
        // Keep this list limited to resolved capabilities. In particular, Ms
        // must use the actual clipboard template, including removal and empty
        // overrides; set-clipboard is a policy, not a terminal capability.
        let capabilities = [
            ("Cr", &self.cursor_colour_reset),
            ("Cs", &self.cursor_colour_template),
            ("Dsbp", &self.disable_bpaste),
            ("Dseks", &self.disable_extkeys),
            ("Dsfcs", &self.disable_focus),
            ("Dsmg", &self.disable_margins),
            ("Enbp", &self.enable_bpaste),
            ("Eneks", &self.enable_extkeys),
            ("Enfcs", &self.enable_focus),
            ("Enmg", &self.enable_margins),
            ("fsl", &self.title_close),
            ("Hls", &self.hyperlink_template),
            ("Ms", &self.clipboard_template),
            ("Se", &self.cursor_style_reset),
            ("Ss", &self.cursor_style_template),
            ("Swd", &self.path_open),
            ("Sync", &self.sync_template),
            ("tsl", &self.title_open),
        ];
        capabilities
            .into_iter()
            .enumerate()
            .map(|(index, (name, value))| match value {
                Some(value) => format!("{index:4}: {name}: (string) {}", escape_capability(value)),
                None => format!("{index:4}: {name}: [missing]"),
            })
            .collect()
    }
}

/// tmux's terminal dump uses visible C-style/octal escapes. Never send the
/// template's terminal control bytes to the client displaying the report.
fn escape_capability(value: &str) -> String {
    let mut escaped = String::new();
    for byte in value.bytes() {
        match byte {
            b'\x07' => escaped.push_str(r"\a"),
            b'\x08' => escaped.push_str(r"\b"),
            b'\t' => escaped.push_str(r"\t"),
            b'\n' => escaped.push_str(r"\n"),
            b'\x0b' => escaped.push_str(r"\v"),
            b'\x0c' => escaped.push_str(r"\f"),
            b'\r' => escaped.push_str(r"\r"),
            b'\\' => escaped.push_str(r"\\"),
            b' '..=b'~' => escaped.push(char::from(byte)),
            _ => write!(escaped, "\\{byte:03o}").expect("writing to String cannot fail"),
        }
    }
    escaped
}
