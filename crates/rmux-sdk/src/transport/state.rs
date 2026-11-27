#[cfg(not(target_has_atomic = "64"))]
use portable_atomic::AtomicU64;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
#[cfg(target_has_atomic = "64")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use rmux_proto::{SdkWaitId, SdkWaitOwnerId};
use tokio::sync::Mutex as AsyncMutex;

use super::failure::TransportFailure;

static NEXT_SDK_WAIT_OWNER_ID: AtomicU64 = AtomicU64::new(1);
static SDK_WAIT_OWNER_PROCESS_SEED: OnceLock<u64> = OnceLock::new();

#[derive(Debug)]
pub(super) struct TransportState {
    terminal_failure: Mutex<Option<TransportFailure>>,
    capabilities: AsyncMutex<Option<Arc<[String]>>>,
    sdk_wait_owner_id: SdkWaitOwnerId,
    next_sdk_wait_id: AtomicU64,
}

impl Default for TransportState {
    fn default() -> Self {
        Self {
            terminal_failure: Mutex::new(None),
            capabilities: AsyncMutex::new(None),
            sdk_wait_owner_id: allocate_sdk_wait_owner_id(),
            next_sdk_wait_id: AtomicU64::new(1),
        }
    }
}

impl TransportState {
    pub(super) fn terminal_failure(&self) -> Option<TransportFailure> {
        self.lock_terminal_failure().clone()
    }

    pub(super) fn set_terminal_failure(&self, failure: TransportFailure) {
        let mut terminal_failure = self.lock_terminal_failure();
        if terminal_failure.is_none() {
            *terminal_failure = Some(failure);
        }
    }

    pub(super) async fn cached_capabilities(&self) -> Option<Arc<[String]>> {
        self.capabilities.lock().await.clone()
    }

    pub(super) async fn cache_capabilities(&self, capabilities: Vec<String>) -> Arc<[String]> {
        let capabilities = Arc::<[String]>::from(capabilities);
        let mut cached = self.capabilities.lock().await;
        if let Some(existing) = cached.as_ref() {
            return Arc::clone(existing);
        }
        *cached = Some(Arc::clone(&capabilities));
        capabilities
    }

    pub(super) fn sdk_wait_owner_id(&self) -> SdkWaitOwnerId {
        self.sdk_wait_owner_id
    }

    pub(super) fn allocate_sdk_wait_id(&self) -> SdkWaitId {
        let id = allocate_bounded_atomic_id(
            &self.next_sdk_wait_id,
            u64::MAX,
            "SDK wait id space exhausted for transport",
        );
        SdkWaitId::new(id)
    }

    fn lock_terminal_failure(&self) -> std::sync::MutexGuard<'_, Option<TransportFailure>> {
        self.terminal_failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn allocate_sdk_wait_owner_id() -> SdkWaitOwnerId {
    let local_id = allocate_bounded_atomic_id(
        &NEXT_SDK_WAIT_OWNER_ID,
        u64::MAX - 1,
        "SDK wait owner id space exhausted for process",
    );
    SdkWaitOwnerId::new(mix_sdk_wait_owner_id(
        sdk_wait_owner_process_seed(),
        local_id,
    ))
}

fn sdk_wait_owner_process_seed() -> u64 {
    *SDK_WAIT_OWNER_PROCESS_SEED.get_or_init(|| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(now as u64);
        hasher.write_u64((now >> 64) as u64);
        hasher.write_u32(std::process::id());
        splitmix64(hasher.finish())
    })
}

pub(super) fn mix_sdk_wait_owner_id(process_seed: u64, local_id: u64) -> u64 {
    let mixed = splitmix64(process_seed ^ local_id.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    if mixed == 0 {
        1
    } else {
        mixed
    }
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

pub(super) fn allocate_bounded_atomic_id(
    counter: &AtomicU64,
    max_inclusive: u64,
    exhausted_message: &'static str,
) -> u64 {
    loop {
        let current = counter.load(Ordering::Relaxed);
        assert!(current <= max_inclusive, "{exhausted_message}");
        let next = current.checked_add(1).expect(exhausted_message);
        if counter
            .compare_exchange(current, next, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return current;
        }
    }
}
