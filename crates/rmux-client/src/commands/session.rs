use rmux_proto::request::{
    AttachSessionExt2Request, AttachSessionExt3Request, AttachSessionExtRequest,
    CreateSessionLeaseRequest, DetachClientExtRequest, ListClientsRequest, NewSessionExtRequest,
    RefreshClientRequest, ReleaseSessionLeaseRequest, RenewSessionLeaseRequest, Request,
    SuspendClientRequest, SwitchClientExt2Request, SwitchClientExt3Request,
};
use rmux_proto::{
    AttachSessionRequest, DetachClientRequest, HasSessionRequest, KillSessionRequest,
    ListPanesRequest, ListSessionsRequest, NewSessionRequest, RenameSessionRequest, Response,
    SessionName, SwitchClientExtRequest, SwitchClientRequest, TerminalSize,
    CAPABILITY_SDK_SESSION_LEASE,
};

use crate::{
    connection::{AttachTransition, Connection},
    ClientError,
};

impl Connection {
    /// Sends a `new-session` request over the detached RPC channel.
    pub fn new_session(
        &mut self,
        session_name: SessionName,
        detached: bool,
        size: Option<TerminalSize>,
    ) -> Result<Response, ClientError> {
        self.new_session_with_environment(session_name, detached, size, None)
    }

    /// Sends a `new-session` request with explicit spawn environment overrides.
    pub fn new_session_with_environment(
        &mut self,
        session_name: SessionName,
        detached: bool,
        size: Option<TerminalSize>,
        environment: Option<Vec<String>>,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::NewSession(NewSessionRequest {
            session_name,
            detached,
            size,
            environment,
        }))
    }

    /// Sends an extended `new-session` request with grouped-session and attach-if-exists semantics.
    pub fn new_session_extended(
        &mut self,
        request: NewSessionExtRequest,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::NewSessionExt(Box::new(request)))
    }

    /// Sends a `has-session` request over the detached RPC channel.
    pub fn has_session(&mut self, target: SessionName) -> Result<Response, ClientError> {
        self.roundtrip(&Request::HasSession(HasSessionRequest { target }))
    }

    /// Sends a `kill-session` request over the detached RPC channel.
    pub fn kill_session(&mut self, request: KillSessionRequest) -> Result<Response, ClientError> {
        self.roundtrip(&Request::KillSession(request))
    }

    /// Sends a `rename-session` request over the detached RPC channel.
    pub fn rename_session(
        &mut self,
        target: SessionName,
        new_name: SessionName,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::RenameSession(RenameSessionRequest {
            target,
            new_name,
        }))
    }

    /// Creates an SDK-owned session lease.
    pub fn create_session_lease(
        &mut self,
        session_name: SessionName,
        ttl_millis: u64,
    ) -> Result<Response, ClientError> {
        if !self.supports_capability(CAPABILITY_SDK_SESSION_LEASE)? {
            return Err(ClientError::Protocol(
                rmux_proto::RmuxError::UnsupportedCapability {
                    feature: CAPABILITY_SDK_SESSION_LEASE.to_owned(),
                    supported: Vec::new(),
                },
            ));
        }
        self.roundtrip(&Request::CreateSessionLease(CreateSessionLeaseRequest {
            session_name,
            ttl_millis,
        }))
    }

    /// Renews an SDK-owned session lease.
    pub fn renew_session_lease(
        &mut self,
        session_name: SessionName,
        token: u64,
        ttl_millis: u64,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::RenewSessionLease(RenewSessionLeaseRequest {
            session_name,
            token,
            ttl_millis,
        }))
    }

    /// Releases an SDK-owned session lease.
    pub fn release_session_lease(
        &mut self,
        session_name: SessionName,
        token: u64,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ReleaseSessionLease(ReleaseSessionLeaseRequest {
            session_name,
            token,
        }))
    }

    /// Sends a `list-sessions` request over the detached RPC channel.
    pub fn list_sessions(&mut self, request: ListSessionsRequest) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ListSessions(request))
    }

    /// Sends a `list-panes` request over the detached RPC channel.
    pub fn list_panes(
        &mut self,
        target: SessionName,
        format: Option<String>,
    ) -> Result<Response, ClientError> {
        self.list_panes_in_window(target, None, format)
    }

    /// Sends a `list-panes` request scoped to an optional window index.
    pub fn list_panes_in_window(
        &mut self,
        target: SessionName,
        target_window_index: Option<u32>,
        format: Option<String>,
    ) -> Result<Response, ClientError> {
        self.list_panes_in_window_with_options(
            target,
            target_window_index,
            format,
            None,
            None,
            false,
        )
    }

    /// Sends a `list-panes` request scoped to an optional window index, with
    /// filtering and ordering options.
    pub fn list_panes_in_window_with_options(
        &mut self,
        target: SessionName,
        target_window_index: Option<u32>,
        format: Option<String>,
        filter: Option<String>,
        sort_order: Option<String>,
        reversed: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ListPanes(Box::new(ListPanesRequest {
            target,
            target_window_index,
            format,
            filter,
            sort_order,
            reversed,
        })))
    }

    /// Sends a `switch-client` request over the detached RPC channel.
    pub fn switch_client(&mut self, target: SessionName) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SwitchClient(SwitchClientRequest { target }))
    }

    /// Sends an extended `switch-client` request over the detached RPC channel.
    pub fn switch_client_extended(
        &mut self,
        target: Option<SessionName>,
        key_table: Option<String>,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SwitchClientExt(SwitchClientExtRequest {
            target,
            key_table,
        }))
    }

    /// Sends a further-extended `switch-client` request over the detached RPC channel.
    pub fn switch_client_with_session_flags(
        &mut self,
        request: SwitchClientExt2Request,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SwitchClientExt2(Box::new(request)))
    }

    /// Sends the most recent `switch-client` request shape over the detached RPC channel.
    pub fn switch_client_with_target_selector(
        &mut self,
        request: SwitchClientExt3Request,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SwitchClientExt3(Box::new(request)))
    }

    /// Sends a `detach-client` request over the detached RPC channel.
    pub fn detach_client(&mut self) -> Result<Response, ClientError> {
        self.roundtrip(&Request::DetachClient(DetachClientRequest))
    }

    /// Sends an extended `detach-client` request over the detached RPC channel.
    pub fn detach_client_extended(
        &mut self,
        request: DetachClientExtRequest,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::DetachClientExt(request))
    }

    /// Sends a `refresh-client` request over the detached RPC channel.
    pub fn refresh_client(
        &mut self,
        request: RefreshClientRequest,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::RefreshClient(Box::new(request)))
    }

    /// Sends a `list-clients` request over the detached RPC channel.
    pub fn list_clients(&mut self, request: ListClientsRequest) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ListClients(Box::new(request)))
    }

    /// Sends a `suspend-client` request over the detached RPC channel.
    pub fn suspend_client(
        &mut self,
        request: SuspendClientRequest,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SuspendClient(request))
    }

    /// Requests an attach upgrade and, on success, yields the raw Unix stream.
    ///
    /// Once this method returns [`AttachTransition::Upgraded`], the detached
    /// framing codec is no longer in the data path for the connection.
    pub fn begin_attach(mut self, target: SessionName) -> Result<AttachTransition, ClientError> {
        self.write_request(&Request::AttachSession(AttachSessionRequest { target }))?;
        let response = self.read_response()?;

        match response {
            Response::AttachSession(response) => Ok(AttachTransition::Upgraded(
                self.into_attach_upgrade(response)?,
            )),
            other => Ok(AttachTransition::Rejected(other)),
        }
    }

    /// Requests an extended attach upgrade and, on success, yields the raw Unix stream.
    pub fn begin_attach_extended(
        mut self,
        request: AttachSessionExtRequest,
    ) -> Result<AttachTransition, ClientError> {
        self.write_request(&Request::AttachSessionExt(request))?;
        let response = self.read_response()?;

        match response {
            Response::AttachSession(response) => Ok(AttachTransition::Upgraded(
                self.into_attach_upgrade(response)?,
            )),
            other => Ok(AttachTransition::Rejected(other)),
        }
    }

    /// Sends the most recent `attach-session` request shape over the detached RPC channel.
    pub fn begin_attach_with_target_spec(
        mut self,
        request: AttachSessionExt2Request,
    ) -> Result<AttachTransition, ClientError> {
        self.write_request(&Request::AttachSessionExt2(Box::new(request)))?;
        let response = self.read_response()?;

        match response {
            Response::AttachSession(response) => Ok(AttachTransition::Upgraded(
                self.into_attach_upgrade(response)?,
            )),
            other => Ok(AttachTransition::Rejected(other)),
        }
    }

    /// Sends a capability-aware `attach-session` request over the detached RPC channel.
    pub fn begin_attach_with_capabilities(
        mut self,
        request: AttachSessionExt3Request,
    ) -> Result<AttachTransition, ClientError> {
        self.write_request(&Request::AttachSessionExt3(Box::new(request)))?;
        let response = self.read_response()?;

        match response {
            Response::AttachSession(response) => Ok(AttachTransition::Upgraded(
                self.into_attach_upgrade(response)?,
            )),
            other => Ok(AttachTransition::Rejected(other)),
        }
    }

    /// Sends the most recent `attach-session` request shape over the detached RPC channel.
    pub fn attach_session_with_target_spec_detached(
        &mut self,
        request: AttachSessionExt2Request,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::AttachSessionExt2(Box::new(request)))
    }
}
