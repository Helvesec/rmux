use crate::{ClientError, Connection};
use rmux_proto::{Request, Response, RuntimeCommandExecutionRequest};

impl Connection {
    /// Sends an atomic offer with an independent operation intent.
    ///
    /// An older peer's unknown-tag refusal is terminal for V2. Never downgrade
    /// or replay after a refusal, completion, or ambiguous transport failure.
    pub fn runtime_command_execution_v2(
        &mut self,
        request: rmux_proto::RuntimeCommandExecutionV2Request,
    ) -> Result<Response, ClientError> {
        self.roundtrip_without_read_timeout(&Request::RuntimeCommandExecutionV2(Box::new(request)))
    }

    /// Sends one atomic split/key offer. Every error is terminal; never replay.
    pub fn split_window_command_v1(
        &mut self,
        request: rmux_proto::SplitWindowCommandV1Request,
    ) -> Result<Response, ClientError> {
        self.roundtrip_without_read_timeout(&Request::SplitWindowCommandV1(Box::new(request)))
    }

    /// Sends one `new-pane -I` offer with its finished payload.
    ///
    /// Terminal like the split offer: a refusal, a completion or an ambiguous
    /// transport failure all end here, and the command is never sent twice.
    pub fn new_pane_input_v1(
        &mut self,
        request: rmux_proto::NewPaneInputV1Request,
    ) -> Result<Response, ClientError> {
        self.roundtrip_without_read_timeout(&Request::NewPaneInputV1(Box::new(request)))
    }

    /// Offers server-owned CLI argv execution with negotiation in the same RPC.
    ///
    /// Only a positive completion response proves execution. The caller must not
    /// replay on transport failure; an old peer's unknown-tag rejection closes
    /// this connection and requires a fresh legacy connection.
    pub fn runtime_command_execution(
        &mut self,
        request: RuntimeCommandExecutionRequest,
    ) -> Result<Response, ClientError> {
        self.roundtrip_without_read_timeout(&Request::RuntimeCommandExecution(Box::new(request)))
    }
}
