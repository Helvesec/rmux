use rmux_core::LifecycleEvent;

use crate::pane_io::AttachControl;

use super::*;

impl RequestHandler {
    pub(in crate::handler) async fn prune_stale_attached_clients_for_session(
        &self,
        session_name: &SessionName,
    ) -> Vec<u32> {
        let stale_clients = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .filter(|(_, active)| {
                    &active.session_name == session_name
                        && (active.control_tx.is_closed()
                            || active.control_backlog.load(Ordering::Acquire)
                                >= super::super::ATTACH_CONTROL_BACKLOG_LIMIT)
                })
                .map(|(pid, active)| active.identity(*pid))
                .collect::<Vec<_>>()
        };
        self.remove_attached_clients_for_session(session_name, stale_clients)
            .await
    }

    pub(in crate::handler) async fn remove_attached_clients_for_session(
        &self,
        session_name: &SessionName,
        attach_identities: Vec<super::super::ActiveAttachIdentity>,
    ) -> Vec<u32> {
        if attach_identities.is_empty() {
            return Vec::new();
        }
        let (removed, key_tables, overlays, focus_effects) = {
            let mut state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            let mut removed = Vec::new();
            let mut key_tables = Vec::new();
            let mut overlays = Vec::new();
            for identity in attach_identities {
                let pid = identity.attach_pid();
                if active_attach
                    .by_pid
                    .get(&pid)
                    .is_some_and(|active| identity.matches(pid, session_name, active))
                {
                    let mut active = active_attach
                        .remove_attached_client(pid)
                        .expect("attached client checked above");
                    let emit_detached = active.needs_detached_event();
                    let _ = active.control_tx.send(AttachControl::Detach);
                    active.closing.store(true, Ordering::SeqCst);
                    removed.push((pid, emit_detached.then_some(active.client_name)));
                    if let Some(table_name) = active.key_table_name.take() {
                        key_tables.push(table_name);
                    }
                    overlays.push(active.overlay.take());
                }
            }
            let focus_effects = (!removed.is_empty()).then(|| {
                super::super::super::focus_support::prepare_server_focus_effects(
                    &mut state,
                    &mut active_attach,
                )
            });
            (removed, key_tables, overlays, focus_effects)
        };
        if let Some(focus_effects) = focus_effects {
            self.bump_active_attach_epoch();
            let _ = focus_effects.execute(self).await;
        }

        for overlay in overlays {
            super::super::terminate_overlay_job(overlay);
        }
        if !key_tables.is_empty() {
            let mut state = self.state.lock().await;
            for table_name in key_tables {
                state.key_bindings.unref_table(&table_name);
            }
        }
        for client_name in removed.iter().filter_map(|(_, name)| name.as_ref()) {
            self.emit_without_attached_refresh(LifecycleEvent::ClientDetached {
                session_name: session_name.clone(),
                client_name: Some(client_name.clone()),
            })
            .await;
        }
        removed.into_iter().map(|(pid, _)| pid).collect()
    }
}
