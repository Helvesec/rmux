#[cfg(any(unix, windows))]
use super::passthrough::render_passthroughs;
#[cfg(any(unix, windows))]
use super::types::OpenAttachTarget;
#[cfg(any(unix, windows))]
use super::wire::emit_attach_bytes;
#[cfg(any(unix, windows))]
use rmux_core::TerminalPassthrough;
#[cfg(any(unix, windows))]
use rmux_ipc::LocalStream;
#[cfg(any(unix, windows))]
use std::io;
#[cfg(any(unix, windows))]
use tracing::warn;

#[cfg(any(unix, windows))]
const DEFERRED_PASSTHROUGH_LIMIT: usize = 16;

// Terminal graphics are forwarded live to attached clients; they are not part
// of the text grid and are not replayed on later attaches. This queue only
// delays passthroughs while rmux-owned overlays are visible, and stays bounded
// so a busy image-producing app cannot grow server memory through the overlay
// path.
#[cfg(any(unix, windows))]
pub(super) fn defer_passthroughs(
    deferred_passthroughs: &mut Vec<TerminalPassthrough>,
    passthroughs: Vec<TerminalPassthrough>,
) {
    if passthroughs.is_empty() {
        return;
    }
    deferred_passthroughs.extend(passthroughs);
    let overflow = deferred_passthroughs
        .len()
        .saturating_sub(DEFERRED_PASSTHROUGH_LIMIT);
    if overflow > 0 {
        deferred_passthroughs.drain(..overflow);
        warn!(
            dropped = overflow,
            retained = DEFERRED_PASSTHROUGH_LIMIT,
            "dropped deferred terminal passthrough events due to overlay safety limit"
        );
    }
}

#[cfg(any(unix, windows))]
pub(super) fn take_passthrough_frame(
    current_target: &OpenAttachTarget,
    deferred_passthroughs: &mut Vec<TerminalPassthrough>,
) -> Vec<u8> {
    if deferred_passthroughs.is_empty() {
        return Vec::new();
    }
    let passthroughs = std::mem::take(deferred_passthroughs);
    render_passthroughs(current_target, &passthroughs)
}

#[cfg(any(unix, windows))]
pub(super) fn clear_deferred_passthroughs_if_target_changed(
    target_changed: bool,
    deferred_passthroughs: &mut Vec<TerminalPassthrough>,
) {
    if target_changed {
        deferred_passthroughs.clear();
    }
}

#[cfg(any(unix, windows))]
pub(super) async fn flush_deferred_passthroughs(
    stream: &LocalStream,
    current_target: &OpenAttachTarget,
    deferred_passthroughs: &mut Vec<TerminalPassthrough>,
    persistent_overlay_visible: bool,
    persistent_overlay_cached: bool,
) -> io::Result<()> {
    if persistent_overlay_visible || persistent_overlay_cached {
        return Ok(());
    }
    let frame = take_passthrough_frame(current_target, deferred_passthroughs);
    if frame.is_empty() {
        return Ok(());
    }
    emit_attach_bytes(stream, &frame).await
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use rmux_core::TerminalPassthrough;

    use super::clear_deferred_passthroughs_if_target_changed;

    #[test]
    fn target_change_discards_deferred_passthroughs() {
        let mut deferred = vec![TerminalPassthrough::kitty_graphics(
            0,
            0,
            b"Gf=100;AAAA".to_vec(),
        )];

        clear_deferred_passthroughs_if_target_changed(true, &mut deferred);

        assert!(deferred.is_empty());
    }

    #[test]
    fn same_target_keeps_deferred_passthroughs() {
        let mut deferred = vec![TerminalPassthrough::kitty_graphics(
            0,
            0,
            b"Gf=100;AAAA".to_vec(),
        )];

        clear_deferred_passthroughs_if_target_changed(false, &mut deferred);

        assert_eq!(deferred.len(), 1);
    }
}
