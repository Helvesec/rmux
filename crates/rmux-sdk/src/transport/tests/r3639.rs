//! R3639: `send` returning only means the command sits in the bounded command
//! queue's buffer. Until the actor admits it, the command has no FIFO response
//! slot and no bytes on the wire, so its expiry or cancellation must fail only
//! its own caller and leave the shared transport usable.

use super::*;

const CAPACITY: usize = super::super::ACTOR_QUEUE_CAPACITY;
/// Bytes that fill the client-to-peer side of the fixture duplex before the
/// actor starts, so no request byte is written until the peer drains them.
const STALL_BYTES: usize = 64;

/// Every caller count for which the actor holds exactly `CAPACITY` unwritten
/// frames and the next caller's command waits in the buffer, behind at least
/// one earlier command, with a free slot so its `send` does not wait.
fn buffered_caller_counts() -> std::ops::RangeInclusive<usize> {
    CAPACITY + 1..=2 * CAPACITY - 1
}

#[derive(Clone, Copy, Debug)]
enum ShortCaller {
    /// A request whose 100 ms deadline expires in the buffer.
    Request,
    /// An armed SDK wait whose 100 ms deadline expires in the buffer.
    ArmedRequest,
    /// An unbounded armed SDK wait whose task is aborted while in the buffer.
    DroppedArmedRequest,
}

struct StalledTransport {
    client: TransportClient,
    peer_stream: tokio::io::DuplexStream,
    callers: Vec<(usize, JoinHandle<crate::Result<Response>>)>,
}

/// Starts `count` unbounded callers on a peer that has not read anything.
async fn stall_callers(count: usize) -> StalledTransport {
    let (mut client_stream, peer_stream) = tokio::io::duplex(STALL_BYTES);
    client_stream
        .write_all(&[0; STALL_BYTES])
        .await
        .expect("stall bytes fill the fixture duplex");
    let client = TransportClient::spawn(client_stream).with_default_timeout(None);
    let callers: Vec<_> = (0..count)
        .map(|index| {
            (
                index,
                spawn_request(&client, numbered_list_sessions_request(index)),
            )
        })
        .collect();
    // With paused time, this sleep returns only once every task is idle.
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert_eq!(
        client.commands.capacity(),
        2 * CAPACITY - count,
        "the actor must hold exactly {CAPACITY} unwritten frames for {count} callers"
    );
    StalledTransport {
        client,
        peer_stream,
        callers,
    }
}

/// Runs the short caller with its command in the buffer, then serves every
/// caller. Returns why the shared transport did not stay healthy, if it did not.
async fn short_caller_outcome(count: usize, short_caller: ShortCaller) -> Result<(), String> {
    let StalledTransport {
        client,
        mut peer_stream,
        callers,
    } = stall_callers(count).await;
    let short = client.with_default_timeout(Some(Duration::from_millis(100)));
    let wait_id = SdkWaitId::new(42);
    match short_caller {
        ShortCaller::Request => {
            expect_timed_out(short.request(has_session_request()).await)?;
        }
        ShortCaller::ArmedRequest => {
            expect_timed_out(short.armed_request(sdk_wait_request(wait_id)).await)?;
        }
        ShortCaller::DroppedArmedRequest => {
            let armed = client.clone();
            let task =
                tokio::spawn(async move { armed.armed_request(sdk_wait_request(wait_id)).await });
            tokio::time::sleep(Duration::from_millis(1)).await;
            assert_eq!(
                client.commands.capacity(),
                2 * CAPACITY - count - 1,
                "the armed command must wait in the buffer"
            );
            task.abort();
            assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        }
    }
    if let Some(failure) = client.state.terminal_failure() {
        return Err(format!(
            "shared transport poisoned: {}",
            failure.to_error("r3639")
        ));
    }

    let peer = tokio::spawn(async move {
        let mut stall = [0; STALL_BYTES];
        peer_stream
            .read_exact(&mut stall)
            .await
            .expect("peer drains the stall bytes");
        serve_requests_serially(peer_stream).await;
    });
    let mut failed = Vec::new();
    for (index, caller) in callers {
        let expected = list_sessions_response(&numbered_list_sessions_stdout(&index.to_string()));
        match join_request(caller).await {
            Ok(response) if response == expected => {}
            other => failed.push((index, other.map_err(|error| error.to_string()).err())),
        }
    }
    let fresh = 4 * CAPACITY;
    let fresh_result = client.request(numbered_list_sessions_request(fresh)).await;
    client.abort();
    peer.abort();
    if !failed.is_empty() {
        return Err(format!(
            "{} unbounded callers failed, first {:?}",
            failed.len(),
            failed[0]
        ));
    }
    match fresh_result {
        Ok(response)
            if response
                == list_sessions_response(&numbered_list_sessions_stdout(&fresh.to_string())) =>
        {
            Ok(())
        }
        other => Err(format!("fresh request on the shared handle: {other:?}")),
    }
}

fn expect_timed_out<T>(result: crate::Result<T>) -> Result<(), String> {
    match result {
        Ok(_) => Err("short caller succeeded, expected TimedOut".to_owned()),
        Err(RmuxError::Transport { source, .. }) if source.kind() == io::ErrorKind::TimedOut => {
            Ok(())
        }
        Err(error) => Err(format!("short caller: expected TimedOut, got {error:?}")),
    }
}

async fn assert_buffered_short_caller_fails_alone(short_caller: ShortCaller) {
    let mut poisoned = Vec::new();
    for count in buffered_caller_counts() {
        if let Err(reason) = short_caller_outcome(count, short_caller).await {
            poisoned.push((count, reason));
        }
    }
    println!(
        "r3639 {short_caller:?}: {} of {} caller counts poisoned the shared transport",
        poisoned.len(),
        buffered_caller_counts().count()
    );
    assert!(
        poisoned.is_empty(),
        "r3639 {short_caller:?} poisoned the shared transport for caller counts {:?}; first: {:?}",
        poisoned.iter().map(|(count, _)| *count).collect::<Vec<_>>(),
        poisoned.first()
    );
}

#[tokio::test(start_paused = true)]
async fn r3639_request_deadline_expiring_in_the_command_buffer_fails_only_its_caller() {
    assert_buffered_short_caller_fails_alone(ShortCaller::Request).await;
}

#[tokio::test(start_paused = true)]
async fn r3639_armed_request_deadline_expiring_in_the_command_buffer_fails_only_its_caller() {
    assert_buffered_short_caller_fails_alone(ShortCaller::ArmedRequest).await;
}

#[tokio::test(start_paused = true)]
async fn r3639_dropping_armed_request_in_the_command_buffer_keeps_transport_usable() {
    assert_buffered_short_caller_fails_alone(ShortCaller::DroppedArmedRequest).await;
}

#[path = "r3642.rs"]
mod r3642;
