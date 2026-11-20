//! Pause immediately before admission and observe its result, including unwinding.
use std::future::Future;
use std::sync::{Arc, Mutex};

use rmux_proto::RmuxError;
use tokio::sync::oneshot;

use super::{QueueExactTargetCapturePause, RequestHandler};

struct Probe {
    handler: usize,
    pause: Arc<QueueExactTargetCapturePause>,
    result: oneshot::Sender<Result<(), RmuxError>>,
}

static PROBES: Mutex<Vec<Probe>> = Mutex::new(Vec::new());

pub(crate) fn install(
    handler: &RequestHandler,
) -> (
    Arc<QueueExactTargetCapturePause>,
    oneshot::Receiver<Result<(), RmuxError>>,
) {
    let pause = Arc::new(QueueExactTargetCapturePause::default());
    let (result, receiver) = oneshot::channel();
    PROBES.lock().unwrap().push(Probe {
        handler: Arc::as_ptr(&handler.state) as usize,
        pause: Arc::clone(&pause),
        result,
    });
    (pause, receiver)
}

pub(super) async fn observe<T>(
    handler: &RequestHandler,
    command: &str,
    admission: impl Future<Output = Result<T, RmuxError>>,
) -> Result<T, RmuxError> {
    let probe = if command == "send-keys" {
        let id = Arc::as_ptr(&handler.state) as usize;
        let mut probes = PROBES.lock().unwrap();
        probes
            .iter()
            .position(|probe| probe.handler == id)
            .map(|index| probes.remove(index))
    } else {
        None
    };
    if let Some(probe) = &probe {
        probe.pause.reached.notify_one();
        probe.pause.release.notified().await;
    }
    let result = admission.await;
    if let Some(probe) = probe {
        let _ = probe
            .result
            .send(result.as_ref().map(|_| ()).map_err(Clone::clone));
    }
    result
}
