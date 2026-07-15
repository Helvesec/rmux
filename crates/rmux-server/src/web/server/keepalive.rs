use std::time::Duration;

use tokio::time::{Instant, Interval, MissedTickBehavior};

const INTERVAL: Duration = Duration::from_secs(2);
const TIMEOUT: Duration = Duration::from_secs(8);

pub(super) const PAYLOAD: &[u8] = b"rmux";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeepaliveAction {
    Ping,
    TimedOut,
}

pub(super) struct WebSocketKeepalive {
    tick: Interval,
    last_pong: Instant,
}

impl WebSocketKeepalive {
    pub(super) fn new() -> Self {
        let now = Instant::now();
        let mut tick = tokio::time::interval_at(now + INTERVAL, INTERVAL);
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        Self {
            tick,
            last_pong: now,
        }
    }

    pub(super) fn acknowledge_pong(&mut self, payload: &[u8]) {
        if payload == PAYLOAD {
            self.last_pong = Instant::now();
        }
    }

    pub(super) async fn next_action(&mut self) -> KeepaliveAction {
        self.tick.tick().await;
        if Instant::now().duration_since(self.last_pong) >= TIMEOUT {
            KeepaliveAction::TimedOut
        } else {
            KeepaliveAction::Ping
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn matching_pongs_extend_liveness_but_other_pongs_do_not() {
        let mut keepalive = WebSocketKeepalive::new();

        assert_eq!(keepalive.next_action().await, KeepaliveAction::Ping);
        keepalive.acknowledge_pong(b"other");
        tokio::time::advance(Duration::from_secs(6)).await;
        assert_eq!(keepalive.next_action().await, KeepaliveAction::TimedOut);

        let mut keepalive = WebSocketKeepalive::new();
        assert_eq!(keepalive.next_action().await, KeepaliveAction::Ping);
        keepalive.acknowledge_pong(PAYLOAD);
        tokio::time::advance(Duration::from_secs(6)).await;
        assert_eq!(keepalive.next_action().await, KeepaliveAction::Ping);
    }
}
