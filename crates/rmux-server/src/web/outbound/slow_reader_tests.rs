use super::*;
use std::io::Write;
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpSocket, TcpStream};

/// The peer of a connection whose kernel send path was full before the
/// writer received its record. It must drain `prefilled` filler bytes before
/// the writer's frames.
pub(crate) struct BlockedPeer {
    stream: TcpStream,
    prefilled: u64,
}

// Upper bound for the filler: far above any loopback buffer measured on the
// supported hosts, so a socket that never refuses fails the test loudly.
const SEND_PATH_FILL_MAX: u64 = 64 * 1024 * 1024;

// Small kernel buffers alone do not block a single large write everywhere.
// Windows accepts a whole non-blocking send while the queued bytes are below
// SO_SNDBUF, so a 2 MiB record completes at once through 1 KiB buffers (S1887
// probe: two 2 MiB sends accepted, then WSAEWOULDBLOCK only after a fill).
// Fill the send path until the kernel refuses, so the real record write is
// pending on every platform.
fn fill_send_path(socket: &mut std::net::TcpStream) -> u64 {
    socket.set_nonblocking(true).unwrap();
    let filler = [0u8; 64 * 1024];
    let mut prefilled = 0u64;
    loop {
        match socket.write(&filler) {
            Ok(written) => prefilled += written as u64,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return prefilled,
            Err(error) => panic!("filling the send path failed: {error}"),
        }
        assert!(
            prefilled <= SEND_PATH_FILL_MAX,
            "the send path never refused after {prefilled} bytes"
        );
    }
}

// Write deadline of a blocked writer whose test is not about the deadline.
// The published WEB_WRITE_TIMEOUT starts when the writer dequeues the record,
// inside `blocked_socket_for_test`, so a test thread starved for 2 s under
// load would see the writer stop before the peer drains it (S2018: 56326 of
// 2097178 bytes, then EOF). Only the timed-out test exercises the published
// deadline.
const BLOCKED_WRITER_TEST_DEADLINE: Duration = Duration::from_secs(60 * 60);

impl WebSocketOutbound {
    // Exercise the real encrypted writer with bounded kernel buffers and a
    // peer that stops reading after the HTTP upgrade, rather than a fake queue.
    pub(crate) async fn blocked_socket_for_test(snapshot: bool) -> (Self, BlockedPeer) {
        Self::blocked_socket_with_write_timeout_for_test(snapshot, BLOCKED_WRITER_TEST_DEADLINE)
            .await
    }

    async fn blocked_socket_with_write_timeout_for_test(
        snapshot: bool,
        write_timeout: Duration,
    ) -> (Self, BlockedPeer) {
        let socket = TcpSocket::new_v4().unwrap();
        socket.set_send_buffer_size(1024).unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let listener: TcpListener = socket.listen(1).unwrap();
        let peer = TcpSocket::new_v4().unwrap();
        peer.set_recv_buffer_size(1024).unwrap();
        let mut peer = peer.connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let stream = stream.into_std().unwrap();
        let mut send_path = stream.try_clone().unwrap();
        let stream = TcpStream::from_std(stream).unwrap();
        let socket = super::super::websocket::WebSocket::accept(stream, "rmux-slow-reader")
            .await
            .unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            header.push(peer.read_u8().await.unwrap());
        }
        let prefilled = fill_send_path(&mut send_path);
        drop(send_path);
        let (_, writer) = socket.split();
        let (_, sealer) = super::super::crypto::derive_server_crypto(
            &[1; 32], &[2; 32], &[3; 32], b"client", b"server",
        )
        .unwrap();
        let outbound = Self::spawn_with_write_timeout(writer, sealer, write_timeout);
        let bytes = vec![b'x'; WEB_OUTBOUND_BYTES_MAX];
        assert_eq!(
            if snapshot {
                outbound.queue_snapshot(bytes)
            } else {
                outbound.queue_frame(bytes)
            },
            OutboundQueueResult::Queued
        );
        tokio::time::timeout(Duration::from_millis(500), async {
            while outbound.tx.capacity() != VIEWER_CHANNEL_CAP {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("writer must dequeue the frame");
        assert!(
            !outbound.writer_task.is_finished(),
            "real writer remains blocked"
        );
        (
            outbound,
            BlockedPeer {
                stream: peer,
                prefilled,
            },
        )
    }
}

const DRAINING_PEER_RECV_BUFFER: u32 = 1024 * 1024;

// Linux honours the 1 KiB receive buffer that blocks the writer, so draining
// the 2 MiB record through it repeatedly advertises a zero window and waits on
// the kernel's delayed window update, about 200 ms per stall. Widen the window
// only after the blocked write has been established. Keep the narrow buffer:
// without it, delayed ACKs against the 1 KiB send buffer make the Linux drain
// take 655-738 ms.
fn widen_receive_window(peer: TcpStream) -> TcpStream {
    let peer = peer.into_std().unwrap();
    TcpSocket::from_std_stream(peer.try_clone().unwrap())
        .set_recv_buffer_size(DRAINING_PEER_RECV_BUFFER)
        .unwrap();
    TcpStream::from_std(peer).unwrap()
}

#[tokio::test]
async fn web_failed_data_write_releases_in_flight_charge() {
    for snapshot in [false, true] {
        let (mut outbound, peer) = WebSocketOutbound::blocked_socket_for_test(snapshot).await;
        drop(peer);
        tokio::time::timeout(Duration::from_millis(500), &mut outbound.writer_task)
            .await
            .expect("closed peer terminates writer")
            .unwrap();
        assert_eq!(outbound.backlog_bytes.load(Ordering::Acquire), 0);
    }
}

#[tokio::test]
async fn web_cancelled_data_write_releases_in_flight_charge() {
    for snapshot in [false, true] {
        let (mut outbound, _peer) = WebSocketOutbound::blocked_socket_for_test(snapshot).await;
        let backlog = outbound.backlog_bytes.clone();
        outbound.writer_task.abort();
        assert!((&mut outbound.writer_task)
            .await
            .as_mut()
            .unwrap_err()
            .is_cancelled());
        assert_eq!(backlog.load(Ordering::Acquire), 0);
    }
}

#[tokio::test]
async fn web_timed_out_data_write_releases_in_flight_charge() {
    for snapshot in [false, true] {
        let (mut outbound, _peer) = WebSocketOutbound::blocked_socket_with_write_timeout_for_test(
            snapshot,
            WEB_WRITE_TIMEOUT,
        )
        .await;
        tokio::time::timeout(Duration::from_secs(3), &mut outbound.writer_task)
            .await
            .expect("published write timeout terminates writer")
            .unwrap();
        assert_eq!(outbound.backlog_bytes.load(Ordering::Acquire), 0);
    }
}

#[tokio::test]
async fn web_in_flight_snapshot_excludes_another_data_budget() {
    let (outbound, _peer) = WebSocketOutbound::blocked_socket_for_test(true).await;
    assert_eq!(
        outbound.backlog_bytes.load(Ordering::Acquire),
        WEB_OUTBOUND_BYTES_MAX,
        "a blocked snapshot remains charged until the actual write ends"
    );
    assert_eq!(
        outbound.queue_snapshot(vec![0]),
        OutboundQueueResult::Backpressure
    );
    assert_eq!(
        outbound.queue_frame(vec![0]),
        OutboundQueueResult::Backpressure
    );
}

#[tokio::test]
async fn web_completed_data_write_leaves_a_live_writer_without_in_flight_charge() {
    for snapshot in [false, true] {
        let (outbound, peer) = WebSocketOutbound::blocked_socket_for_test(snapshot).await;
        let prefilled = peer.prefilled;
        let mut peer = widen_receive_window(peer.stream);
        assert_eq!(
            outbound.backlog_bytes.load(Ordering::Acquire),
            WEB_OUTBOUND_BYTES_MAX,
            "snapshot={snapshot}: the blocked write keeps its charge until the peer drains it"
        );
        tokio::time::timeout(Duration::from_millis(500), async {
            let filler = tokio::io::copy(&mut (&mut peer).take(prefilled), &mut tokio::io::sink())
                .await
                .unwrap();
            assert_eq!(filler, prefilled);
            // Observe the complete encrypted record at the peer. Task liveness
            // alone cannot distinguish a pending write from an idle writer.
            assert_eq!(peer.read_u8().await.unwrap(), 0x82);
            assert_eq!(peer.read_u8().await.unwrap(), 127);
            let len = peer.read_u64().await.unwrap();
            assert!(len > WEB_OUTBOUND_BYTES_MAX as u64);
            let received = tokio::io::copy(&mut (&mut peer).take(len), &mut tokio::io::sink())
                .await
                .unwrap();
            assert_eq!(received, len);
            // A control acknowledgement orders the observation after the data
            // handler has returned and released its reservation.
            outbound.write_ping(b"completed").await.unwrap();
        })
        .await
        .expect("reading peer completes the data write and control acknowledgement");

        assert_eq!(outbound.tx.capacity(), VIEWER_CHANNEL_CAP);
        assert!(!outbound.writer_task.is_finished());
        assert_eq!(outbound.backlog_bytes.load(Ordering::Acquire), 0);
        assert_eq!(outbound.queue_frame(vec![0]), OutboundQueueResult::Queued);
    }
}
