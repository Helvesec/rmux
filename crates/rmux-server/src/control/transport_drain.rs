use std::io;
use std::sync::Arc;
#[cfg(all(test, unix))]
use std::sync::{Mutex, OnceLock};

use rmux_ipc::{LocalStream, ServerWriteDrain};
#[cfg(all(test, unix))]
use tokio::sync::Notify;

use crate::handler::{ControlClientIdentity, RequestHandler};

pub(super) struct ControlTransportDrain {
    write_drain: Option<ServerWriteDrain>,
}

impl ControlTransportDrain {
    pub(super) fn capture(stream: &LocalStream) -> io::Result<Self> {
        Ok(Self {
            write_drain: Some(ServerWriteDrain::capture(stream)?),
        })
    }

    pub(super) async fn finish(
        &mut self,
        handler: &Arc<RequestHandler>,
        identity: ControlClientIdentity,
    ) -> io::Result<()> {
        let Some(write_drain) = self.write_drain.take() else {
            return Ok(());
        };
        #[cfg(all(test, unix))]
        pause_before_control_transport_drain(handler, identity).await;
        #[cfg(not(all(test, unix)))]
        let _ = (handler, identity);
        write_drain.flush_to_peer().await
    }
}

#[cfg(all(test, unix))]
struct ControlTransportDrainPause {
    handler: Arc<RequestHandler>,
    identity: ControlClientIdentity,
    reached: Notify,
    release: Notify,
}

#[cfg(all(test, unix))]
impl ControlTransportDrainPause {
    fn matches(&self, handler: &RequestHandler, identity: ControlClientIdentity) -> bool {
        std::ptr::eq(self.handler.as_ref(), handler) && self.identity == identity
    }
}

#[cfg(all(test, unix))]
pub(super) struct ControlTransportDrainPauseGuard {
    pause: Arc<ControlTransportDrainPause>,
}

#[cfg(all(test, unix))]
impl ControlTransportDrainPauseGuard {
    pub(super) async fn wait_until_reached(&self) {
        self.pause.reached.notified().await;
    }

    pub(super) fn release(&self) {
        self.pause.release.notify_one();
    }
}

#[cfg(all(test, unix))]
impl Drop for ControlTransportDrainPauseGuard {
    fn drop(&mut self) {
        let Some(pauses) = CONTROL_TRANSPORT_DRAIN_PAUSES.get() else {
            return;
        };
        let mut installed = pauses.lock().expect("control transport drain pause");
        if let Some(index) = installed
            .iter()
            .position(|pause| Arc::ptr_eq(pause, &self.pause))
        {
            installed.swap_remove(index);
        }
    }
}

#[cfg(all(test, unix))]
static CONTROL_TRANSPORT_DRAIN_PAUSES: OnceLock<Mutex<Vec<Arc<ControlTransportDrainPause>>>> =
    OnceLock::new();

#[cfg(all(test, unix))]
pub(super) fn install_control_transport_drain_pause(
    handler: &Arc<RequestHandler>,
    identity: ControlClientIdentity,
) -> ControlTransportDrainPauseGuard {
    let pause = Arc::new(ControlTransportDrainPause {
        handler: Arc::clone(handler),
        identity,
        reached: Notify::new(),
        release: Notify::new(),
    });
    let mut installed = CONTROL_TRANSPORT_DRAIN_PAUSES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .expect("control transport drain pause");
    let duplicate = installed
        .iter()
        .any(|installed| installed.matches(handler, identity));
    if !duplicate {
        installed.push(Arc::clone(&pause));
    }
    drop(installed);
    assert!(
        !duplicate,
        "only one control transport drain pause may be installed per handler and identity"
    );
    ControlTransportDrainPauseGuard { pause }
}

#[cfg(all(test, unix))]
async fn pause_before_control_transport_drain(
    handler: &RequestHandler,
    identity: ControlClientIdentity,
) {
    let pause = {
        let mut installed = CONTROL_TRANSPORT_DRAIN_PAUSES
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .expect("control transport drain pause");
        installed
            .iter()
            .position(|pause| pause.matches(handler, identity))
            .map(|index| installed.swap_remove(index))
    };
    if let Some(pause) = pause {
        pause.reached.notify_one();
        pause.release.notified().await;
    }
}
