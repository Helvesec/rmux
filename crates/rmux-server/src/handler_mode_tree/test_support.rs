use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) enum ModeTreeIdentityPausePoint {
    Activation(u32),
    DeferredAction(u32),
    Mutation(u32),
    Store(u32),
}

#[derive(Debug, Default)]
pub(in crate::handler) struct ModeTreeIdentityPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

static MODE_TREE_IDENTITY_PAUSES: Mutex<
    Vec<(ModeTreeIdentityPausePoint, Arc<ModeTreeIdentityPause>)>,
> = Mutex::new(Vec::new());

pub(in crate::handler) fn install_mode_tree_identity_pause(
    point: ModeTreeIdentityPausePoint,
) -> Arc<ModeTreeIdentityPause> {
    let pause = Arc::new(ModeTreeIdentityPause::default());
    let mut pauses = MODE_TREE_IDENTITY_PAUSES
        .lock()
        .expect("mode-tree identity pause lock");
    pauses.retain(|(installed, _)| *installed != point);
    pauses.push((point, Arc::clone(&pause)));
    pause
}

pub(in crate::handler) async fn pause_mode_tree_identity(point: ModeTreeIdentityPausePoint) {
    let pause = {
        let mut pauses = MODE_TREE_IDENTITY_PAUSES
            .lock()
            .expect("mode-tree identity pause lock");
        let Some(index) = pauses.iter().position(|(installed, _)| *installed == point) else {
            return;
        };
        pauses.swap_remove(index).1
    };
    pause.reached.notify_one();
    pause.release.notified().await;
}
