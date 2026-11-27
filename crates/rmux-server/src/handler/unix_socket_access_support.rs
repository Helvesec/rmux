use rmux_proto::RmuxError;

use super::RequestHandler;
#[cfg(unix)]
use crate::unix_socket::{BoundUnixListener, SocketFileIdentity, UnixTransportAccess};
#[cfg(unix)]
use crate::unix_socket_access::UnixSocketAccessController;

impl RequestHandler {
    pub(in crate::handler) fn transition_unix_transport(
        &self,
        allow_listed: bool,
    ) -> Result<(), RmuxError> {
        #[cfg(unix)]
        {
            let mut transport = self
                .unix_socket_access
                .lock()
                .expect("Unix socket access mutex must not be poisoned");
            let Some(transport) = transport.as_mut() else {
                #[cfg(test)]
                return Ok(());
                #[cfg(not(test))]
                return Err(RmuxError::Server(
                    "Unix socket access controller is not initialized".to_owned(),
                ));
            };
            let target = if allow_listed {
                UnixTransportAccess::AllowListed
            } else {
                UnixTransportAccess::OwnerOnly
            };
            transport.transition(target).map_err(|error| {
                RmuxError::Server(format!("failed to update Unix socket access: {error}"))
            })
        }
        #[cfg(not(unix))]
        {
            let _ = allow_listed;
            Ok(())
        }
    }

    #[cfg(unix)]
    pub(crate) fn install_unix_socket_access_controller(
        &self,
        controller: UnixSocketAccessController,
    ) -> std::io::Result<()> {
        let mut slot = self
            .unix_socket_access
            .lock()
            .expect("Unix socket access mutex must not be poisoned");
        if slot.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "Unix socket access controller is already installed",
            ));
        }
        *slot = Some(controller);
        Ok(())
    }

    #[cfg(all(test, unix))]
    pub(in crate::handler) fn install_unix_socket_access_for_test(
        &self,
        socket_path: &std::path::Path,
        socket_identity: Option<SocketFileIdentity>,
    ) -> std::io::Result<()> {
        self.install_unix_socket_access_controller(UnixSocketAccessController::new(
            socket_path,
            socket_identity,
        )?)
    }

    #[cfg(unix)]
    pub(crate) async fn rebind_unix_socket(
        &self,
        socket_path: &std::path::Path,
        current_identity: Option<SocketFileIdentity>,
    ) -> std::io::Result<BoundUnixListener> {
        let _mutation = self.server_access_mutation.lock().await;
        let access = {
            let transport = self
                .unix_socket_access
                .lock()
                .expect("Unix socket access mutex must not be poisoned");
            let transport = transport.as_ref().ok_or_else(|| {
                std::io::Error::other("Unix socket access controller is not initialized")
            })?;
            transport.validate_rebind_source()?;
            transport.access()
        };
        let rebound =
            crate::unix_socket::rebind_unix_listener_at(socket_path, current_identity, access)?;
        self.unix_socket_access
            .lock()
            .expect("Unix socket access mutex must not be poisoned")
            .as_mut()
            .expect("Unix socket access controller checked above")
            .adopt_rebound_socket(rebound.identity)?;
        Ok(rebound)
    }

    #[cfg(unix)]
    pub(crate) async fn restore_owner_only_unix_transport(&self) -> std::io::Result<()> {
        let _mutation = self.server_access_mutation.lock().await;
        let mut transport = self
            .unix_socket_access
            .lock()
            .expect("Unix socket access mutex must not be poisoned");
        if let Some(transport) = transport.as_mut() {
            transport.transition(UnixTransportAccess::OwnerOnly)?;
        }
        Ok(())
    }

    #[cfg(all(test, unix))]
    pub(in crate::handler) fn fail_next_unix_transport_transition_for_test(&self) {
        self.unix_socket_access
            .lock()
            .expect("Unix socket access mutex must not be poisoned")
            .as_mut()
            .expect("Unix socket access controller must be installed")
            .fail_next_transition_after_first_mode_change();
    }
}
