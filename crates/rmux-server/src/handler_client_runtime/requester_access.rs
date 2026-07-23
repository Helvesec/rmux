use rmux_os::identity::UserIdentity;

use crate::handler::attach_support::ClientFlags;
use crate::handler::control_support::{current_control_queue_identity, ControlClientIdentity};
use crate::handler::{DetachedRequesterAuthority, RequestHandler, RequesterOrigin};
use crate::server_access::{AccessMode, ServerAccessAdmission};

enum DetachedAdmissionLookup {
    Absent,
    Unambiguous(ServerAccessAdmission),
    DeniedOrAmbiguous,
}

impl RequestHandler {
    pub(in crate::handler) async fn capture_requester_origin(
        &self,
        requester_pid: u32,
    ) -> RequesterOrigin {
        RequesterOrigin::new(
            requester_pid,
            self.requester_detached_authority(requester_pid).await,
        )
    }

    pub(in crate::handler) async fn requester_can_write(&self, requester_pid: u32) -> bool {
        if let Some(identity) = current_control_queue_identity(requester_pid) {
            return self.control_queue_can_write(identity).await;
        }

        // A detached scope identifies the exact RPC admission. Resolve it
        // before PID-only attach/control fallbacks so a colliding local PID
        // cannot lend authority to this request.
        match self.detached_admission_lookup(requester_pid) {
            DetachedAdmissionLookup::Absent => {}
            DetachedAdmissionLookup::Unambiguous(admission) => {
                return self
                    .server_access
                    .lock()
                    .expect("server access mutex must not be poisoned")
                    .revalidate_detached_admission(&admission)
                    .is_some_and(AccessMode::can_write);
            }
            DetachedAdmissionLookup::DeniedOrAmbiguous => return false,
        }

        {
            let active_attach = self.active_attach.lock().await;
            if let Some(active) = active_attach.by_pid.get(&requester_pid) {
                return active.can_write && !active.flags.contains(ClientFlags::READONLY);
            }
        }

        let active_control = self.active_control.lock().await;
        if let Some(active) = active_control.by_pid.get(&requester_pid) {
            return active.can_write;
        }
        drop(active_control);

        requester_pid == std::process::id()
    }

    pub(in crate::handler) async fn requester_detached_authority(
        &self,
        requester_pid: u32,
    ) -> DetachedRequesterAuthority {
        if let Some(identity) = current_control_queue_identity(requester_pid) {
            return self
                .control_queue_access(identity)
                .await
                .and_then(|(user, can_write)| {
                    self.admission_for_identity_with_write_cap(&user, can_write)
                })
                .map_or(
                    DetachedRequesterAuthority::Denied,
                    DetachedRequesterAuthority::Admission,
                );
        }

        match self.detached_admission_lookup(requester_pid) {
            DetachedAdmissionLookup::Unambiguous(admission) => {
                return DetachedRequesterAuthority::Admission(admission);
            }
            DetachedAdmissionLookup::DeniedOrAmbiguous => {
                return DetachedRequesterAuthority::Denied;
            }
            DetachedAdmissionLookup::Absent => {}
        }

        let attach_access = {
            let active_attach = self.active_attach.lock().await;
            active_attach.by_pid.get(&requester_pid).map(|active| {
                (
                    active.user.clone(),
                    active.can_write && !active.flags.contains(ClientFlags::READONLY),
                )
            })
        };
        if let Some((user, can_write)) = attach_access {
            return self.authority_for_identity(&user, can_write);
        }

        let control_access = {
            let active_control = self.active_control.lock().await;
            active_control
                .by_pid
                .get(&requester_pid)
                .map(|active| (active.user.clone(), active.can_write))
        };
        if let Some((user, can_write)) = control_access {
            return self.authority_for_identity(&user, can_write);
        }

        if requester_pid == std::process::id() {
            return DetachedRequesterAuthority::Admission(
                self.server_access
                    .lock()
                    .expect("server access mutex must not be poisoned")
                    .owner_admission(),
            );
        }

        DetachedRequesterAuthority::Denied
    }

    fn detached_admission_lookup(&self, requester_pid: u32) -> DetachedAdmissionLookup {
        let detached_access = self
            .active_detached_requester_access
            .lock()
            .expect("active detached requester access mutex must not be poisoned");
        let Some(active) = detached_access.get(&requester_pid) else {
            return DetachedAdmissionLookup::Absent;
        };
        active.unambiguous_admission().cloned().map_or(
            DetachedAdmissionLookup::DeniedOrAmbiguous,
            DetachedAdmissionLookup::Unambiguous,
        )
    }

    fn authority_for_identity(
        &self,
        identity: &UserIdentity,
        can_write: bool,
    ) -> DetachedRequesterAuthority {
        self.admission_for_identity_with_write_cap(identity, can_write)
            .map_or(
                DetachedRequesterAuthority::Denied,
                DetachedRequesterAuthority::Admission,
            )
    }

    fn admission_for_identity_with_write_cap(
        &self,
        identity: &UserIdentity,
        can_write: bool,
    ) -> Option<ServerAccessAdmission> {
        self.server_access
            .lock()
            .ok()?
            .admission_for_identity_with_write_cap(identity, can_write)
    }

    async fn control_queue_access(
        &self,
        identity: ControlClientIdentity,
    ) -> Option<(UserIdentity, bool)> {
        let state = self.state.lock().await;
        let active_control = self.active_control.lock().await;
        Self::validate_control_queue_identity_locked(
            &state,
            &active_control,
            identity.requester_pid(),
            identity.control_id(),
        )
        .ok()?;
        active_control
            .by_pid
            .get(&identity.requester_pid())
            .map(|active| (active.user.clone(), active.can_write))
    }
}
