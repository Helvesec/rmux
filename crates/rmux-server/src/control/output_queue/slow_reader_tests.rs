use std::pin::Pin;
use std::task::{Context, Poll};

use super::*;

struct LostWriter {
    kind: io::ErrorKind,
    fail_on_flush: bool,
}

impl AsyncWrite for LostWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.fail_on_flush {
            Poll::Ready(Ok(bytes.len()))
        } else {
            Poll::Ready(Err(io::Error::new(self.kind, "reader disconnected")))
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::new(self.kind, "reader disconnected")))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

#[tokio::test]
async fn slow_reader_isolation_lost_control_rejection_never_reports_success() {
    // A rejected input has no accepted command frame to drain after EOF.
    // Losing either rejection write or flush must preserve the transport error.
    for kind in [
        io::ErrorKind::BrokenPipe,
        io::ErrorKind::ConnectionAborted,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::NotConnected,
        io::ErrorKind::UnexpectedEof,
    ] {
        for fail_on_flush in [false, true] {
            let mut input = Vec::new();
            let mut lines = VecDeque::new();
            let mut queued_bytes = 0;
            let mut writer = LostWriter {
                kind,
                fail_on_flush,
            };
            let error = super::super::input::append_control_input_with_rejection(
                &mut input,
                &mut lines,
                &mut queued_bytes,
                &vec![b'x'; super::super::input::MAX_CONTROL_LINE_BYTES + 1],
                &mut writer,
            )
            .await
            .expect_err("lost rejection must never become silent success");
            assert_eq!(error.kind(), kind, "flush boundary: {fail_on_flush}");
        }
    }
}
