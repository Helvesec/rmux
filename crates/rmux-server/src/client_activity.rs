//! Client input timestamps retain clock resolution until the format boundary.
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ClientActivityTimestamp(Duration);

impl ClientActivityTimestamp {
    pub(crate) fn now() -> Self {
        Self::from_system_time(SystemTime::now())
    }

    fn from_system_time(time: SystemTime) -> Self {
        Self(time.duration_since(UNIX_EPOCH).unwrap_or_default())
    }

    pub(crate) fn unix_seconds(self) -> i64 {
        self.0.as_secs() as i64
    }

    #[cfg(test)]
    pub(crate) fn from_parts(seconds: u64, nanos: u32) -> Self {
        Self::from_system_time(UNIX_EPOCH + Duration::new(seconds, nanos))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_activity_capture_keeps_subseconds_while_formatting_seconds() {
        let first = ClientActivityTimestamp::from_parts(42, 100_000_000);
        let second = ClientActivityTimestamp::from_parts(42, 500_000_000);
        assert!(first < second);
        assert_eq!(first.unix_seconds(), 42);
        assert_eq!(second.unix_seconds(), 42);
        assert!(second < ClientActivityTimestamp::from_parts(43, 0));
    }
}
