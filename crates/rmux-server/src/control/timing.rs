use super::{ControlFormatSubscriptions, Duration, Pin};

#[cfg(any(unix, windows))]
// Keep one bounded, global post-EOF window for already accepted frames to
// publish fast replies before falling back to the detached finite drain.
pub(super) const CONTROL_EOF_GRACE: Duration = Duration::from_millis(250);
#[cfg(any(unix, windows))]
pub(super) const CONTROL_FORMAT_SUBSCRIPTION_INTERVAL: Duration = Duration::from_secs(1);

#[cfg(any(unix, windows))]
pub(super) type ControlEofTransition = Pin<Box<tokio::time::Sleep>>;
#[cfg(any(unix, windows))]
pub(super) type ControlFormatSubscriptionTick = Pin<Box<tokio::time::Sleep>>;

#[cfg(any(unix, windows))]
pub(super) fn arm_control_eof_transition(transition: &mut Option<ControlEofTransition>) {
    if transition.is_none() {
        *transition = Some(Box::pin(tokio::time::sleep(CONTROL_EOF_GRACE)));
    }
}

#[cfg(any(unix, windows))]
pub(super) async fn wait_for_control_eof_transition(transition: &mut Option<ControlEofTransition>) {
    match transition.as_mut() {
        Some(transition) => transition.as_mut().await,
        None => std::future::pending().await,
    }
}

#[cfg(any(unix, windows))]
pub(super) fn reconcile_control_format_subscription_tick(
    tick: &mut Option<ControlFormatSubscriptionTick>,
    subscriptions: &ControlFormatSubscriptions,
) {
    if subscriptions.is_empty() {
        *tick = None;
    } else if tick.is_none() {
        arm_control_format_subscription_tick(tick);
    }
}

#[cfg(any(unix, windows))]
pub(super) fn reset_control_format_subscription_tick_after_evaluation(
    tick: &mut Option<ControlFormatSubscriptionTick>,
    subscriptions: &ControlFormatSubscriptions,
) {
    *tick = None;
    reconcile_control_format_subscription_tick(tick, subscriptions);
}

#[cfg(any(unix, windows))]
pub(super) fn arm_control_format_subscription_tick(
    tick: &mut Option<ControlFormatSubscriptionTick>,
) {
    *tick = Some(Box::pin(tokio::time::sleep(
        CONTROL_FORMAT_SUBSCRIPTION_INTERVAL,
    )));
}

#[cfg(any(unix, windows))]
pub(super) async fn wait_for_control_format_subscription_tick(
    tick: &mut Option<ControlFormatSubscriptionTick>,
) {
    match tick.as_mut() {
        Some(tick) => tick.as_mut().await,
        None => std::future::pending().await,
    }
}
