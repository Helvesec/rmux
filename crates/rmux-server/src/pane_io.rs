#[cfg(any(unix, windows))]
#[cfg(not(target_has_atomic = "64"))]
use portable_atomic::AtomicU64;
#[cfg(any(unix, windows))]
use rmux_core::TerminalPassthrough;
#[cfg(any(unix, windows))]
use rmux_proto::{AttachFrameDecoder, AttachMessage};
#[cfg(test)]
use std::collections::HashMap;
#[cfg(all(any(unix, windows), target_has_atomic = "64"))]
use std::sync::atomic::AtomicU64;
#[cfg(any(unix, windows))]
use std::sync::atomic::{AtomicBool, AtomicUsize};
#[cfg(any(unix, windows))]
use std::sync::Arc;
#[cfg(any(unix, windows))]
use std::{collections::VecDeque, io, sync::atomic::Ordering};
#[cfg(any(unix, windows))]
use tokio::sync::mpsc;
#[cfg(any(unix, windows))]
use tokio::sync::watch;
#[cfg(any(unix, windows))]
use tokio::time::{Duration, Instant};

pub(crate) const READ_BUFFER_SIZE: usize = 64 * 1024;
#[cfg(any(unix, windows))]
const ATTACH_INTERACTIVE_OUTPUT_WINDOW: Duration = Duration::from_millis(250);
#[cfg(any(unix, windows))]
// Bound each opportunistic socket drain so sustained attach input cannot keep
// this task away from render, control, shutdown, or escape-flush futures.
const MAX_IMMEDIATE_ATTACH_READS: usize = 8;
#[cfg(windows)]
const ATTACH_EXIT_OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);
#[cfg(any(unix, windows))]
const ATTACH_INPUT_STACK_PAYLOAD: usize = 1024;
#[cfg(all(unix, test))]
const MAX_PREDICTED_LOCAL_ECHO_BYTES: usize = 16;
#[cfg(unix)]
const PREDICTED_LOCAL_ECHO_TIMEOUT: Duration = Duration::from_millis(250);

#[cfg(test)]
#[derive(Debug, Default)]
struct LiveAttachInputApplyPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
static LIVE_ATTACH_INPUT_APPLY_PAUSE: std::sync::Mutex<
    Option<(
        crate::handler::attach_support::ActiveAttachIdentity,
        Arc<LiveAttachInputApplyPause>,
    )>,
> = std::sync::Mutex::new(None);

#[cfg(test)]
static LIVE_ATTACH_INPUT_VALIDATION_PAUSE: std::sync::Mutex<
    Vec<(
        crate::handler::attach_support::ActiveAttachIdentity,
        Arc<LiveAttachInputApplyPause>,
    )>,
> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
#[derive(Debug, Default)]
struct PaneOutputBatchDrainPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
static PANE_OUTPUT_BATCH_DRAIN_PAUSE: std::sync::LazyLock<
    std::sync::Mutex<HashMap<rmux_proto::SessionName, Arc<PaneOutputBatchDrainPause>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

#[cfg(test)]
fn install_pane_output_batch_drain_pause(
    session_name: rmux_proto::SessionName,
) -> Arc<PaneOutputBatchDrainPause> {
    let pause = Arc::new(PaneOutputBatchDrainPause::default());
    PANE_OUTPUT_BATCH_DRAIN_PAUSE
        .lock()
        .expect("pane output batch drain pause lock")
        .insert(session_name, Arc::clone(&pause));
    pause
}

#[cfg(test)]
async fn pause_after_pane_output_batch_drain(session_name: &rmux_proto::SessionName) {
    let pause = {
        let mut installed = PANE_OUTPUT_BATCH_DRAIN_PAUSE
            .lock()
            .expect("pane output batch drain pause lock");
        installed.remove(session_name)
    };
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
}

#[cfg(test)]
fn install_live_attach_input_apply_pause(
    identity: crate::handler::attach_support::ActiveAttachIdentity,
) -> Arc<LiveAttachInputApplyPause> {
    let pause = Arc::new(LiveAttachInputApplyPause::default());
    *LIVE_ATTACH_INPUT_APPLY_PAUSE
        .lock()
        .expect("live attach input pause lock") = Some((identity, Arc::clone(&pause)));
    pause
}

#[cfg(test)]
fn install_live_attach_input_validation_pause(
    identity: crate::handler::attach_support::ActiveAttachIdentity,
) -> Arc<LiveAttachInputApplyPause> {
    let pause = Arc::new(LiveAttachInputApplyPause::default());
    LIVE_ATTACH_INPUT_VALIDATION_PAUSE
        .lock()
        .expect("live attach input validation pause lock")
        .push((identity, Arc::clone(&pause)));
    pause
}

#[cfg(test)]
async fn pause_before_live_attach_input_validation(
    identity: crate::handler::attach_support::ActiveAttachIdentity,
) {
    let pause = {
        let mut installed = LIVE_ATTACH_INPUT_VALIDATION_PAUSE
            .lock()
            .expect("live attach input validation pause lock");
        installed
            .iter()
            .position(|(expected, _)| *expected == identity)
            .map(|position| installed.remove(position).1)
    };
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
}

#[cfg(test)]
async fn pause_after_live_attach_input_validation(
    identity: crate::handler::attach_support::ActiveAttachIdentity,
) {
    let pause = {
        let mut installed = LIVE_ATTACH_INPUT_APPLY_PAUSE
            .lock()
            .expect("live attach input pause lock");
        installed
            .as_ref()
            .is_some_and(|(expected, _)| *expected == identity)
            .then(|| {
                installed
                    .take()
                    .expect("matching pause remains installed")
                    .1
            })
    };
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
}
mod attach_control;
#[cfg(any(unix, windows))]
mod attach_eof;
#[cfg(any(unix, windows))]
mod attach_input_dispatch;
mod attach_output_batch;
mod attach_transport;
mod control;
mod deferred_passthrough;
mod exit_log;
mod live_render;
mod passthrough;
mod pending_escape;
mod persistent_overlay;
mod reader;
mod refresh_scheduler;
mod types;
mod wire;

#[cfg(any(unix, windows))]
use attach_input_dispatch::{process_socket_messages, process_socket_messages_until_barrier};

#[cfg(any(unix, windows))]
use crate::renderer::{PaneRenderDelta, PaneRenderDeltaFrame};
#[cfg(test)]
pub(crate) use attach_control::release_attach_control_backlog;
#[cfg_attr(windows, allow(unused_imports))]
pub(crate) use attach_control::{AttachControl, AttachControlSender};
#[cfg(any(unix, windows))]
use attach_output_batch::{
    collect_attach_output_batch, collect_attach_output_batch_metadata, AttachOutputBatch,
};
#[cfg(all(any(unix, windows), feature = "web"))]
pub(crate) use attach_transport::in_process_attach_pair;
use attach_transport::{AttachTransport, TryAttachRead};
#[cfg(any(unix, windows))]
use control::{
    apply_pending_attach_controls, coalesce_render_switches, preserves_live_output,
    recv_attach_control, redraw_after_persistent_overlay_state_advance, should_emit_overlay,
    switch_attach_target, take_pending_live_passthroughs, try_recv_attach_control,
    PendingAttachAction, PendingAttachExit, PendingAttachInputState,
};
#[cfg(any(unix, windows))]
use deferred_passthrough::{
    clear_deferred_passthroughs_if_target_changed, defer_passthroughs,
    defer_passthroughs_after_source_invalidation, drain_passthrough_frame_at_exit,
    flush_deferred_passthroughs, holds_cursor_positioned_passthrough,
    take_passthrough_frame_for_client, DeferredPassthroughQueue, PaneOutputGeneration,
};
#[cfg(any(unix, windows))]
use exit_log::{record_attach_error, record_attach_exit, AttachExitReason};
pub(crate) use live_render::LivePaneRender;
#[cfg(any(unix, windows))]
use pending_escape::PendingEscapeFlush;
#[cfg(test)]
pub(crate) use persistent_overlay::replay_client_visible_payloads;
#[cfg(any(unix, windows))]
use persistent_overlay::{
    accept_persistent_overlay_state, advance_persistent_overlay_state,
    cache_and_choose_overlay_frame, discard_stale_persistent_overlays, is_stale_persistent_switch,
    persistent_overlay_replacement_pending, prime_persistent_overlay_barriers,
    switch_persistent_overlay_frame, take_pending_persistent_overlay_for_state,
    update_persistent_overlay_cache, CachedPersistentOverlay,
};
#[cfg(windows)]
pub(crate) use reader::spawn_pane_exit_watcher;
pub(crate) use reader::spawn_pane_output_reader;
#[cfg(windows)]
pub(crate) use reader::PaneOutputEofState;
#[cfg(unix)]
pub(crate) use reader::PaneOutputReaderTask;
#[cfg(test)]
pub(crate) use reader::{
    publish_pane_bytes_capturing_alerts, publish_pane_bytes_for_test,
    publish_pane_bytes_through_effects,
};
#[cfg(any(unix, windows))]
use refresh_scheduler::{
    wait_for_refresh_deadline, AttachRefreshScheduler, AttachStatusRefreshScheduler,
};
#[cfg(test)]
pub(crate) use types::pane_output_channel;
#[cfg(test)]
pub(crate) use types::pane_output_channel_with_limits;
#[cfg(any(unix, windows))]
pub(crate) use types::LiveAttachInputContext;
#[cfg_attr(windows, allow(unused_imports))]
pub(crate) use types::{
    pane_output_channel_with_read_demand, AttachSessionUpgrade, AttachTarget, HandleOutcome,
    OverlayFrame, PaneAlertCallback, PaneAlertEvent, PaneBoundary, PaneEffectSink,
    PaneExitCallback, PaneExitEvent, PaneInvalidationReason, PaneObservationItem,
    PaneOutputAttachedClientReadDemandGuard, PaneOutputReadDemandGuard,
    PaneOutputReadDemandRegistry, PaneOutputReceiver, PaneOutputSender,
};
#[cfg(any(unix, windows))]
use wire::{
    emit_attach_bytes, emit_attach_frame, emit_attach_message, emit_attach_stop,
    emit_coalescible_render_frame, emit_detached_attach_stop, emit_exited_attach_stop,
    emit_render_frame, emit_server_exited_attach_stop, invalid_attach_message, open_attach_target,
    read_socket_bytes, recv_pane_output_optional, try_read_socket_bytes,
};

#[cfg(any(unix, windows))]
mod forward;
#[cfg(any(unix, windows))]
pub(crate) use forward::{forward_attach, forward_attach_with_opt_ins, AttachStreamOptIns};
#[cfg(any(unix, windows))]
mod input;
#[cfg(any(unix, windows))]
mod lifecycle;
#[cfg(any(unix, windows))]
mod render;

#[cfg(any(unix, windows))]
use input::{
    absorb_transient_terminal_prefix, flush_due_pending_escape_input,
    mark_attach_interactive_input, pending_escape_deadline_due, process_attach_data_payload,
    process_attach_socket_messages,
};
#[cfg(unix)]
use input::{consume_predicted_echo, PredictedEcho};
#[cfg(all(test, unix))]
use input::{
    is_predictable_local_echo, predictable_local_echo_prefix_len,
    sync_pending_escape_flush_with_escape_time,
};
#[cfg(any(unix, windows))]
use lifecycle::{
    attach_shutdown_observable, begin_attach_mutation_batch, finish_pending_attach_exit,
    finish_pending_attach_exit_with_batch, finish_terminal_attach_control, log_attach_exit,
    pending_attach_exit_output_batch, take_pending_terminal_attach_control,
};
#[cfg(any(unix, windows))]
use render::{
    clear_close_pane_output_after_refresh_if_target_changed,
    collect_pending_attach_output_batch_metadata, emit_live_render_frame,
    refresh_current_attach_client, reschedule_status_refresh_for_session,
    reschedule_status_refresh_for_target, reschedule_status_refresh_if_target_changed,
    schedule_attach_render_refresh, should_treat_attach_output_as_interactive,
    synchronize_persistent_overlay_epoch, try_forward_plain_output,
};

#[cfg(all(test, unix))]
mod tests;
