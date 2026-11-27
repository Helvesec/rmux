//! C12: a second `display-popup` leaves a live first popup and its job untouched.
//!
//! Measured on tmux 3.7b with a real attached client: while a first popup's job runs, another
//! `display-popup` for the same client succeeds at once, prints nothing, runs no job and checks
//! no size (`-w 500` succeeds too). The first job keeps its input, its command still waits and
//! then reports the job's own status; once it has closed, a new popup opens normally.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rmux_proto::Response;
use tokio::time::{sleep, timeout};

use super::super::attach_support::ActiveAttachIdentity;
use super::super::RequestHandler;
use super::completion_tests::{
    blocking_job, cli_command, exit_status, release_path, scene, wait_for_popup, ATTACH_PID,
};

async fn popup_response(handler: &RequestHandler, size: &str, marker: &Path) -> Response {
    timeout(
        Duration::from_secs(10),
        handler.handle(cli_command(format!(
            "display-popup -c {ATTACH_PID} {size} -E \"touch {}\"",
            marker.display()
        ))),
    )
    .await
    .expect("a popup command that opens nothing answers without waiting")
}

async fn popup_touching(handler: &RequestHandler, size: &str, marker: &Path) -> Option<i32> {
    exit_status(&popup_response(handler, size, marker).await)
}

fn stderr_text(response: &Response) -> String {
    match response {
        Response::SourceFile(source) => String::from_utf8_lossy(source.stderr()).into_owned(),
        other => panic!("expected the queued command response, got {other:?}"),
    }
}

async fn client_identity(handler: &RequestHandler) -> ActiveAttachIdentity {
    let active_attach = handler.active_attach.lock().await;
    active_attach
        .by_pid
        .get(&ATTACH_PID)
        .expect("the attached client")
        .identity(ATTACH_PID)
}

/// Whether the client's popup admission is reserved right now.
async fn admission_reserved(handler: &RequestHandler) -> bool {
    let admissions = std::sync::Arc::clone(&handler.state.lock().await.popup_admissions);
    let reserved = admissions
        .lock()
        .expect("admission registry")
        .contains(&ATTACH_PID);
    reserved
}

#[tokio::test]
async fn a_second_popup_leaves_the_live_first_popup_and_its_job_untouched() {
    let (handler, _control_rx) = scene("c12-second").await;
    let release = release_path("c12-first");
    let second_marker = release_path("c12-second");
    let oversized_marker = release_path("c12-oversized");
    let after_marker = release_path("c12-after");
    let first = tokio::spawn({
        let handler = handler.clone();
        let command = format!(
            "display-popup -c {ATTACH_PID} -w 20 -h 5 -E \"{}\"",
            blocking_job(&release, 3)
        );
        async move { handler.handle(cli_command(command)).await }
    });
    wait_for_popup(&handler).await;

    assert_eq!(
        popup_touching(&handler, "-w 20 -h 5", &second_marker).await,
        None,
        "the second popup command succeeds without opening"
    );
    assert_eq!(
        popup_touching(&handler, "-w 500 -h 5", &oversized_marker).await,
        None,
        "no size is checked while the first popup is open"
    );
    sleep(Duration::from_millis(200)).await;
    assert!(
        !first.is_finished(),
        "the first command still waits on its popup"
    );
    assert!(handler.overlay_active(ATTACH_PID).await);

    std::fs::write(&release, b"go").expect("release the first popup job");
    let first = timeout(Duration::from_secs(10), first)
        .await
        .expect("the first command completes when its job exits")
        .expect("the first command task");
    let _ = std::fs::remove_file(&release);
    assert_eq!(
        exit_status(&first),
        Some(3),
        "the first job ran to its own end, it was never replaced"
    );

    assert_eq!(
        popup_touching(&handler, "-w 20 -h 5", &after_marker).await,
        None
    );
    assert!(
        after_marker.exists(),
        "a popup opens again once the first has closed"
    );
    assert!(
        !second_marker.exists(),
        "the second popup never ran its job"
    );
    assert!(
        !oversized_marker.exists(),
        "the oversized popup never ran its job"
    );
    let _ = std::fs::remove_file(&after_marker);
}

/// Checks that a skipped admission built nothing and freed the client: no overlay, no
/// reservation, no job, and the next popup opens and runs its job.
async fn next_popup_opens_after(handler: &RequestHandler, skipped: &Path, label: &str) {
    assert!(!handler.overlay_active(ATTACH_PID).await, "{label}");
    assert!(!admission_reserved(handler).await, "{label}: released");
    let next_marker = release_path(&format!("{label}-next"));
    assert_eq!(
        popup_touching(handler, "-w 20 -h 5", &next_marker).await,
        None
    );
    assert!(next_marker.exists(), "{label}: the next popup runs its job");
    assert!(!skipped.exists(), "{label}: the skipped popup ran nothing");
    let _ = std::fs::remove_file(&next_marker);
}

/// An admission refused by its size (the error branch) frees the client.
#[tokio::test]
async fn a_refused_size_frees_the_client_for_the_next_popup() {
    let (handler, _control_rx) = scene("c12-release-refused").await;
    let skipped = release_path("c12-release-refused");
    let refused = popup_response(&handler, "-w 500 -h 5", &skipped).await;
    assert_eq!(exit_status(&refused), Some(1), "{refused:?}");
    assert_eq!(stderr_text(&refused).trim_end(), "width too large");
    next_popup_opens_after(&handler, &skipped, "c12-release-refused").await;
}

/// An admission that builds no popup (no content cell, the empty branch) frees the client.
#[tokio::test]
async fn an_empty_popup_frees_the_client_for_the_next_popup() {
    let (handler, _control_rx) = scene("c12-release-empty").await;
    let skipped = release_path("c12-release-empty");
    let empty = popup_response(&handler, "-w 2 -h 5", &skipped).await;
    assert_eq!(exit_status(&empty), None, "{empty:?}");
    assert_eq!(stderr_text(&empty), "");
    next_popup_opens_after(&handler, &skipped, "c12-release-empty").await;
}

/// R3: only the admission that reserved the client releases it. While A holds its admission
/// (built but not installed yet), B and then C are refused; A's release lets D in.
#[tokio::test]
async fn only_the_admission_holder_releases_the_client() {
    let (handler, _control_rx) = scene("c12-holder").await;
    let identity = client_identity(&handler).await;

    let holder = handler
        .admit_display_popup(identity)
        .await
        .expect("A is admitted");
    assert!(admission_reserved(&handler).await, "A holds the client");
    assert!(
        handler.admit_display_popup(identity).await.is_none(),
        "B is refused while A holds"
    );
    assert!(
        admission_reserved(&handler).await,
        "B's refusal keeps A's reservation"
    );
    assert!(
        handler.admit_display_popup(identity).await.is_none(),
        "C is still refused while A holds"
    );
    drop(holder);
    assert!(!admission_reserved(&handler).await, "A's release frees it");
    assert!(
        handler.admit_display_popup(identity).await.is_some(),
        "D is admitted once A has released"
    );
}

/// R3 at the queue: while an admission holds the client, queued CLI popups B and C each
/// succeed at once without a job or an overlay and leave the reservation in place; once it is
/// released, the queued popup D opens and runs its job.
#[tokio::test]
async fn queued_popups_stay_out_while_an_admission_holds_the_client() {
    let (handler, _control_rx) = scene("c12-queued-holder").await;
    let identity = client_identity(&handler).await;
    let holder = handler
        .admit_display_popup(identity)
        .await
        .expect("A is admitted");
    assert!(admission_reserved(&handler).await, "A holds the client");

    for label in ["c12-queued-b", "c12-queued-c"] {
        let marker = release_path(label);
        let response = popup_response(&handler, "-w 20 -h 5", &marker).await;
        assert_eq!(exit_status(&response), None, "{label}: {response:?}");
        assert_eq!(stderr_text(&response), "", "{label}");
        sleep(Duration::from_millis(500)).await;
        assert!(!marker.exists(), "{label}: no job ran");
        assert!(!handler.overlay_active(ATTACH_PID).await, "{label}");
        assert!(
            admission_reserved(&handler).await,
            "{label}: the holder keeps the client"
        );
    }

    drop(holder);
    let marker = release_path("c12-queued-d");
    assert_eq!(popup_touching(&handler, "-w 20 -h 5", &marker).await, None);
    assert!(marker.exists(), "D opens and runs its job once released");
    let _ = std::fs::remove_file(&marker);
}

// Refused requests can finish before the admitted shell publishes its marker. Wait for that
// event, not a fixed scheduling allowance; the timeout only bounds a missing job's failure.
async fn wait_for_started_job(markers: &[PathBuf]) {
    timeout(Duration::from_secs(10), async {
        while !markers.iter().any(|marker| marker.exists()) {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the admitted popup job publishes its marker");
}

/// R3 with three real queued requests released together behind the ordinary state lock (no
/// internal acknowledgement: only "all in flight, no effect yet" is checked before release).
/// Request i owns marker i. The refused requests must not free the winner's admission, so
/// exactly one job ever runs, both others succeed at once, and the winner reports its status.
#[tokio::test]
async fn three_concurrent_popups_for_one_client_admit_one_and_spawn_no_other_job() {
    let (handler, _control_rx) = scene("c12-three").await;
    let release = release_path("c12-three-release");
    let markers: Vec<_> = ["a", "b", "c"]
        .iter()
        .map(|name| release_path(&format!("c12-three-{name}")))
        .collect();
    let gate = handler.state.lock().await;
    let requests: Vec<_> = markers
        .iter()
        .map(|marker| {
            let handler = handler.clone();
            let command = format!(
                "display-popup -c {ATTACH_PID} -w 20 -h 5 -E \
                 \"sh -c 'touch {}; while [ ! -e {} ]; do sleep 0.02; done; exit 3'\"",
                marker.display(),
                release.display()
            );
            tokio::spawn(async move { handler.handle(cli_command(command)).await })
        })
        .collect();
    sleep(Duration::from_millis(200)).await;
    assert!(
        requests.iter().all(|request| !request.is_finished())
            && markers.iter().all(|marker| !marker.exists()),
        "all three requests are in flight before any admission effect"
    );
    drop(gate);

    timeout(Duration::from_secs(5), async {
        while requests
            .iter()
            .filter(|request| request.is_finished())
            .count()
            < 2
        {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("two requests complete without waiting");
    wait_for_started_job(&markers).await;
    let started: Vec<usize> = (0..3).filter(|&index| markers[index].exists()).collect();
    assert_eq!(started.len(), 1, "exactly one popup job ran: {started:?}");
    let winner = started[0];
    let mut requests: Vec<_> = requests.into_iter().map(Some).collect();
    for (index, request) in requests.iter_mut().enumerate() {
        if index == winner {
            continue;
        }
        let request = request.take().expect("a refused request");
        assert!(request.is_finished(), "request {index} did not wait");
        let response = request.await.expect("refused request task");
        assert_eq!(
            exit_status(&response),
            None,
            "request {index}: {response:?}"
        );
    }
    assert!(handler.overlay_active(ATTACH_PID).await);

    std::fs::write(&release, b"go").expect("release the admitted popup job");
    let admitted = requests[winner].take().expect("the admitted request");
    let admitted = timeout(Duration::from_secs(10), admitted)
        .await
        .expect("the admitted request completes when its job exits")
        .expect("admitted request task");
    assert_eq!(
        exit_status(&admitted),
        Some(3),
        "request {winner} ran its own job"
    );
    assert_eq!(
        (0..3).filter(|&index| markers[index].exists()).count(),
        1,
        "no other job ever ran"
    );
    let _ = std::fs::remove_file(&release);
    for marker in &markers {
        let _ = std::fs::remove_file(marker);
    }
}

/// C12-A01: two popups for one client in flight at once, before either is installed.
///
/// The test holds the ordinary state lock as a start gate, so both queued requests wait before
/// any admission effect; once released they advance in turn on that lock, and each reaches the
/// overlay check while the other is still being built. As tmux does, exactly one popup opens
/// and runs its job; the other request succeeds at once without a job.
#[tokio::test]
async fn concurrent_popups_for_one_client_admit_one_and_spawn_no_second_job() {
    let (handler, _control_rx) = scene("c12-concurrent").await;
    let release = release_path("c12-concurrent-release");
    let markers = [
        release_path("c12-concurrent-a"),
        release_path("c12-concurrent-b"),
    ];
    let gate = handler.state.lock().await;
    let requests: Vec<_> = markers
        .iter()
        .map(|marker| {
            let handler = handler.clone();
            let command = format!(
                "display-popup -c {ATTACH_PID} -w 20 -h 5 -E \
                 \"sh -c 'touch {}; while [ ! -e {} ]; do sleep 0.02; done; exit 3'\"",
                marker.display(),
                release.display()
            );
            tokio::spawn(async move { handler.handle(cli_command(command)).await })
        })
        .collect();
    sleep(Duration::from_millis(200)).await;
    assert!(
        requests.iter().all(|request| !request.is_finished())
            && markers.iter().all(|marker| !marker.exists()),
        "both requests are in flight before any admission effect"
    );
    drop(gate);

    timeout(Duration::from_secs(5), async {
        while !requests.iter().any(|request| request.is_finished()) {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("one request completes without waiting");
    wait_for_started_job(&markers).await;
    let started: Vec<_> = markers.iter().filter(|marker| marker.exists()).collect();
    assert_eq!(started.len(), 1, "exactly one popup job was spawned");
    let (finished, waiting): (Vec<_>, Vec<_>) = requests
        .into_iter()
        .partition(|request| request.is_finished());
    assert_eq!((finished.len(), waiting.len()), (1, 1));
    let loser = finished.into_iter().next().expect("the finished request");
    let loser = loser.await.expect("the finished request task");
    assert_eq!(exit_status(&loser), None, "the other request succeeds");
    assert!(handler.overlay_active(ATTACH_PID).await);

    std::fs::write(&release, b"go").expect("release the admitted popup job");
    let winner = waiting.into_iter().next().expect("the waiting request");
    let winner = timeout(Duration::from_secs(10), winner)
        .await
        .expect("the admitted request completes when its job exits")
        .expect("the admitted request task");
    assert_eq!(
        exit_status(&winner),
        Some(3),
        "the admitted job ran to its own end"
    );
    assert_eq!(
        markers.iter().filter(|marker| marker.exists()).count(),
        1,
        "no second job ever ran"
    );

    let after_marker = release_path("c12-concurrent-after");
    assert_eq!(
        popup_touching(&handler, "-w 20 -h 5", &after_marker).await,
        None
    );
    assert!(
        after_marker.exists(),
        "a popup opens again once the admitted one has closed"
    );
    let _ = std::fs::remove_file(&release);
    let _ = std::fs::remove_file(&after_marker);
    for marker in &markers {
        let _ = std::fs::remove_file(marker);
    }
}
