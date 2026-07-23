use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rmux_os::process_tree::{ConsoleWindowBehavior, ProcessTreeChild, ProcessTreeController};
#[cfg(unix)]
use rustix::process::Signal;

use crate::terminal::TerminalProfile;

#[path = "status_jobs/output.rs"]
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
}

#[derive(Default)]
struct StatusJobRuntimeState {
    closing: bool,
    next_job_id: u64,
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
    shell: Option<OsString>,
    cwd: Option<OsString>,
    environment: Option<Arc<Vec<(OsString, OsString)>>>,
}

impl StatusJobKey {
    fn new(command: &str, profile: Option<&TerminalProfile>) -> Self {
        Self {
            command: command.to_owned(),
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
}

impl StatusJobRuntime {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(StatusJobRuntimeInner {
                state: Mutex::new(StatusJobRuntimeState::default()),
                shutdown: Mutex::new(()),
                owners: AtomicUsize::new(1),
            }),
        }
    }

    pub(crate) fn cached_output(
        &self,
        command: &str,
        profile: Option<&TerminalProfile>,
        cache_ttl: Duration,
    ) -> String {
        let now = Instant::now();
        let key = StatusJobKey::new(command, profile);
        let mut state = self.inner.lock_state();
        reap_completed_workers(&mut state);
        if state.closing {
            return state
                .cache
                .get(&key)
                .map(|entry| entry.output.clone())
                .unwrap_or_default();
        }

        ensure_status_job_cache_capacity(&mut state.cache, &key, now);
        let active_limit_reached = state.active.len() >= STATUS_JOB_ACTIVE_LIMIT;
        let entry = state.cache.entry(key.clone()).or_default();
        let cached = entry.output.clone();
        let stale = entry
            .updated_at
            .is_none_or(|updated_at| now.duration_since(updated_at) >= cache_ttl);
        if !stale || entry.in_flight || active_limit_reached {
            return cached;
        }

        entry.in_flight = true;
        let job_id = state.next_job_id;
        state.next_job_id = state.next_job_id.wrapping_add(1);
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation);
        let weak_runtime = Arc::downgrade(&self.inner);
        let worker_key = key.clone();
        let command = command.to_owned();
        let profile = profile.cloned();
        let (start_sender, start_receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("rmux-status-job".to_owned())
            .spawn(move || {
                if start_receiver.recv().is_err() {
                    return;
                }
                let output = run_status_job_with_cancellation(
                    &command,
                    profile.as_ref(),
                    &worker_cancellation,
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
    pub(crate) fn seed_completed_output(&self, command: &str, output: &str) {
        self.seed_cache(
            StatusJobKey::new(command, None),
            StatusJobCacheEntry {
                output: output.to_owned(),
                updated_at: Some(Instant::now()),
                in_flight: false,
            },
        );
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

    fn complete_job(&self, job_id: u64, key: &StatusJobKey, output: String) {
        let mut state = self.lock_state();
        let closing = state.closing;
        if let Some(entry) = state.cache.get_mut(key) {
            if !closing {
                entry.output = output;
                entry.updated_at = Some(Instant::now());
            }
            entry.in_flight = false;
        }
        if let Some(job) = state.active.get_mut(&job_id) {
            job.completed = true;
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
        .filter_map(|(job_id, job)| job.completed.then_some(*job_id))
        .collect::<Vec<_>>();
    for job_id in completed {
        if let Some(worker) = state.active.remove(&job_id).and_then(|job| job.worker) {
            let _ = worker.join();
        }
    }
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

#[cfg(test)]
fn run_status_job(command: &str, profile: Option<&TerminalProfile>) -> String {
    run_status_job_with_cancellation(command, profile, &AtomicBool::new(false))
}

#[cfg(all(test, windows))]
fn run_status_job_with_timeout(
    command: &str,
    profile: Option<&TerminalProfile>,
    timeout: Duration,
) -> String {
    run_status_job_until(command, profile, &AtomicBool::new(false), timeout)
}

fn run_status_job_with_cancellation(
    command: &str,
    profile: Option<&TerminalProfile>,
    cancellation: &AtomicBool,
) -> String {
    run_status_job_until(command, profile, cancellation, STATUS_JOB_TIMEOUT)
}

fn run_status_job_until(
    command: &str,
    profile: Option<&TerminalProfile>,
    cancellation: &AtomicBool,
    timeout: Duration,
) -> String {
    if cancellation.load(Ordering::Acquire) {
        return String::new();
    }
    let mut process = status_job_command(command, profile);
    process
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let Ok(mut child) =
        ProcessTreeChild::spawn_with_console_window(&mut process, ConsoleWindowBehavior::Suppress)
    else {
        return String::new();
    };
    let process_group = child.controller();
    let Some(stdout) = child.child_mut().stdout.take() else {
        terminate_status_job(child, &process_group);
        return String::new();
    };
    let mut stdout = StatusJobOutputCapture::new(stdout, STATUS_JOB_OUTPUT_LIMIT);

    let started = Instant::now();
    loop {
        if cancellation.load(Ordering::Acquire) {
            terminate_status_job(child, &process_group);
            wait_for_status_job_output_close(&mut stdout);
            return String::new();
        }

        match stdout.poll() {
            Ok(CaptureProgress::Pending | CaptureProgress::Eof) => {}
            Ok(CaptureProgress::LimitReached) => {
                terminate_status_job(child, &process_group);
                wait_for_status_job_output_close(&mut stdout);
                return status_job_stdout(stdout.into_output());
            }
            Err(_) => {
                terminate_status_job(child, &process_group);
                return String::new();
            }
        }

        let child_exited = match child.has_exited() {
            Ok(exited) => exited,
            Err(_) => {
                terminate_status_job(child, &process_group);
                wait_for_status_job_output_close(&mut stdout);
                return String::new();
            }
        };
        if child_exited {
            terminate_completed_status_job(child, &process_group);
            return status_job_stdout(drain_completed_status_job_output(stdout));
        }

        if started.elapsed() >= timeout {
            terminate_status_job(child, &process_group);
            wait_for_status_job_output_close(&mut stdout);
            return String::new();
        }

        thread::sleep(STATUS_JOB_POLL_INTERVAL);
    }
}

fn drain_completed_status_job_output(mut stdout: StatusJobOutputCapture) -> Vec<u8> {
    let deadline = Instant::now() + STATUS_JOB_OUTPUT_DRAIN_GRACE;
    loop {
        match stdout.poll() {
            Ok(CaptureProgress::Eof) | Err(_) => {
                return stdout.into_output();
            }
            Ok(CaptureProgress::LimitReached) => {
                wait_for_status_job_output_close_until(&mut stdout, deadline);
                return stdout.into_output();
            }
            Ok(CaptureProgress::Pending) if Instant::now() >= deadline => {
                return stdout.into_output();
            }
            Ok(CaptureProgress::Pending) => thread::sleep(STATUS_JOB_POLL_INTERVAL),
        }
    }
}

fn wait_for_status_job_output_close(stdout: &mut StatusJobOutputCapture) {
    wait_for_status_job_output_close_until(stdout, Instant::now() + STATUS_JOB_OUTPUT_DRAIN_GRACE);
}

fn wait_for_status_job_output_close_until(stdout: &mut StatusJobOutputCapture, deadline: Instant) {
    loop {
        match stdout.poll_discard() {
            Ok(CaptureProgress::Eof) | Err(_) => return,
            Ok(CaptureProgress::Pending) if Instant::now() >= deadline => return,
            Ok(CaptureProgress::Pending) => thread::sleep(STATUS_JOB_POLL_INTERVAL),
            Ok(CaptureProgress::LimitReached) => {
                unreachable!("discarding output does not enforce the capture limit")
            }
        }
    }
}

#[cfg(unix)]
fn request_status_job_stop(child: &mut ProcessTreeChild) {
    let _ = child.forward_signal(Signal::TERM.as_raw());
}

#[cfg(not(unix))]
fn request_status_job_stop(_: &mut ProcessTreeChild) {}

fn terminate_status_job(mut child: ProcessTreeChild, process_group: &ProcessTreeController) {
    request_status_job_stop(&mut child);
    thread::sleep(STATUS_JOB_TERMINATION_GRACE);
    terminate_status_job_immediately(child, process_group);
}

fn terminate_completed_status_job(child: ProcessTreeChild, process_group: &ProcessTreeController) {
    terminate_status_job_immediately(child, process_group);
}

fn terminate_status_job_immediately(
    mut child: ProcessTreeChild,
    process_group: &ProcessTreeController,
) {
    // `wait` disarms both the Unix group identity and Windows kill-on-close.
    // Terminate first while the unreaped leader still makes the PGID
    // non-reusable and the Job Object still owns every descendant.
    let tree_stopped = process_group
        .terminate_and_wait(STATUS_JOB_FORCE_TERMINATION_WAIT)
        .unwrap_or(false);
    if !tree_stopped {
        // Enumeration is unavailable on a few Unix targets and can fail under
        // restrictive host policies. Preserve the bounded cleanup contract by
        // making one final force-termination attempt before reaping the leader.
        let _ = child.terminate();
    }
    let _ = child.wait();
}

fn status_job_command(command: &str, profile: Option<&TerminalProfile>) -> Command {
    if let Some(profile) = profile {
        let mut process = status_job_profile_command(command, profile);
        process.env_clear();
        for (name, value) in profile.raw_environment() {
            process.env(name, value);
        }
        return process;
    }

    shell_command(command)
}

#[cfg(unix)]
fn status_job_profile_command(command: &str, profile: &TerminalProfile) -> Command {
    let mut process =
        crate::terminal::shell_std_command(std::path::Path::new("/bin/sh"), profile.cwd(), command);
    process.current_dir(profile.cwd());
    process
}

#[cfg(not(unix))]
fn status_job_profile_command(command: &str, profile: &TerminalProfile) -> Command {
    profile.shell_std_command(command)
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;

        let shell = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
        let mut process = Command::new(shell);
        process.arg("/D").arg("/S").arg("/C");
        process.raw_arg(rmux_os::command::cmd_c_verbatim_tail(command));
        process
    }

    #[cfg(not(windows))]
    {
        let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
        let mut process = Command::new(shell);
        process.arg("-c").arg(command);
        process
    }
}

fn status_job_stdout(stdout: Vec<u8>) -> String {
    let mut output = String::from_utf8_lossy(&stdout).into_owned();
    while output.ends_with(['\r', '\n']) {
        output.pop();
    }
    output
}

#[cfg(test)]
#[path = "status_jobs/tests.rs"]
mod tests;

#[cfg(all(test, windows))]
#[path = "status_jobs/windows_tests.rs"]
mod windows_tests;
