use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use super::super::super::RequestHandler;

#[derive(Debug, Default)]
pub(crate) struct QueueExactTargetCapturePause {
    pub(crate) reached: Notify,
    pub(crate) release: Notify,
}

static PAUSES: Mutex<Vec<(usize, String, Arc<QueueExactTargetCapturePause>)>> =
    Mutex::new(Vec::new());

pub(crate) fn install_queue_exact_target_capture_pause(
    handler: &RequestHandler,
    command_name: &str,
) -> Arc<QueueExactTargetCapturePause> {
    let handler_id = Arc::as_ptr(&handler.state) as usize;
    let pause = Arc::new(QueueExactTargetCapturePause::default());
    let mut pauses = PAUSES.lock().expect("queue exact-target pause lock");
    pauses.retain(|(candidate_handler, candidate, _)| {
        *candidate_handler != handler_id || candidate != command_name
    });
    pauses.push((handler_id, command_name.to_owned(), Arc::clone(&pause)));
    pause
}

pub(crate) async fn pause_after_queue_exact_target_capture(
    handler: &RequestHandler,
    command_name: &str,
) {
    let handler_id = Arc::as_ptr(&handler.state) as usize;
    let pause = PAUSES
        .lock()
        .expect("queue exact-target pause lock")
        .iter()
        .find(|(candidate_handler, candidate, _)| {
            *candidate_handler == handler_id && candidate == command_name
        })
        .map(|(_, _, pause)| Arc::clone(pause));
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
    PAUSES
        .lock()
        .expect("queue exact-target pause lock")
        .retain(|(candidate_handler, candidate, current)| {
            *candidate_handler != handler_id
                || candidate != command_name
                || !Arc::ptr_eq(current, &pause)
        });
}
