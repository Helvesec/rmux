//! State-machine decisions for Windows managed-endpoint startup reservations.

#![cfg(windows)]

use crate::endpoint::LocalEndpoint;
use crate::windows_endpoint_record::{EndpointPhase, EndpointRecord, ProcessStamp};

/// Atomic result of reserving a managed Windows endpoint for daemon startup.
///
/// The reservation keeps the selected generation in `Starting` state until a
/// daemon listener adopts it. Dropping an unadopted owner reservation retires
/// that generation so a later launcher can rotate safely.
#[derive(Debug)]
pub struct ManagedEndpointStartReservation {
    pub(crate) endpoint: LocalEndpoint,
    pub(crate) legacy_endpoint: Option<LocalEndpoint>,
    pub(crate) owner: bool,
    pub(crate) _claim: Option<ManagedStartClaim>,
}

impl ManagedEndpointStartReservation {
    pub(crate) fn explicit(endpoint: LocalEndpoint) -> Self {
        Self {
            endpoint,
            legacy_endpoint: None,
            owner: true,
            _claim: None,
        }
    }

    pub(crate) fn join(endpoint: LocalEndpoint, legacy_endpoint: Option<LocalEndpoint>) -> Self {
        Self {
            endpoint,
            legacy_endpoint,
            owner: false,
            _claim: None,
        }
    }

    pub(crate) fn owner(
        endpoint: LocalEndpoint,
        key: String,
        nonce: String,
        process: ProcessStamp,
        integrity: &'static str,
        legacy_endpoint: Option<LocalEndpoint>,
    ) -> Self {
        Self {
            endpoint,
            legacy_endpoint,
            owner: true,
            _claim: Some(ManagedStartClaim {
                key,
                nonce,
                process,
                integrity,
            }),
        }
    }

    /// Returns the exact endpoint generation that must be probed or launched.
    #[must_use]
    pub fn endpoint(&self) -> &LocalEndpoint {
        &self.endpoint
    }

    pub(crate) fn legacy_endpoint(&self) -> Option<&LocalEndpoint> {
        self.legacy_endpoint.as_ref()
    }

    /// Returns whether this caller owns startup for the selected generation.
    #[must_use]
    pub const fn is_owner(&self) -> bool {
        self.owner
    }
}

#[derive(Debug)]
pub(crate) struct ManagedStartClaim {
    pub(crate) key: String,
    pub(crate) nonce: String,
    pub(crate) process: ProcessStamp,
    pub(crate) integrity: &'static str,
}

impl Drop for ManagedStartClaim {
    fn drop(&mut self) {
        if let Err(error) = crate::windows_endpoint_state::retire_starting_claim(self) {
            tracing::warn!("failed to retire Windows endpoint startup claim: {error}");
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReservationDecision {
    JoinCurrent,
    Rotate,
}

pub(crate) fn decide(
    record: &EndpointRecord,
    requested_nonce: &str,
    recorded_process_is_live: bool,
) -> ReservationDecision {
    if record.nonce != requested_nonce {
        return if record.phase != EndpointPhase::Stopped && recorded_process_is_live {
            ReservationDecision::JoinCurrent
        } else {
            ReservationDecision::Rotate
        };
    }

    match record.phase {
        EndpointPhase::Starting | EndpointPhase::Running if recorded_process_is_live => {
            ReservationDecision::JoinCurrent
        }
        EndpointPhase::Starting | EndpointPhase::Running | EndpointPhase::Stopped => {
            ReservationDecision::Rotate
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(pid: u32) -> ProcessStamp {
        ProcessStamp {
            pid,
            created: u64::from(pid) * 10,
        }
    }

    fn record(phase: EndpointPhase, nonce: &str, process: ProcessStamp) -> EndpointRecord {
        EndpointRecord {
            phase,
            key: "a".repeat(crate::windows_endpoint_record::KEY_HEX_LEN),
            nonce: nonce.to_owned(),
            legacy_component: None,
            process,
        }
    }

    #[test]
    fn repeated_same_process_reservation_joins_the_current_generation() {
        let requester = stamp(10);
        assert_eq!(
            decide(
                &record(EndpointPhase::Starting, "old", requester),
                "old",
                true,
            ),
            ReservationDecision::JoinCurrent
        );
    }

    #[test]
    fn dead_running_generation_rotates_between_resolve_and_claim() {
        assert_eq!(
            decide(
                &record(EndpointPhase::Running, "exposed", stamp(10)),
                "exposed",
                false,
            ),
            ReservationDecision::Rotate
        );
    }

    #[test]
    fn concurrent_restart_joins_the_generation_reserved_by_the_winner() {
        let winner = stamp(10);
        let reserved = record(EndpointPhase::Starting, "replacement", winner);

        assert_eq!(
            decide(&reserved, "stopped-old", true),
            ReservationDecision::JoinCurrent
        );
        assert_eq!(
            decide(&reserved, "replacement", true),
            ReservationDecision::JoinCurrent
        );
    }

    #[test]
    fn dead_mismatched_reservation_can_be_recovered_by_rotation() {
        assert_eq!(
            decide(
                &record(EndpointPhase::Starting, "abandoned", stamp(10)),
                "older",
                false,
            ),
            ReservationDecision::Rotate
        );
    }
}
