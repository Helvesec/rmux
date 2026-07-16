use std::collections::HashMap;
use std::hash::Hash;

use rmux_core::events::SdkWaitKey;
use rmux_proto::{PaneId, SdkWaitOwnerId, DEFAULT_MAX_DETACHED_FRAME_LENGTH};

const SDK_WAIT_WEIGHT_UNIT_BYTES: usize = 1024 * 1024;
const MAX_FRAME_WEIGHT_UNITS: usize =
    DEFAULT_MAX_DETACHED_FRAME_LENGTH.div_ceil(SDK_WAIT_WEIGHT_UNIT_BYTES);
const DEFAULT_MAX_GLOBAL_WEIGHT: usize = 8 * MAX_FRAME_WEIGHT_UNITS;
const DEFAULT_MAX_CONNECTION_WEIGHT: usize = MAX_FRAME_WEIGHT_UNITS;
const DEFAULT_MAX_OWNER_WEIGHT: usize = MAX_FRAME_WEIGHT_UNITS;
const DEFAULT_MAX_PANE_WEIGHT: usize = 2 * MAX_FRAME_WEIGHT_UNITS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SdkWaitWeight(usize);

impl SdkWaitWeight {
    pub(super) fn for_pattern_len(pattern_len: usize) -> Self {
        Self(pattern_len.div_ceil(SDK_WAIT_WEIGHT_UNIT_BYTES).max(1))
    }

    const fn units(self) -> usize {
        self.0
    }

    #[cfg(test)]
    const fn from_units(units: usize) -> Self {
        Self(units)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SdkWaitQuotaLimits {
    global: usize,
    per_connection: usize,
    per_owner: usize,
    per_pane: usize,
}

impl SdkWaitQuotaLimits {
    #[cfg(test)]
    pub(super) const fn new(
        global: usize,
        per_connection: usize,
        per_owner: usize,
        per_pane: usize,
    ) -> Self {
        Self {
            global,
            per_connection,
            per_owner,
            per_pane,
        }
    }
}

impl Default for SdkWaitQuotaLimits {
    fn default() -> Self {
        Self {
            global: DEFAULT_MAX_GLOBAL_WEIGHT,
            per_connection: DEFAULT_MAX_CONNECTION_WEIGHT,
            per_owner: DEFAULT_MAX_OWNER_WEIGHT,
            per_pane: DEFAULT_MAX_PANE_WEIGHT,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SdkWaitQuotaError {
    AlreadyReserved,
    Global { requested: usize, limit: usize },
    PerConnection { requested: usize, limit: usize },
    PerOwner { requested: usize, limit: usize },
    PerPane { requested: usize, limit: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SdkWaitQuotaRecord {
    connection_id: u64,
    owner_id: SdkWaitOwnerId,
    pane_id: PaneId,
    weight: SdkWaitWeight,
}

#[derive(Debug)]
pub(super) struct SdkWaitQuota {
    limits: SdkWaitQuotaLimits,
    reservations: HashMap<SdkWaitKey, SdkWaitQuotaRecord>,
    global_weight: usize,
    by_connection: HashMap<u64, usize>,
    by_owner: HashMap<SdkWaitOwnerId, usize>,
    by_pane: HashMap<PaneId, usize>,
}

impl SdkWaitQuota {
    pub(super) fn new(limits: SdkWaitQuotaLimits) -> Self {
        Self {
            limits,
            reservations: HashMap::new(),
            global_weight: 0,
            by_connection: HashMap::new(),
            by_owner: HashMap::new(),
            by_pane: HashMap::new(),
        }
    }

    pub(super) fn reserve(
        &mut self,
        key: SdkWaitKey,
        connection_id: u64,
        pane_id: PaneId,
        weight: SdkWaitWeight,
    ) -> Result<(), SdkWaitQuotaError> {
        if self.reservations.contains_key(&key) {
            return Err(SdkWaitQuotaError::AlreadyReserved);
        }

        let owner_id = key.owner_id();
        let next_global = checked_usage(self.global_weight, weight, self.limits.global)
            .map_err(|(requested, limit)| SdkWaitQuotaError::Global { requested, limit })?;
        let next_connection = checked_usage(
            self.by_connection.get(&connection_id).copied().unwrap_or(0),
            weight,
            self.limits.per_connection,
        )
        .map_err(|(requested, limit)| SdkWaitQuotaError::PerConnection { requested, limit })?;
        let next_owner = checked_usage(
            self.by_owner.get(&owner_id).copied().unwrap_or(0),
            weight,
            self.limits.per_owner,
        )
        .map_err(|(requested, limit)| SdkWaitQuotaError::PerOwner { requested, limit })?;
        let next_pane = checked_usage(
            self.by_pane.get(&pane_id).copied().unwrap_or(0),
            weight,
            self.limits.per_pane,
        )
        .map_err(|(requested, limit)| SdkWaitQuotaError::PerPane { requested, limit })?;

        self.global_weight = next_global;
        self.by_connection.insert(connection_id, next_connection);
        self.by_owner.insert(owner_id, next_owner);
        self.by_pane.insert(pane_id, next_pane);
        self.reservations.insert(
            key,
            SdkWaitQuotaRecord {
                connection_id,
                owner_id,
                pane_id,
                weight,
            },
        );
        Ok(())
    }

    pub(super) fn release(&mut self, key: SdkWaitKey) -> bool {
        let Some(record) = self.reservations.remove(&key) else {
            return false;
        };
        let units = record.weight.units();
        self.global_weight = self
            .global_weight
            .checked_sub(units)
            .expect("SDK wait global quota accounting must not underflow");
        subtract_usage(&mut self.by_connection, &record.connection_id, units);
        subtract_usage(&mut self.by_owner, &record.owner_id, units);
        subtract_usage(&mut self.by_pane, &record.pane_id, units);
        true
    }

    #[cfg(test)]
    pub(super) fn reservation_count(&self) -> usize {
        self.reservations.len()
    }
}

impl Default for SdkWaitQuota {
    fn default() -> Self {
        Self::new(SdkWaitQuotaLimits::default())
    }
}

fn checked_usage(
    current: usize,
    added: SdkWaitWeight,
    limit: usize,
) -> Result<usize, (usize, usize)> {
    let requested = current.saturating_add(added.units());
    (requested <= limit)
        .then_some(requested)
        .ok_or((requested, limit))
}

fn subtract_usage<K>(usage: &mut HashMap<K, usize>, key: &K, units: usize)
where
    K: Eq + Hash,
{
    let remove = {
        let current = usage
            .get_mut(key)
            .expect("SDK wait scoped quota entry must exist while reserved");
        *current = current
            .checked_sub(units)
            .expect("SDK wait scoped quota accounting must not underflow");
        *current == 0
    };
    if remove {
        usage.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_proto::SdkWaitId;

    fn key(owner: u64, wait: u64) -> SdkWaitKey {
        SdkWaitKey::new(SdkWaitOwnerId::new(owner), SdkWaitId::new(wait))
    }

    #[test]
    fn pattern_weight_rounds_up_and_charges_small_patterns() {
        assert_eq!(SdkWaitWeight::for_pattern_len(0).units(), 1);
        assert_eq!(SdkWaitWeight::for_pattern_len(1).units(), 1);
        assert_eq!(
            SdkWaitWeight::for_pattern_len(SDK_WAIT_WEIGHT_UNIT_BYTES).units(),
            1
        );
        assert_eq!(
            SdkWaitWeight::for_pattern_len(SDK_WAIT_WEIGHT_UNIT_BYTES + 1).units(),
            2
        );
    }

    #[test]
    fn default_quota_preserves_one_maximum_frame_wait() {
        let mut quota = SdkWaitQuota::default();
        let weight = SdkWaitWeight::for_pattern_len(DEFAULT_MAX_DETACHED_FRAME_LENGTH);

        quota
            .reserve(key(1, 1), 10, PaneId::new(1), weight)
            .expect("one maximum-frame wait remains supported");
        assert!(matches!(
            quota.reserve(key(1, 2), 11, PaneId::new(2), weight),
            Err(SdkWaitQuotaError::PerOwner { .. })
        ));
    }

    #[test]
    fn scoped_limits_preserve_capacity_for_other_clients() {
        let limits = SdkWaitQuotaLimits::new(12, 4, 6, 8);
        let mut quota = SdkWaitQuota::new(limits);
        let four = SdkWaitWeight::from_units(4);

        quota
            .reserve(key(1, 1), 10, PaneId::new(1), four)
            .expect("first client reserves its connection allowance");
        assert!(matches!(
            quota.reserve(key(2, 1), 10, PaneId::new(2), SdkWaitWeight::from_units(1)),
            Err(SdkWaitQuotaError::PerConnection { .. })
        ));
        quota
            .reserve(key(2, 2), 20, PaneId::new(2), four)
            .expect("another connection retains independent capacity");
        quota
            .reserve(key(3, 3), 30, PaneId::new(3), four)
            .expect("global capacity can be consumed by independent clients");
        assert!(matches!(
            quota.reserve(key(4, 4), 40, PaneId::new(4), SdkWaitWeight::from_units(1)),
            Err(SdkWaitQuotaError::Global { .. })
        ));
    }

    #[test]
    fn owner_and_pane_limits_apply_across_connections() {
        let limits = SdkWaitQuotaLimits::new(32, 8, 6, 7);
        let mut quota = SdkWaitQuota::new(limits);

        quota
            .reserve(key(1, 1), 10, PaneId::new(1), SdkWaitWeight::from_units(4))
            .expect("first reservation");
        assert!(matches!(
            quota.reserve(key(1, 2), 20, PaneId::new(2), SdkWaitWeight::from_units(3)),
            Err(SdkWaitQuotaError::PerOwner { .. })
        ));
        assert!(matches!(
            quota.reserve(key(2, 1), 20, PaneId::new(1), SdkWaitWeight::from_units(4)),
            Err(SdkWaitQuotaError::PerPane { .. })
        ));
    }

    #[test]
    fn release_is_exact_and_restores_every_scope() {
        let limits = SdkWaitQuotaLimits::new(4, 4, 4, 4);
        let mut quota = SdkWaitQuota::new(limits);
        let reservation = key(1, 1);
        let weight = SdkWaitWeight::from_units(4);

        quota
            .reserve(reservation, 10, PaneId::new(1), weight)
            .expect("initial reservation");
        assert!(quota.release(reservation));
        assert!(!quota.release(reservation));
        quota
            .reserve(key(2, 1), 10, PaneId::new(1), weight)
            .expect("release restores global, connection, owner and pane capacity");
    }
}
