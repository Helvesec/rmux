//! Completion of the queued invocations that dispatch one `Request`: the
//! explicit run-shell target, its targeted output delivery, the dispatched
//! hooks and the queue action for the queue mode.

use rmux_proto::RunShellRequest;

use super::super::queue_special_target::QueueSpecialTargetBinding;
use super::super::{
    capture_inline_hooks, queue_action_from_response, PaneId, PaneTarget, ParsedCommand,
    QueueCommandAction, QueueExecutionContext, QueueMode, Request, RequestHandler, Response,
    RmuxError,
};
use crate::future::boxed;
use crate::hook_runtime::PendingInlineHook;
use crate::pane_io::HandleOutcome;

/// A queued request after dispatch, with what its completion still needs.
pub(super) struct DispatchedQueueRequest {
    pub(super) request: Request,
    pub(super) outcome: HandleOutcome,
    pub(super) inline_hooks: Vec<PendingInlineHook>,
    pub(super) explicit_run_shell_target: Option<(PaneTarget, PaneId)>,
}

/// The queue that dispatched a request: its mode and, for a control queue,
/// the control client identity it captured.
#[derive(Clone, Copy)]
pub(super) struct QueueDispatchOrigin {
    pub(super) requester_pid: u32,
    pub(super) mode: QueueMode,
    pub(super) expected_control_id: Option<u64>,
}

/// A non-inserting queued run-shell, after the queue context and the access
/// policy were applied to its request.
pub(super) struct QueuedRunShellResponse<'a> {
    pub(super) request: Box<RunShellRequest>,
    pub(super) explicit_target: Option<PaneTarget>,
    pub(super) client_name: Option<String>,
    pub(super) target_missing_canfail: bool,
    pub(super) special_target_binding: Option<&'a QueueSpecialTargetBinding>,
}

impl RequestHandler {
    /// Resolves an explicit run-shell target to the pane it names when the
    /// command is queued, so its output reaches that pane after dispatch.
    pub(super) async fn resolve_explicit_run_shell_target(
        &self,
        target: Option<PaneTarget>,
    ) -> Option<(PaneTarget, PaneId)> {
        match target {
            Some(target) => self
                .pane_id_for_slot_target(&target)
                .await
                .map(|pane_id| (target, pane_id)),
            None => None,
        }
    }

    pub(super) async fn execute_queued_run_shell_response(
        &self,
        origin: QueueDispatchOrigin,
        run_shell: QueuedRunShellResponse<'_>,
        context: &QueueExecutionContext,
        command_for_hooks: &ParsedCommand,
    ) -> Result<QueueCommandAction, RmuxError> {
        let QueuedRunShellResponse {
            request,
            explicit_target,
            client_name,
            target_missing_canfail,
            special_target_binding,
        } = run_shell;
        let explicit_run_shell_target = self
            .resolve_explicit_run_shell_target(explicit_target)
            .await;
        let request_for_hooks = Request::RunShell(request.clone());
        let (outcome, inline_hooks) = boxed(|| {
            capture_inline_hooks(async {
                HandleOutcome::response(
                    self.handle_queued_run_shell_with_client_name(
                        origin.requester_pid,
                        *request,
                        client_name,
                        target_missing_canfail,
                        context,
                        special_target_binding,
                    )
                    .await,
                )
            })
        })
        .await;
        self.complete_dispatched_queue_request(
            origin,
            DispatchedQueueRequest {
                request: request_for_hooks,
                outcome,
                inline_hooks,
                explicit_run_shell_target,
            },
            command_for_hooks,
        )
        .await
    }

    /// Delivers targeted run-shell output, runs the dispatched hooks, then
    /// derives the queue action; delivered output is not printed again.
    pub(super) async fn complete_dispatched_queue_request(
        &self,
        origin: QueueDispatchOrigin,
        dispatched: DispatchedQueueRequest,
        command_for_hooks: &ParsedCommand,
    ) -> Result<QueueCommandAction, RmuxError> {
        let DispatchedQueueRequest {
            request,
            outcome,
            inline_hooks,
            explicit_run_shell_target,
        } = dispatched;
        let requester_pid = origin.requester_pid;
        let targeted_output_delivered =
            if let Some((target, pane_id)) = explicit_run_shell_target.as_ref() {
                boxed(|| {
                    self.deliver_targeted_run_shell_output(
                        requester_pid,
                        target,
                        *pane_id,
                        &outcome.response,
                    )
                })
                .await
            } else {
                false
            };
        self.run_dispatched_hooks(
            requester_pid,
            &request,
            &outcome.response,
            inline_hooks,
            Some(command_for_hooks),
        )
        .await;
        let action = match origin.mode {
            QueueMode::Detached => queue_action_from_response(outcome.response),
            QueueMode::Control => {
                boxed(|| {
                    self.control_queue_action_from_outcome(
                        requester_pid,
                        origin
                            .expected_control_id
                            .expect("control queues capture a client identity"),
                        request,
                        outcome,
                    )
                })
                .await
            }
        };
        if targeted_output_delivered {
            action.map(QueueCommandAction::without_output)
        } else {
            action
        }
    }
}

pub(super) fn quiet_only_if_unset_response(request: &Request, response: Response) -> Response {
    let (
        Request::SetOptionByName(request),
        Response::Error(rmux_proto::ErrorResponse {
            error: RmuxError::InvalidSetOption(message),
        }),
    ) = (request, &response)
    else {
        return response;
    };
    let Some(name) = message.strip_suffix(" is already set") else {
        return response;
    };
    Response::SetOptionByName(rmux_proto::SetOptionByNameResponse {
        scope: request.scope.clone(),
        name: name.to_owned(),
        mode: request.mode,
    })
}
