use super::*;

// The deadline is a Tokio deadline, so this scenario runs on paused Tokio
// time that the test steps 1 ms at a time. The attach loop runs between
// steps: the growth frame is therefore always dispatched 300 ms before the
// deadline its first byte armed, and `u` always arrives 100 ms after that
// deadline. With the wall clock, host load could delay the test's own wakeups
// or the forwarder's read past the 500 ms escape-time; the product then
// correctly expired the frame before the growth arrived, and the test failed.
// The product reads the same clock, so a restarted, extended, late or
// unserved deadline still fails here.
#[tokio::test]
async fn kitty_rereview_r3_bounded_quarantine_keeps_deadline_on_both_attach_paths() {
    for (name, prefix, growth) in [
        (
            "escape",
            "\x1b".to_owned(),
            format!("[97;x{}", "0".repeat(4100)).into_bytes(),
        ),
        (
            "csi",
            "\x1b[".to_owned(),
            format!("97;x{}", "0".repeat(4100)).into_bytes(),
        ),
        (
            "crossing",
            format!("\x1b[97;x{}", "0".repeat(4080)),
            vec![b'0'; 30],
        ),
        (
            "numeric",
            format!("\x1b[97;{}", "0".repeat(4100)),
            vec![b'0'],
        ),
        (
            "invalid",
            format!("\x1b[97;x{}", "0".repeat(4100)),
            vec![b'0'],
        ),
    ] {
        for keystroke in [false, true] {
            let mut fixture =
                PendingEscapeSchedulerFixture::start(&format!("kitty-bound-{name}-{keystroke}"))
                    .await;
            let message = |bytes| {
                if keystroke {
                    AttachMessage::Keystroke(AttachedKeystroke::new(bytes))
                } else {
                    AttachMessage::Data(bytes)
                }
            };
            tokio::time::pause();
            fixture
                .send(message([b"A".as_slice(), prefix.as_bytes()].concat()))
                .await;
            fixture
                .wait_for_capture_on_paused_clock(|bytes| bytes == b"A", "initial bounded frame")
                .await;
            step_paused_clock(Duration::from_millis(300)).await;
            fixture.send(message(growth.clone())).await;
            step_paused_clock(Duration::from_millis(300)).await;
            fixture.send(message(b"u".to_vec())).await;
            fixture
                .wait_for_capture_on_paused_clock(
                    |bytes| bytes == b"Au",
                    "original bounded-frame deadline",
                )
                .await;
            tokio::time::resume();
            fixture.finish().await;
        }
    }
}
