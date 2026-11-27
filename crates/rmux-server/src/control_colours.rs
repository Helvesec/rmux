//! Pane-scoped control reports and the live-control gate for OSC query replies.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, Weak,
};

mod report;
pub(crate) use report::parse_rgb;
pub(crate) use report::ColourReport;

/// A registration owns the presence; replacement/removal cannot retain it via a
/// pending command or pane. Closing clients cease contributing immediately.
#[derive(Debug)]
pub(crate) struct ControlColourPresence(Arc<AtomicBool>);

#[derive(Debug, Clone, Default)]
pub(crate) struct ControlColourClients(Arc<Mutex<Vec<Weak<ControlColourPresence>>>>);

impl ControlColourClients {
    pub(crate) fn register(&self, closing: Arc<AtomicBool>) -> Arc<ControlColourPresence> {
        let presence = Arc::new(ControlColourPresence(closing));
        let mut clients = self.0.lock().expect("control colour clients poisoned");
        clients.retain(|client| client.strong_count() != 0);
        clients.push(Arc::downgrade(&presence));
        presence
    }

    fn any_live(&self) -> bool {
        self.0
            .lock()
            .expect("control colour clients poisoned")
            .iter()
            .any(|client| {
                client
                    .upgrade()
                    .is_some_and(|client| !client.0.load(Ordering::SeqCst))
            })
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PaneControlColours {
    colours: [Option<[u8; 3]>; 2],
    clients: Option<ControlColourClients>,
}

impl PaneControlColours {
    pub(crate) fn apply(&mut self, report: ColourReport, clients: &ControlColourClients) {
        self.colours[report.slot] = Some(report.rgb);
        self.clients = Some(clients.clone());
    }

    pub(crate) fn query_colours(&self) -> [Option<[u8; 3]>; 2] {
        if self
            .clients
            .as_ref()
            .is_some_and(ControlColourClients::any_live)
        {
            self.colours
        } else {
            [None, None]
        }
    }
}

#[cfg(test)]
mod tests;
