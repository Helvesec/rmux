//! Ordered server-owned theme requests. The parser never chooses a client.

use super::{InputParser, OscColourSlot};

/// A semantic theme operation at an exact position among terminal replies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeEvent {
    /// Number of ordinary reply bytes preceding this operation.
    pub reply_offset: usize,
    /// Operation observed in the application's output stream.
    pub action: ThemeAction,
}

/// State changes and queries resolved by the pane's server owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeAction {
    /// Query the current theme, even without a subscription.
    Query,
    /// Set or reset the notification subscription.
    Subscribe(bool),
    /// Application OSC 11 value, or OSC 111 reset. Colour grammar belongs to R.
    Background(Option<String>),
}

impl InputParser {
    /// Returns an application-defined colour for an OSC 10/11/12 query.
    pub(crate) fn osc_colour(&self, slot: OscColourSlot) -> Option<&str> {
        self.osc_colours[slot.index()].as_deref()
    }

    /// Records an application-set OSC 10/11/12 colour so later queries
    /// round-trip the value.
    pub(crate) fn set_osc_colour(&mut self, slot: OscColourSlot, value: &str) {
        // A stored colour is reflected verbatim to every later query, so bound
        // it to a sane colour-spec length: otherwise a pane could set a huge
        // (up to input-buffer-sized) value and then flood queries in one read
        // batch, amplifying it into unbounded reply memory and OOM-ing the
        // daemon. A real X11 / `rgb:` / `#rrggbb` spec is far shorter; longer
        // values are not colours and are left unstored, so queries keep
        // answering the prior value or remain silent when the palette is
        // unknown.
        const OSC_COLOUR_MAX_LEN: usize = 64;
        if value.len() > OSC_COLOUR_MAX_LEN {
            return;
        }
        self.osc_colours[slot.index()] = Some(value.to_owned());
    }

    /// Resets an OSC colour back to unknown (OSC 110/111/112).
    pub(crate) fn reset_osc_colour(&mut self, slot: OscColourSlot) {
        self.osc_colours[slot.index()] = None;
    }

    pub(super) fn theme_event(&mut self, action: ThemeAction) {
        if !self.theme_events_enabled {
            return;
        }
        self.theme_events.push(ThemeEvent {
            reply_offset: self.reply_buf.len(),
            action,
        });
    }

    /// Enable server effects only for consumers that drain them after every feed.
    pub fn enable_theme_events(&mut self) {
        self.theme_events_enabled = true;
    }

    pub(crate) const fn theme_events_enabled(&self) -> bool {
        self.theme_events_enabled
    }

    /// Drain operations together with `take_replies`, before the next feed.
    pub fn take_theme_events(&mut self) -> Vec<ThemeEvent> {
        std::mem::take(&mut self.theme_events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Screen;
    use rmux_proto::TerminalSize;

    #[test]
    fn theme_events_capture_each_mutation_at_its_reply_position() {
        let stream = b"\x1b[c\x1b]11;#ffffff\x07\x1b[?996n\x1b[5n\x1b]11;#000000\x07\x1b[?996n\x1b[?2031h\x1b[?2031l";
        for split in 0..=stream.len() {
            let mut parser = InputParser::new();
            parser.enable_theme_events();
            let mut screen = Screen::new(TerminalSize { cols: 80, rows: 24 }, 100);
            parser.parse(&stream[..split], &mut screen);
            parser.parse(&stream[split..], &mut screen);
            let events = parser.take_theme_events();
            assert_eq!(
                events.iter().map(|e| e.reply_offset).collect::<Vec<_>>(),
                [7, 7, 11, 11, 11, 11]
            );
            assert_eq!(
                events.iter().map(|e| e.action.clone()).collect::<Vec<_>>(),
                [
                    ThemeAction::Background(Some("#ffffff".into())),
                    ThemeAction::Query,
                    ThemeAction::Background(Some("#000000".into())),
                    ThemeAction::Query,
                    ThemeAction::Subscribe(true),
                    ThemeAction::Subscribe(false),
                ]
            );
            assert_eq!(parser.take_replies(), b"\x1b[?1;2c\x1b[0n");
            assert!(parser.take_theme_events().is_empty());
        }
    }

    #[test]
    fn theme_events_are_opt_in_for_non_server_consumers() {
        let mut parser = InputParser::new();
        let mut screen = Screen::new(TerminalSize { cols: 80, rows: 24 }, 100);
        for _ in 0..100 {
            parser.parse(b"\x1b[?2031h\x1b[?996n\x1b]11;white\x07", &mut screen);
        }
        assert!(parser.take_theme_events().is_empty());
    }
}
