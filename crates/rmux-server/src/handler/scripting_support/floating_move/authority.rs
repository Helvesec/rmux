use rmux_os::identity::UserIdentity;
use rmux_proto::RmuxError;

use crate::handler::attach_support::{ActiveAttachIdentity, ClientFlags};
use crate::handler::control_support::{current_control_queue_identity, ControlClientIdentity};
use crate::handler::{DetachedRequesterAccess, RequestHandler};
use crate::server_access::{AccessMode, ServerAccessAdmission};

/// The exact source that admitted this floating move.
///
/// A PID-only boolean cannot survive an await: a replacement client could lend
/// authority to work admitted for its predecessor. Each variant retains both
/// the live-client generation, when one exists, and the access-store admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) enum FloatingMoveWriteAuthority {
    ControlQueue {
        identity: ControlClientIdentity,
        admission: ServerAccessAdmission,
    },
    Detached {
        requester_pid: u32,
        admission: ServerAccessAdmission,
    },
    Attach {
        identity: ActiveAttachIdentity,
        admission: ServerAccessAdmission,
    },
    Control {
        identity: ControlClientIdentity,
        admission: ServerAccessAdmission,
    },
    Owner(ServerAccessAdmission),
}

impl RequestHandler {
    pub(in crate::handler) async fn capture_floating_move_write_authority(
        &self,
        requester_pid: u32,
    ) -> Result<FloatingMoveWriteAuthority, RmuxError> {
        if let Some(identity) = current_control_queue_identity(requester_pid) {
            let (user, can_write) = {
                let active_control = self.active_control.lock().await;
                let active = active_control
                    .by_pid
                    .get(&requester_pid)
                    .filter(|active| active.id == identity.control_id())
                    .ok_or_else(authority_changed_error)?;
                (
                    active.user.clone(),
                    active.can_write && !active.closing.load(std::sync::atomic::Ordering::SeqCst),
                )
            };
            return Ok(FloatingMoveWriteAuthority::ControlQueue {
                identity,
                admission: self.floating_move_write_admission(&user, can_write)?,
            });
        }

        let detached = {
            let detached_access = self
                .active_detached_requester_access
                .lock()
                .expect("active detached requester access mutex must not be poisoned");
            detached_access.get(&requester_pid).map(|access| {
                access
                    .unambiguous_admission()
                    .cloned()
                    .ok_or_else(read_only_error)
            })
        };
        if let Some(admission) = detached {
            let admission = admission?;
            let can_write = self
                .server_access
                .lock()
                .expect("server access mutex must not be poisoned")
                .revalidate_detached_admission(&admission)
                .is_some_and(AccessMode::can_write);
            if !can_write {
                return Err(read_only_error());
            }
            return Ok(FloatingMoveWriteAuthority::Detached {
                requester_pid,
                admission,
            });
        }

        let attach = {
            let active_attach = self.active_attach.lock().await;
            active_attach.by_pid.get(&requester_pid).map(|active| {
                (
                    active.identity(requester_pid),
                    active.user.clone(),
                    active.can_write
                        && !active.flags.contains(ClientFlags::READONLY)
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst),
                )
            })
        };
        if let Some((identity, user, can_write)) = attach {
            return Ok(FloatingMoveWriteAuthority::Attach {
                identity,
                admission: self.floating_move_write_admission(&user, can_write)?,
            });
        }

        let control = {
            let active_control = self.active_control.lock().await;
            active_control.by_pid.get(&requester_pid).map(|active| {
                (
                    ControlClientIdentity::new(requester_pid, active.id),
                    active.user.clone(),
                    active.can_write && !active.closing.load(std::sync::atomic::Ordering::SeqCst),
                )
            })
        };
        if let Some((identity, user, can_write)) = control {
            return Ok(FloatingMoveWriteAuthority::Control {
                identity,
                admission: self.floating_move_write_admission(&user, can_write)?,
            });
        }

        if requester_pid == std::process::id() {
            return Ok(FloatingMoveWriteAuthority::Owner(
                self.server_access
                    .lock()
                    .expect("server access mutex must not be poisoned")
                    .owner_admission(),
            ));
        }
        Err(read_only_error())
    }

    fn floating_move_write_admission(
        &self,
        user: &UserIdentity,
        can_write: bool,
    ) -> Result<ServerAccessAdmission, RmuxError> {
        self.server_access
            .lock()
            .expect("server access mutex must not be poisoned")
            .admission_for_identity_with_write_cap(user, can_write)
            .filter(ServerAccessAdmission::can_write)
            .ok_or_else(read_only_error)
    }

    pub(in crate::handler) async fn commit_floating_move_with_authority<T>(
        &self,
        authority: &FloatingMoveWriteAuthority,
        state: &mut crate::pane_terminals::HandlerState,
        commit: impl FnOnce(&mut crate::pane_terminals::HandlerState) -> Result<T, RmuxError>,
    ) -> Result<T, RmuxError> {
        match authority {
            FloatingMoveWriteAuthority::ControlQueue {
                identity,
                admission,
            } => {
                let active_control = self.active_control.lock().await;
                Self::validate_control_queue_identity_locked(
                    state,
                    &active_control,
                    identity.requester_pid(),
                    identity.control_id(),
                )?;
                let active = active_control
                    .by_pid
                    .get(&identity.requester_pid())
                    .expect("validated control identity remains present while locked");
                if !active.can_write || active.closing.load(std::sync::atomic::Ordering::SeqCst) {
                    return Err(read_only_error());
                }
                let server_access = self
                    .server_access
                    .lock()
                    .expect("server access mutex must not be poisoned");
                require_current_write_admission(&server_access, admission)?;
                commit(state)
            }
            FloatingMoveWriteAuthority::Detached {
                requester_pid,
                admission,
            } => {
                let detached_access = self
                    .active_detached_requester_access
                    .lock()
                    .expect("active detached requester access mutex must not be poisoned");
                if detached_access
                    .get(requester_pid)
                    .and_then(DetachedRequesterAccess::unambiguous_admission)
                    != Some(admission)
                {
                    return Err(authority_changed_error());
                }
                let server_access = self
                    .server_access
                    .lock()
                    .expect("server access mutex must not be poisoned");
                require_current_write_admission(&server_access, admission)?;
                commit(state)
            }
            FloatingMoveWriteAuthority::Attach {
                identity,
                admission,
            } => {
                let active_attach = self.active_attach.lock().await;
                let active = active_attach
                    .by_pid
                    .get(&identity.attach_pid())
                    .filter(|active| identity.matches_active(active))
                    .ok_or_else(authority_changed_error)?;
                if !active.can_write
                    || active.flags.contains(ClientFlags::READONLY)
                    || active.closing.load(std::sync::atomic::Ordering::SeqCst)
                {
                    return Err(read_only_error());
                }
                let server_access = self
                    .server_access
                    .lock()
                    .expect("server access mutex must not be poisoned");
                require_current_write_admission(&server_access, admission)?;
                commit(state)
            }
            FloatingMoveWriteAuthority::Control {
                identity,
                admission,
            } => {
                let active_control = self.active_control.lock().await;
                let active = active_control
                    .by_pid
                    .get(&identity.requester_pid())
                    .filter(|active| active.id == identity.control_id())
                    .ok_or_else(authority_changed_error)?;
                if !active.can_write || active.closing.load(std::sync::atomic::Ordering::SeqCst) {
                    return Err(read_only_error());
                }
                let server_access = self
                    .server_access
                    .lock()
                    .expect("server access mutex must not be poisoned");
                require_current_write_admission(&server_access, admission)?;
                commit(state)
            }
            FloatingMoveWriteAuthority::Owner(admission) => {
                let server_access = self
                    .server_access
                    .lock()
                    .expect("server access mutex must not be poisoned");
                require_current_write_admission(&server_access, admission)?;
                commit(state)
            }
        }
    }
}

fn require_current_write_admission(
    server_access: &crate::server_access::ServerAccessStore,
    admission: &ServerAccessAdmission,
) -> Result<(), RmuxError> {
    server_access
        .revalidate_detached_admission(admission)
        .is_some_and(AccessMode::can_write)
        .then_some(())
        .ok_or_else(read_only_error)
}

fn read_only_error() -> RmuxError {
    RmuxError::Server("client is read-only".to_owned())
}

fn authority_changed_error() -> RmuxError {
    RmuxError::Server("requester authority changed before commit".to_owned())
}
