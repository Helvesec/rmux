use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::terminal::TerminalProfile;

mod scope;
pub(crate) use scope::{new_job_owner_id, StatusJobOwner, StatusJobScope};

mod output;
use output::{CaptureProgress, StatusJobOutputCapture};

#[cfg(windows)]
const STATUS_JOB_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(not(windows))]
const STATUS_JOB_TIMEOUT: Duration = Duration::from_millis(750);
const STATUS_JOB_POLL_INTERVAL: Duration = Duration::from_millis(10);
const STATUS_JOB_OUTPUT_DRAIN_GRACE: Duration = Duration::from_millis(250);
const STATUS_JOB_FORCE_TERMINATION_WAIT: Duration = Duration::from_millis(100);
#[cfg(unix)]
const STATUS_JOB_TERMINATION_GRACE: Duration = Duration::from_millis(100);
#[cfg(not(unix))]
const STATUS_JOB_TERMINATION_GRACE: Duration = Duration::ZERO;
const STATUS_JOB_CACHE_LIMIT: usize = 256;
const STATUS_JOB_OUTPUT_LIMIT: usize = 64 * 1024;
const STATUS_JOB_ACTIVE_LIMIT: usize = 32;

pub(crate) struct StatusJobRuntime {
    inner: Arc<StatusJobRuntimeInner>,
}

struct StatusJobRuntimeInner {
    state: Mutex<StatusJobRuntimeState>,
    shutdown: Mutex<()>,
    owners: AtomicUsize,
    updates: tokio::sync::watch::Sender<u64>,
}

#[derive(Default)]
struct StatusJobRuntimeState {
    closing: bool,
    next_job_id: u64,
    capacity_waiting: bool,
    cache: HashMap<StatusJobKey, StatusJobCacheEntry>,
    active: HashMap<u64, ActiveStatusJob>,
}

struct ActiveStatusJob {
    cancellation: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    completed: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct StatusJobKey {
    command: String,
    source: Option<String>,
    scope: Option<StatusJobScope>,
    shell: Option<OsString>,
    cwd: Option<OsString>,
    environment: Option<Arc<Vec<(OsString, OsString)>>>,
}

impl StatusJobKey {
    fn new(command: &str, profile: Option<&TerminalProfile>) -> Self {
        Self {
            command: command.to_owned(),
            source: None,
            scope: None,
            shell: profile.map(|profile| profile.shell().as_os_str().to_owned()),
            cwd: profile.map(|profile| profile.cwd().as_os_str().to_owned()),
            environment: profile.map(status_job_environment_key),
        }
    }
}

fn status_job_environment_key(profile: &TerminalProfile) -> Arc<Vec<(OsString, OsString)>> {
    let mut environment = profile
        .raw_environment()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect::<Vec<_>>();
    environment.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    Arc::new(environment)
}

#[derive(Default)]
struct StatusJobCacheEntry {
    output: String,
    updated_at: Option<Instant>,
    in_flight: bool,
    job_id: Option<u64>,
}

impl StatusJobRuntime {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(StatusJobRuntimeInner {
                state: Mutex::new(StatusJobRuntimeState::default()),
                shutdown: Mutex::new(()),
                owners: AtomicUsize::new(1),
                updates: tokio::sync::watch::channel(0).0,
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn cached_output(
        &self,
        command: &str,
        profile: Option<&TerminalProfile>,
        cache_ttl: Duration,
    ) -> String {
        self.cached_output_key(StatusJobKey::new(command, profile), profile, cache_ttl)
    }

    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        let mut receiver = self.inner.updates.subscribe();
        // The initial attach frame may have started a job before subscribing.
        // Recheck the retained cache once even if its only update already landed.
        if !self.inner.lock_state().cache.is_empty() {
            receiver.mark_changed();
        }
        receiver
    }

    pub(crate) fn scoped_output(
        &self,
        source: &str,
        command: &str,
        profile: Option<&TerminalProfile>,
        scope: StatusJobScope,
        cache_ttl: Duration,
    ) -> String {
        let mut key = StatusJobKey::new(command, profile);
        key.source = Some(source.to_owned());
        key.scope = Some(scope);
        self.cached_output_key(key, profile, cache_ttl)
    }

    fn cached_output_key(
        &self,
        key: StatusJobKey,
        profile: Option<&TerminalProfile>,
        cache_ttl: Duration,
    ) -> String {
        let now = Instant::now();
        let mut state = self.inner.lock_state();
        reap_completed_workers(&mut state);
        if state.closing {
            return state
                .cache
                .get(&key)
                .map(|entry| entry.output.clone())
                .unwrap_or_default();
        }

        let previous = invalidate_changed_input(&mut state, &key);
        ensure_status_job_cache_capacity(&mut state.cache, &key, now);
        let active_limit_reached =
            state.active.values().filter(|job| !job.completed).count() >= STATUS_JOB_ACTIVE_LIMIT;
        let entry = state.cache.entry(key.clone()).or_default();
        if let Some(output) = previous {
            entry.output = output;
        }
        let cached = entry.output.clone();
        let stale = entry
            .updated_at
            .is_none_or(|updated_at| now.duration_since(updated_at) >= cache_ttl);
        if !stale || entry.in_flight {
            return cached;
        }
        if active_limit_reached {
            state.capacity_waiting = true;
            return cached;
        }

        let job_id = state.next_job_id;
        let entry = state.cache.get_mut(&key).expect("admitted cache entry");
        entry.in_flight = true;
        entry.job_id = Some(job_id);
        state.next_job_id = state.next_job_id.wrapping_add(1);
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation);
        let weak_runtime = Arc::downgrade(&self.inner);
        let worker_key = key.clone();
        let command = key.command.clone();
        let profile = profile.cloned();
        let (start_sender, start_receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("rmux-status-job".to_owned())
            .spawn(move || {
                if start_receiver.recv().is_err() {
                    return;
                }
                let output = run_status_job_until(
                    &command,
                    profile.as_ref(),
                    &worker_cancellation,
                    STATUS_JOB_TIMEOUT,
                    &mut |output| {
                        if let Some(runtime) = weak_runtime.upgrade() {
                            runtime.publish_job(job_id, &worker_key, output);
                        }
                    },
                );
                if let Some(runtime) = weak_runtime.upgrade() {
                    runtime.complete_job(job_id, &worker_key, output);
                }
            });

        match worker {
            Ok(worker) => {
                state.active.insert(
                    job_id,
                    ActiveStatusJob {
                        cancellation,
                        worker: Some(worker),
                        completed: false,
                    },
                );
                if start_sender.send(()).is_err() {
                    let failed = state.active.remove(&job_id);
                    if let Some(entry) = state.cache.get_mut(&key) {
                        entry.in_flight = false;
                    }
                    drop(state);
                    if let Some(worker) = failed.and_then(|job| job.worker) {
                        let _ = worker.join();
                    }
                }
            }
            Err(_) => {
                if let Some(entry) = state.cache.get_mut(&key) {
                    entry.in_flight = false;
                }
            }
        }
        cached
    }

    pub(crate) fn refresh_owner(&self, owner: &StatusJobOwner) {
        let mut state = self.inner.lock_state();
        let mut cancelled = Vec::new();
        for (key, entry) in &mut state.cache {
            if key
                .scope
                .as_ref()
                .is_some_and(|scope| &scope.owner == owner)
            {
                entry.updated_at = None;
                entry.in_flight = false;
                cancelled.extend(entry.job_id.take());
            }
        }
        for id in cancelled {
            if let Some(job) = state.active.get(&id) {
                job.cancellation.store(true, Ordering::Release);
            }
        }
    }

    pub(crate) fn cancel_owner(&self, owner: &StatusJobOwner) {
        let mut state = self.inner.lock_state();
        let keys = state
            .cache
            .keys()
            .filter(|key| {
                key.scope
                    .as_ref()
                    .is_some_and(|scope| &scope.owner == owner)
            })
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            if let Some(entry) = state.cache.remove(&key) {
                if let Some(job) = entry.job_id.and_then(|id| state.active.get(&id)) {
                    job.cancellation.store(true, Ordering::Release);
                }
            }
        }
    }

    pub(crate) fn shutdown_and_join(&self) {
        let _shutdown = self
            .inner
            .shutdown
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let workers = self.inner.begin_shutdown();
        join_status_job_workers(workers);
        self.inner.finish_shutdown();
    }

    #[cfg(all(test, unix))]
    fn is_closing(&self) -> bool {
        self.inner.lock_state().closing
    }

    #[cfg(test)]
    fn active_job_count(&self) -> usize {
        self.inner
            .lock_state()
            .active
            .values()
            .filter(|job| !job.completed)
            .count()
    }

    #[cfg(test)]
    fn seed_cache(&self, key: StatusJobKey, entry: StatusJobCacheEntry) {
        self.inner.lock_state().cache.insert(key, entry);
    }

    #[cfg(test)]
    fn cache_entry_in_flight(&self, key: &StatusJobKey) -> bool {
        self.inner
            .lock_state()
            .cache
            .get(key)
            .is_some_and(|entry| entry.in_flight)
    }
}

impl Default for StatusJobRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for StatusJobRuntime {
    fn clone(&self) -> Self {
        self.inner.owners.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Drop for StatusJobRuntime {
    fn drop(&mut self) {
        if self.inner.owners.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.shutdown_and_join();
        }
    }
}

impl fmt::Debug for StatusJobRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.inner.lock_state();
        formatter
            .debug_struct("StatusJobRuntime")
            .field("closing", &state.closing)
            .field("cached_jobs", &state.cache.len())
            .field("active_jobs", &state.active.len())
            .finish()
    }
}

impl StatusJobRuntimeInner {
    fn lock_state(&self) -> MutexGuard<'_, StatusJobRuntimeState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn publish_job(&self, job_id: u64, key: &StatusJobKey, output: String) {
        let mut state = self.lock_state();
        if state.closing {
            return;
        }
        if let Some(entry) = state
            .cache
            .get_mut(key)
            .filter(|entry| entry.job_id == Some(job_id))
        {
            if entry.output != output {
                entry.output = output;
                self.updates
                    .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
            }
        }
    }

    fn complete_job(&self, job_id: u64, key: &StatusJobKey, output: String) {
        let mut state = self.lock_state();
        let closing = state.closing;
        let mut current = false;
        if let Some(entry) = state
            .cache
            .get_mut(key)
            .filter(|entry| entry.job_id == Some(job_id))
        {
            current = true;
            if !closing {
                entry.output = output;
                entry.updated_at = Some(Instant::now());
            }
            entry.in_flight = false;
        }
        if let Some(job) = state.active.get_mut(&job_id) {
            job.completed = true;
        }
        // A stale generation must not recursively redraw consumers merely
        // because its cancellation finished. It may still provide the one
        // wakeup needed to retry work previously refused at the active limit.
        let capacity_waiting = std::mem::take(&mut state.capacity_waiting);
        if !closing && (current || capacity_waiting) {
            self.updates
                .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
        }
    }

    fn begin_shutdown(&self) -> Vec<JoinHandle<()>> {
        let mut state = self.lock_state();
        state.closing = true;
        for entry in state.cache.values_mut() {
            entry.in_flight = false;
        }
        for job in state.active.values() {
            job.cancellation.store(true, Ordering::Release);
        }
        state
            .active
            .values_mut()
            .filter_map(|job| job.worker.take())
            .collect()
    }

    fn finish_shutdown(&self) {
        self.lock_state().active.clear();
    }
}

fn join_status_job_workers(workers: Vec<JoinHandle<()>>) {
    for worker in workers {
        let _ = worker.join();
    }
}

fn reap_completed_workers(state: &mut StatusJobRuntimeState) {
    let completed = state
        .active
        .iter()
        .filter_map(|(job_id, job)| {
            (job.completed && job.worker.as_ref().is_none_or(JoinHandle::is_finished))
                .then_some(*job_id)
        })
        .collect::<Vec<_>>();
    for job_id in completed {
        if let Some(worker) = state.active.remove(&job_id).and_then(|job| job.worker) {
            let _ = worker.join();
        }
    }
}

fn invalidate_changed_input(
    state: &mut StatusJobRuntimeState,
    key: &StatusJobKey,
) -> Option<String> {
    key.scope.as_ref()?;
    let obsolete = state
        .cache
        .keys()
        .find(|old| *old != key && old.scope == key.scope && old.source == key.source)
        .cloned()?;
    let entry = state.cache.remove(&obsolete)?;
    if let Some(job) = entry.job_id.and_then(|id| state.active.get(&id)) {
        job.cancellation.store(true, Ordering::Release);
    }
    // A command edit may display its previous line while refreshing. A profile
    // change must never expose output from the old environment or directory.
    (obsolete.shell == key.shell
        && obsolete.cwd == key.cwd
        && obsolete.environment == key.environment)
        .then_some(entry.output)
}

fn ensure_status_job_cache_capacity(
    jobs: &mut HashMap<StatusJobKey, StatusJobCacheEntry>,
    key: &StatusJobKey,
    now: Instant,
) {
    if jobs.len() < STATUS_JOB_CACHE_LIMIT || jobs.contains_key(key) {
        return;
    }

    let Some(oldest_key) = jobs
        .iter()
        .filter(|(_, entry)| !entry.in_flight)
        .min_by_key(|(_, entry)| entry.updated_at.unwrap_or(now))
        .map(|(key, _)| key.clone())
    else {
        return;
    };
    jobs.remove(&oldest_key);
}

mod execution;
use execution::*;

#[cfg(test)]
#[path = "status_jobs/tests.rs"]
mod tests;

#[cfg(all(test, windows))]
#[path = "status_jobs/windows_tests.rs"]
mod windows_tests;

#[cfg(test)]
#[path = "status_jobs/cache_tests.rs"]
mod cache_tests;
