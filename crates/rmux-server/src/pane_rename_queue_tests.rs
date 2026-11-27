//! The three properties the sequence owns: order across panes, a real bound
//! with backpressure, and an exit that takes a prefix rather than its own
//! pane's requests alone.

use std::sync::Arc;

use super::*;

fn session() -> SessionName {
    SessionName::new("rename-queue").expect("valid session name")
}

fn accepted(owner: u32, name: &str) -> AcceptedWindowRename {
    AcceptedWindowRename {
        owner: WindowId::new(owner),
        name: name.to_owned(),
    }
}

fn push(queue: &PaneRenameQueue, pane: u32, name: &str) {
    let admission = queue.try_admit().expect("the sequence has room");
    queue.push(
        &session(),
        PaneId::new(pane),
        Some(1),
        vec![accepted(7, name)],
    );
    drop(admission);
}

fn names(drained: &[QueuedWindowRename]) -> Vec<&str> {
    drained
        .iter()
        .map(|request| request.name.as_str())
        .collect()
}

#[test]
fn requests_keep_the_order_they_were_accepted_in_across_panes() {
    let queue = PaneRenameQueue::new();

    push(&queue, 1, "A1");
    push(&queue, 2, "B1");
    push(&queue, 1, "A2");

    let drained = queue.drain();
    assert_eq!(
        names(&drained),
        vec!["A1", "B1", "A2"],
        "one sequence, not one group per pane"
    );
    assert_eq!(
        drained
            .iter()
            .map(|r| r.pane_id.as_u32())
            .collect::<Vec<_>>(),
        vec![1, 2, 1]
    );
}

#[test]
fn a_full_sequence_refuses_the_next_producer_until_it_is_drained() {
    let queue = PaneRenameQueue::with_capacity(2, 1024);

    push(&queue, 1, "one");
    push(&queue, 1, "two");

    assert!(
        queue.try_admit().is_none(),
        "the count limit stops the next feed before it produces anything"
    );
    assert_eq!(queue.stats().pending, 2);

    let drained = queue.drain();

    assert_eq!(names(&drained), vec!["one", "two"]);
    assert_eq!(queue.stats().pending, 0);
    assert!(
        queue.try_admit().is_some(),
        "the drain returned the capacity"
    );
}

#[test]
fn the_byte_limit_stops_a_producer_the_count_alone_would_admit() {
    let queue = PaneRenameQueue::with_capacity(1024, 8);

    push(&queue, 1, "0123456789");

    assert_eq!(queue.stats().pending, 1, "the request itself is never lost");
    assert_eq!(queue.stats().pending_bytes, 10);
    assert!(
        queue.try_admit().is_none(),
        "one long name can fill the sequence without filling its count"
    );

    let _ = queue.drain();

    assert_eq!(queue.stats().pending_bytes, 0);
    assert!(queue.try_admit().is_some());
}

#[test]
fn an_admission_that_accepts_nothing_returns_its_capacity() {
    let queue = PaneRenameQueue::with_capacity(1, 1024);

    {
        let _admission = queue.try_admit().expect("room");
        assert!(
            queue.try_admit().is_none(),
            "an admitted feed counts against the limit while it runs"
        );
    }

    assert!(
        queue.try_admit().is_some(),
        "a feed that accepted no rename must not leak capacity"
    );
}

#[tokio::test]
async fn a_waiting_producer_resumes_when_the_consumer_drains_without_loss() {
    let queue = Arc::new(PaneRenameQueue::with_capacity(1, 1024));

    push(&queue, 1, "first");
    assert!(queue.try_admit().is_none(), "the sequence is full");

    let producer_queue = Arc::clone(&queue);
    let producer = tokio::spawn(async move {
        let admission = producer_queue.admit().await;
        producer_queue.push(
            &session(),
            PaneId::new(2),
            Some(1),
            vec![accepted(7, "second")],
        );
        drop(admission);
    });

    // The producer is parked on capacity, so nothing of its own is stored yet.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(queue.stats().pending, 1);
    let first = queue.drain();
    producer.await.expect("the producer resumes");

    assert_eq!(names(&first), vec!["first"]);
    assert_eq!(
        names(&queue.drain()),
        vec!["second"],
        "the held request is stored once, neither lost nor duplicated"
    );
}

#[test]
fn an_exit_takes_the_prefix_through_its_own_generation() {
    let queue = PaneRenameQueue::new();

    push(&queue, 1, "A1");
    push(&queue, 2, "B1");
    push(&queue, 1, "A2");
    push(&queue, 2, "B2");

    let drained = queue.drain_through(PaneId::new(1), Some(1));

    assert_eq!(
        names(&drained),
        vec!["A1", "B1", "A2"],
        "the exiting pane cannot overtake a request accepted before its own"
    );
    assert_eq!(
        names(&queue.drain()),
        vec!["B2"],
        "everything accepted after it stays for the ordinary flush"
    );
}

#[test]
fn an_exit_prefix_for_a_pane_with_nothing_pending_takes_nothing() {
    let queue = PaneRenameQueue::new();

    push(&queue, 1, "A1");

    assert!(queue.drain_through(PaneId::new(2), Some(1)).is_empty());
    assert!(
        queue.drain_through(PaneId::new(1), Some(2)).is_empty(),
        "a different generation is a different pane run"
    );
    assert_eq!(names(&queue.drain()), vec!["A1"]);
}

#[test]
fn a_prefix_drain_returns_only_the_capacity_it_took() {
    let queue = PaneRenameQueue::with_capacity(4, 1024);

    push(&queue, 1, "AAAA");
    push(&queue, 2, "BBBB");

    let drained = queue.drain_through(PaneId::new(1), Some(1));

    assert_eq!(names(&drained), vec!["AAAA"]);
    let stats = queue.stats();
    assert_eq!(stats.pending, 1);
    assert_eq!(stats.pending_bytes, 4, "only the taken bytes are released");
}
