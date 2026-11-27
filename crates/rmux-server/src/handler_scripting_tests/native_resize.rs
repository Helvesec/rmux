//! The Darwin resize probe's platform boundary for placement tests.

use super::RequestHandler;
pub(super) use crate::pane_terminals::ResizeCase;

pub(super) async fn arm_resize_recording(handler: &RequestHandler) {
    handler
        .state
        .lock()
        .await
        .arm_resize_outcome_test(std::path::Path::new("."), ResizeCase::Record);
}

pub(super) async fn recorded_resize_ids(handler: &RequestHandler) -> Vec<u32> {
    handler.state.lock().await.finish_resize_outcome_test()["calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|call| call["pane_id"].as_u64().unwrap() as u32)
        .collect()
}
