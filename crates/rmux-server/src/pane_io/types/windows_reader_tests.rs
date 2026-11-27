use super::*;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

fn suspended_sender() -> (PaneOutputSender, PaneOutputAttachedClientReadDemandGuard) {
    let registry = PaneOutputReadDemandRegistry::default();
    let sender = pane_output_channel_with_read_demand(registry.clone(), 4, 64);
    let client = registry.acquire_attached_client();
    client.set_pane_off(13, true);
    sender.set_generation(1);
    (sender, client)
}

fn parked_waiter(
    sender: &PaneOutputSender,
    generation: u64,
) -> (mpsc::Receiver<bool>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let copy = sender.clone();
    let thread = thread::spawn(move || {
        let _ = tx.send(copy.wait_for_reader_demand_blocking(13, Some(generation)));
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    while sender.reader_probe_for_test().parked() == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        sender.reader_probe_for_test().parked(),
        1,
        "Windows condvar wait reached"
    );
    (rx, thread)
}

#[test]
fn windows_blocked_reader_retires_on_explicit_removal() {
    let (sender, _client) = suspended_sender();
    let (rx, thread) = parked_waiter(&sender, 1);
    sender.retire_reader();
    let result = rx.recv_timeout(Duration::from_secs(1));
    assert!(!result.unwrap());
    thread.join().unwrap();
}

#[test]
fn windows_blocked_reader_retires_on_generation_replacement() {
    let (sender, _client) = suspended_sender();
    let (rx, thread) = parked_waiter(&sender, 1);
    sender.reset_generation(2);
    let result = rx.recv_timeout(Duration::from_secs(1));
    sender.retire_reader();
    assert!(!result.unwrap());
    thread.join().unwrap();
}

#[test]
fn windows_exit_drain_is_fenced_by_generation_and_retirement() {
    let (sender, _client) = suspended_sender();
    sender.drain_reader_for_generation(Some(1));
    assert!(
        !sender.reader_demanded(13),
        "exit drain does not change client OFF demand"
    );
    assert!(sender.wait_for_reader_demand_blocking(13, Some(1)));
    sender.retire_reader_for_generation(Some(1));
    sender.drain_reader_for_generation(Some(1));
    assert!(
        !sender.wait_for_reader_demand_blocking(13, Some(1)),
        "late watcher cannot reopen retirement"
    );

    sender.reset_generation(2);
    let (rx, thread) = parked_waiter(&sender, 2);
    sender.drain_reader_for_generation(Some(1));
    sender.retire_reader_for_generation(Some(1));
    let stale = rx.recv_timeout(Duration::from_millis(30));
    sender.drain_reader_for_generation(Some(2));
    let current = rx.recv_timeout(Duration::from_secs(1));
    sender.retire_reader_for_generation(Some(2));
    thread.join().unwrap();
    assert!(matches!(stale, Err(mpsc::RecvTimeoutError::Timeout)));
    assert!(current.unwrap());

    sender.set_generation(3);
    let (rx, thread) = parked_waiter(&sender, 3);
    sender.drain_reader_for_generation(Some(2));
    let stale = rx.recv_timeout(Duration::from_millis(30));
    sender.retire_reader();
    let removed = rx.recv_timeout(Duration::from_secs(1));
    thread.join().unwrap();
    assert!(matches!(stale, Err(mpsc::RecvTimeoutError::Timeout)));
    assert!(!removed.unwrap());
}
