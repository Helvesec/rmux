//! Theme policy shared by client admission and the ordered pane reply service.

use rmux_core::input::{ThemeAction, ThemeEvent};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Theme {
    #[default]
    Unknown,
    Dark,
    Light,
}

impl Theme {
    pub(crate) fn from_rgb([r, g, b]: [u8; 3]) -> Self {
        if u16::from(r) + u16::from(g) + u16::from(b) > 382 {
            Self::Light
        } else {
            Self::Dark
        }
    }

    fn reply(self) -> &'static [u8] {
        match self {
            Self::Unknown => b"",
            Self::Dark => b"\x1b[?997;1n",
            Self::Light => b"\x1b[?997;2n",
        }
    }
}

/// Facts owned by a single attach registration, never by its TTY name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ClientTheme {
    pub(crate) report: Theme,
    pub(crate) background: Option<[u8; 3]>,
}

/// The registration's closing flag remains authoritative between publications.
#[derive(Debug, Clone)]
pub(crate) struct ThemeClient {
    pub(crate) value: ClientTheme,
    pub(crate) closing: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionThemeClient {
    pub(crate) session: rmux_proto::SessionId,
    pub(crate) client: ThemeClient,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ThemeInputs {
    pub(crate) background: Option<[u8; 3]>,
    pub(crate) consensus: Theme,
}

impl ThemeInputs {
    /// Callers supply eligible registrations in stable creation order.
    pub(crate) fn from_clients(clients: impl Iterator<Item = ClientTheme>) -> Self {
        let mut result = Self::default();
        let (mut dark, mut light) = (false, false);
        for client in clients {
            result.background = result.background.or(client.background);
            dark |= client.report == Theme::Dark;
            light |= client.report == Theme::Light;
        }
        result.consensus = match (dark, light) {
            (true, false) => Theme::Dark,
            (false, true) => Theme::Light,
            _ => Theme::Unknown,
        };
        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThemeSource {
    Control,
    Application,
    OuterBackground,
    ClientConsensus,
}

#[derive(Debug, Default)]
pub(crate) struct PaneTheme {
    clients: Vec<ThemeClient>,
    inputs: ThemeInputs,
    control: Option<[u8; 3]>,
    application: Option<[u8; 3]>,
    subscribed: bool,
    last: Theme,
}

impl PaneTheme {
    pub(crate) fn refresh_clients(
        &mut self,
        clients: Vec<ThemeClient>,
        control: Option<[u8; 3]>,
    ) -> Vec<u8> {
        self.clients = clients;
        self.refresh(self.live_inputs(), control)
    }

    fn live_inputs(&self) -> ThemeInputs {
        ThemeInputs::from_clients(
            self.clients
                .iter()
                .filter(|client| !client.closing.load(Ordering::SeqCst))
                .map(|client| client.value),
        )
    }
    fn resolved(&self) -> (Theme, ThemeSource) {
        for (colour, source) in [
            (self.control, ThemeSource::Control),
            (self.application, ThemeSource::Application),
            (self.inputs.background, ThemeSource::OuterBackground),
        ] {
            if let Some(rgb) = colour {
                return (Theme::from_rgb(rgb), source);
            }
        }
        (self.inputs.consensus, ThemeSource::ClientConsensus)
    }

    fn notification(&mut self) -> &'static [u8] {
        if !self.subscribed {
            return b"";
        }
        let current = self.resolved().0;
        if current == self.last {
            return b"";
        }
        self.last = current;
        current.reply()
    }

    pub(crate) fn refresh(&mut self, inputs: ThemeInputs, control: Option<[u8; 3]>) -> Vec<u8> {
        if self.inputs == inputs && self.control == control {
            return Vec::new();
        }
        self.inputs = inputs;
        self.control = control;
        self.notification().to_vec()
    }

    pub(crate) fn apply(
        &mut self,
        ordinary: Vec<u8>,
        events: Vec<ThemeEvent>,
        control: Option<[u8; 3]>,
    ) -> Vec<u8> {
        self.inputs = self.live_inputs();
        self.control = control;
        let mut replies = Vec::with_capacity(ordinary.len());
        let mut copied = 0;
        for event in events {
            replies.extend_from_slice(&ordinary[copied..event.reply_offset]);
            copied = event.reply_offset;
            match event.action {
                ThemeAction::Query => {
                    self.last = self.resolved().0;
                    replies.extend_from_slice(self.last.reply());
                }
                ThemeAction::Subscribe(enabled) => {
                    self.subscribed = enabled;
                    self.last = self.resolved().0;
                }
                ThemeAction::Background(value) => {
                    self.application = value.as_deref().and_then(crate::control_colours::parse_rgb);
                }
            }
        }
        replies.extend_from_slice(&ordinary[copied..]);
        // A following query already observed the mutation. Never send a second
        // identical report merely because this feed also changed OSC 11.
        replies.extend_from_slice(self.notification());
        replies
    }
}

#[cfg(test)]
#[path = "theme/tests.rs"]
mod tests;
