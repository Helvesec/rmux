//! Publish eligible client facts before hooks, on the existing mutation paths.

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::pane_terminal_lookup::pane_id_for_target;
use crate::theme::{SessionThemeClient, ThemeClient};

use super::RequestHandler;

#[cfg(test)]
#[path = "../handler_theme/tests.rs"]
mod tests;

impl RequestHandler {
    pub(in crate::handler) async fn refresh_theme_sources(&self) {
        let publication = self.active_attach.lock().await.theme_publication.clone();
        let _publication = publication.lock().await;
        let writes = {
            // Same state -> attach order as registration and refresh-client.
            let mut state = self.state.lock().await;
            let attached = self.active_attach.lock().await;
            let mut clients = attached
                .by_pid
                .values()
                .filter(|client| {
                    !client.closing.load(Ordering::SeqCst)
                        && !client.suspended
                        && client.terminal_context.allows_theme_reports()
                        && state
                            .sessions
                            .session(&client.session_name)
                            .is_some_and(|session| session.id() == client.session_id)
                })
                .collect::<Vec<_>>();
            clients.sort_by_key(|client| client.id);
            state.theme_clients = clients
                .iter()
                .map(|client| SessionThemeClient {
                    session: client.session_id,
                    client: ThemeClient {
                        value: client.theme,
                        closing: client.closing.clone(),
                    },
                })
                .collect();
            let mut seen = HashSet::new();
            let mut writes = Vec::new();
            for target in state.all_pane_targets() {
                let Ok(pane) = pane_id_for_target(
                    &state.sessions,
                    target.session_name(),
                    target.window_index(),
                    target.pane_index(),
                ) else {
                    continue;
                };
                if !seen.insert(pane) {
                    continue;
                }
                let inputs = state.theme_clients_for_pane(pane);
                let Ok(transcript) = state.transcript_handle(&target) else {
                    continue;
                };
                let bytes = {
                    // Every request refreshes every pane: one poisoned pane
                    // must not fail list-sessions or requests on other panes.
                    let Some(mut transcript) = transcript.lock_for_cross_pane_sweep() else {
                        continue;
                    };
                    let control = transcript.control_colours.query_colours()[1];
                    transcript.theme.refresh_clients(inputs, control)
                };
                if bytes.is_empty() {
                    continue;
                }
                #[cfg(test)]
                if state.append_pane_input_capture_for_test(&target, &bytes) {
                    continue;
                }
                let Ok(output) = state.pane_output_for_target(
                    target.session_name(),
                    target.window_index(),
                    target.pane_index(),
                ) else {
                    continue;
                };
                let generation = output.current_generation();
                let Ok(master) = state.clone_pane_master(
                    target.session_name(),
                    target.window_index(),
                    target.pane_index(),
                ) else {
                    continue;
                };
                writes.push((master, output, generation, bytes));
            }
            writes
        };
        // Each master is cloned from the exact runtime while resolving PaneId.
        // A replacement cannot redirect this descriptor to its new child.
        for (master, output, generation, bytes) in writes {
            let result = tokio::task::spawn_blocking(move || {
                if !output.accepts_generation(Some(generation)) {
                    return Ok(());
                }
                master.write_all_with_timeout(&bytes, Duration::from_secs(2))
            })
            .await;
            if !matches!(result, Ok(Ok(()))) {
                tracing::debug!(?result, "theme notification writer unavailable");
            }
        }
    }
}
