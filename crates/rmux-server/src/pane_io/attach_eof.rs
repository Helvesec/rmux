use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use rmux_proto::{AttachFrameDecoder, WindowTarget};
use tokio::sync::watch;

use super::{
    begin_attach_mutation_batch, emit_attach_stop, flush_due_pending_escape_input, log_attach_exit,
    process_socket_messages_until_barrier, types, AttachExitReason, AttachTransport,
    LiveAttachInputContext, PendingAttachInputState, PendingEscapeFlush,
};

// EOF ends input admission, but the finite decoded tail still owns its
// ordinary identity checks, input ordering and unlock output barriers.
#[allow(clippy::too_many_arguments)]
pub(super) async fn finish_attach_eof(
    stream: &AttachTransport,
    decoder: &mut AttachFrameDecoder,
    live_input: &LiveAttachInputContext,
    shutdown: &watch::Receiver<()>,
    closing: &AtomicBool,
    current_target: &mut types::OpenAttachTarget,
    pending_input: &mut Vec<u8>,
    active_emit_cache: &mut Option<(u64, WindowTarget)>,
    locked: &mut bool,
    pending_escape_flush: &mut PendingEscapeFlush,
) -> io::Result<()> {
    let result: io::Result<()> = async {
        let Some(_batch) = begin_attach_mutation_batch(live_input, shutdown) else {
            return Ok(());
        };
        while !decoder.is_empty() {
            flush_due_pending_escape_input(
                pending_escape_flush,
                live_input,
                pending_input,
                *locked,
            )
            .await?;
            let (_, stopped_at_barrier) = process_socket_messages_until_barrier(
                decoder,
                stream,
                live_input,
                Some(&mut *current_target),
                PendingAttachInputState::new(pending_input, pending_escape_flush),
                active_emit_cache,
                locked,
            )
            .await?;
            if !stopped_at_barrier {
                // Exhausted complete frames; a truncated final wire frame
                // remains uncommitted, just as on the normal input path.
                break;
            }
        }
        while !*locked && !pending_input.is_empty() {
            // No later client byte can complete an admitted escape prefix.
            // An OSC/APC flush can reroute its body and retain a new suffix.
            // Each pass must consume bytes, bounding this drain by the tail.
            let retained_len = pending_input.len();
            live_input
                .handler
                .flush_attached_pending_escape_input_for_identity(
                    live_input.identity,
                    pending_input,
                )
                .await?;
            if pending_input.len() >= retained_len {
                return Err(io::Error::other("attach EOF escape flush made no progress"));
            }
        }
        Ok(())
    }
    .await;
    let result = match result {
        Err(_) if closing.load(Ordering::SeqCst) => {
            PendingAttachInputState::new(pending_input, pending_escape_flush).clear();
            Ok(())
        }
        result => result,
    };
    log_attach_exit(
        live_input,
        current_target,
        AttachExitReason::AttachStreamClosed,
    );
    // Input errors still restore the terminal. Preserve the dispatch error
    // and the existing best-effort handling of a disconnected output peer.
    let _ = emit_attach_stop(stream, current_target).await;
    result
}
