#[cfg(unix)]
use super::STATUS_JOB_OUTPUT_LIMIT;
use super::{
    ensure_status_job_cache_capacity, run_status_job, ActiveStatusJob, StatusJobCacheEntry,
    StatusJobKey, StatusJobRuntime, STATUS_JOB_ACTIVE_LIMIT, STATUS_JOB_CACHE_LIMIT,
};
use std::collections::HashMap;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(windows)]
#[test]
fn windows_status_job_preserves_quoted_command_arguments() {
    assert_eq!(
        run_status_job(r#"echo "RMUX STATUS JOB""#, None),
        r#""RMUX STATUS JOB""#
    );
}

#[cfg(unix)]
#[test]
fn status_job_key_canonicalizes_profile_environment_order() {
    let profile = test_profile(&[("RMUX_STATUS_KEY", "shared")]);
    let key = StatusJobKey::new("printf probe", Some(&profile));
    let environment = key.environment.as_ref().expect("profile environment key");
    let mut sorted = environment.as_ref().clone();

    sorted.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    assert_eq!(environment.as_ref(), &sorted);
}

#[test]
fn status_job_cache_evicts_old_completed_entries() {
    let now = Instant::now();
    let mut jobs = HashMap::new();
    for index in 0..STATUS_JOB_CACHE_LIMIT {
        jobs.insert(
            StatusJobKey::new(&format!("job-{index}"), None),
            StatusJobCacheEntry {
                output: String::new(),
                updated_at: Some(now + Duration::from_millis(index as u64)),
                in_flight: false,
            },
        );
    }

    ensure_status_job_cache_capacity(&mut jobs, &StatusJobKey::new("job-new", None), now);

    assert_eq!(jobs.len(), STATUS_JOB_CACHE_LIMIT - 1);
    assert!(!jobs.contains_key(&StatusJobKey::new("job-0", None)));
}

#[test]
fn status_job_cache_honors_render_ttl() {
    let runtime = StatusJobRuntime::new();
    let command = format!("ttl-job-{}", std::process::id());
    let key = StatusJobKey::new(&command, None);
    runtime.seed_cache(
        key.clone(),
        StatusJobCacheEntry {
            output: "cached".to_owned(),
            updated_at: Some(Instant::now()),
            in_flight: false,
        },
    );

    let rendered = runtime.cached_output(&command, None, Duration::from_secs(3600));

    assert_eq!(rendered, "cached");
    assert!(
        !runtime.cache_entry_in_flight(&key),
        "fresh cache entries must not spawn a replacement job"
    );
}

#[test]
fn status_job_runtime_bounds_active_workers() {
    let runtime = StatusJobRuntime::new();
    {
        let mut state = runtime.inner.lock_state();
        for job_id in 0..u64::try_from(STATUS_JOB_ACTIVE_LIMIT).expect("limit fits u64") {
            state.active.insert(
                job_id,
                ActiveStatusJob {
                    cancellation: Arc::new(AtomicBool::new(false)),
                    worker: None,
                    completed: false,
                },
            );
        }
    }
    let command = format!("bounded-job-{}", std::process::id());
    let key = StatusJobKey::new(&command, None);

    assert_eq!(runtime.cached_output(&command, None, Duration::ZERO), "");
    assert_eq!(runtime.active_job_count(), STATUS_JOB_ACTIVE_LIMIT);
    assert!(
        !runtime.cache_entry_in_flight(&key),
        "the active limit must reject rather than detach an untracked worker"
    );
}

#[cfg(unix)]
#[test]
fn status_job_drains_stdout_while_child_is_running() {
    let output = run_status_job("printf '%70000s' x", None);

    assert_eq!(output.len(), STATUS_JOB_OUTPUT_LIMIT);
}

#[cfg(unix)]
#[test]
fn status_job_timeout_kills_descendants_holding_stdout() {
    let started = Instant::now();
    let output = run_status_job("sleep 5 &", None);

    assert_eq!(output, "");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "status job should time out instead of waiting for background descendants"
    );
}

#[cfg(unix)]
#[test]
fn status_job_normal_completion_reaps_background_descendants() {
    let probe = StatusJobProcessProbe::new("normal-completion");
    let command = probe.normal_completion_command();
    let started = Instant::now();

    let output = run_status_job(&command, None);

    assert_eq!(output, "complete");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "normal status completion should not wait for a detached background job"
    );
    assert_probe_processes_dead(
        &probe.wait_for_descendant_count(1),
        "normal status completion",
    );
}

#[cfg(unix)]
#[test]
fn status_job_normal_completion_does_not_join_an_escaped_stdout_writer() {
    let probe = EscapedStatusWriterProbe::new();
    let started = Instant::now();

    let output = run_status_job(&probe.command(), None);

    assert!(
        output.contains("complete"),
        "the direct shell output must be preserved: {output:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "an escaped descendant retaining stdout must not block status completion"
    );
}

#[cfg(unix)]
#[test]
fn status_job_timeout_force_kills_term_ignoring_descendants() {
    let probe = StatusJobProcessProbe::new("term-resistant");
    let started = Instant::now();

    let output = run_status_job(&probe.command(), None);

    assert_eq!(output, "");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "status job should escalate promptly after its TERM grace"
    );
    assert_probe_processes_dead(&probe.wait_for_descendant_count(1), "timeout cleanup");
}

#[cfg(unix)]
#[test]
fn status_job_cache_releases_only_after_process_tree_cleanup() {
    let runtime = StatusJobRuntime::new();
    let probe = StatusJobProcessProbe::new("cache-cleanup");
    let command = probe.command();
    let key = StatusJobKey::new(&command, None);

    let _ = runtime.cached_output(&command, None, Duration::ZERO);
    let first_generation = probe.wait_for_descendant_count(1);
    wait_for_status_job_in_flight(&runtime, &key, false);
    assert_probe_processes_dead(&first_generation, "first cache generation");

    let _ = runtime.cached_output(&command, None, Duration::ZERO);
    let second_generation = probe.wait_for_descendant_count(2);
    let live = second_generation
        .iter()
        .filter(|pid| rmux_os::process::is_live(**pid))
        .count();
    assert!(
        live <= 1,
        "status refresh accumulated {live} live TERM-resistant descendants: \
         {second_generation:?}"
    );

    wait_for_status_job_in_flight(&runtime, &key, false);
    assert_probe_processes_dead(&second_generation, "second cache generation");
}

#[cfg(unix)]
#[test]
fn daemon_shutdown_cancels_joins_and_reaps_status_job_tree() {
    let runtime = StatusJobRuntime::new();
    let probe = StatusJobProcessProbe::new("daemon-shutdown");
    let command = probe.command();

    let _ = runtime.cached_output(&command, None, Duration::ZERO);
    let descendants = probe.wait_for_descendant_count(1);
    assert_eq!(runtime.active_job_count(), 1);
    let started = Instant::now();

    runtime.shutdown_and_join();

    assert!(runtime.is_closing());
    assert_eq!(
        runtime.active_job_count(),
        0,
        "shutdown must join every worker"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "daemon-owned status shutdown exceeded its bounded TERM/KILL cleanup"
    );
    assert_probe_processes_dead(&descendants, "daemon shutdown");

    let _ = runtime.cached_output(&command, None, Duration::ZERO);
    std::thread::sleep(Duration::from_millis(25));
    assert_eq!(
        runtime.active_job_count(),
        0,
        "a closing daemon must reject new status workers"
    );
}

#[cfg(unix)]
#[test]
fn dropping_status_runtime_cancels_and_reaps_owned_tree() {
    let probe = StatusJobProcessProbe::new("runtime-drop");
    let command = probe.command();
    let runtime = StatusJobRuntime::new();
    let _ = runtime.cached_output(&command, None, Duration::ZERO);
    let descendants = probe.wait_for_descendant_count(1);

    drop(runtime);

    assert_probe_processes_dead(&descendants, "runtime drop");
}

#[cfg(unix)]
#[test]
fn status_job_uses_profile_environment() {
    let profile = test_profile(&[
        ("RMUX_STATUS_PROBE", "from-profile"),
        ("TMUX_PROGRAM", "/tmp/rmux-shim/tmux"),
    ]);

    let output = run_status_job(
        "printf '%s/%s' \"$RMUX_STATUS_PROBE\" \"$TMUX_PROGRAM\"",
        Some(&profile),
    );

    assert_eq!(output, "from-profile//tmp/rmux-shim/tmux");
}

#[cfg(unix)]
#[test]
fn status_job_cache_is_partitioned_by_profile_environment() {
    let first = test_profile(&[("TMUX_PANE", "%1")]);
    let second = test_profile(&[("TMUX_PANE", "%2")]);

    assert_ne!(
        StatusJobKey::new("printf probe", Some(&first)),
        StatusJobKey::new("printf probe", Some(&second))
    );
}

#[cfg(unix)]
fn test_profile(environment: &[(&str, &str)]) -> crate::terminal::TerminalProfile {
    use rmux_core::{EnvironmentStore, OptionStore};
    use rmux_proto::SessionName;

    let mut spawn_environment = HashMap::new();
    for (name, value) in environment {
        spawn_environment.insert((*name).to_owned(), (*value).to_owned());
    }
    let session_name = SessionName::new("alpha").expect("valid session name");
    crate::terminal::TerminalProfile::for_run_shell_with_base_environment(
        &EnvironmentStore::default(),
        &OptionStore::default(),
        Some(&session_name),
        Some(1),
        Path::new("/tmp/rmux-status-job-test.sock"),
        None,
        false,
        None,
        None,
    )
    .expect("profile")
    .with_test_environment(spawn_environment)
}

#[cfg(unix)]
struct StatusJobProcessProbe {
    root: PathBuf,
    process_groups: PathBuf,
    descendants: PathBuf,
}

#[cfg(unix)]
impl StatusJobProcessProbe {
    fn new(label: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "rmux-status-job-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create status job probe root");
        Self {
            process_groups: root.join("groups.pid"),
            descendants: root.join("descendants.pid"),
            root,
        }
    }

    fn command(&self) -> String {
        format!(
            "printf '%s\\n' \"$$\" >> {}; \
             sh -c 'trap \"\" TERM; printf \"%s\\n\" \"$$\" >> \"$1\"; \
             while :; do sleep 30; done' sh {} & wait",
            shell_quote_path(&self.process_groups),
            shell_quote_path(&self.descendants),
        )
    }

    fn normal_completion_command(&self) -> String {
        format!(
            "sh -c 'trap \"\" TERM; printf \"%s\\n\" \"$$\" >> \"$1\"; \
             while :; do sleep 30; done' sh {} </dev/null >/dev/null 2>&1 & \
             while [ ! -s {} ]; do sleep 0.01; done; printf complete",
            shell_quote_path(&self.descendants),
            shell_quote_path(&self.descendants),
        )
    }

    fn wait_for_descendant_count(&self, expected: usize) -> Vec<u32> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let descendants = read_probe_pids(&self.descendants);
            if descendants.len() >= expected {
                return descendants;
            }
            assert!(
                Instant::now() < deadline,
                "status job did not record {expected} descendant pids; got {descendants:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(unix)]
const ESCAPED_STATUS_WRITER_ENV: &str = "RMUX_TEST_ESCAPED_STATUS_WRITER_PID";

#[cfg(unix)]
const ESCAPED_STATUS_WRITER_TEST: &str = "status_jobs::tests::escaped_status_stdout_writer_helper";

#[cfg(unix)]
#[test]
fn escaped_status_stdout_writer_helper() {
    use std::io::Write as _;

    let Some(pid_path) = std::env::var_os(ESCAPED_STATUS_WRITER_ENV) else {
        return;
    };

    rustix::process::setsid().expect("escape the status process group");
    std::fs::write(pid_path, format!("{}\n", std::process::id()))
        .expect("record escaped status writer pid");
    let mut stdout = std::io::stdout().lock();
    loop {
        if stdout
            .write_all(b".")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(unix)]
struct EscapedStatusWriterProbe {
    root: PathBuf,
    pid_path: PathBuf,
}

#[cfg(unix)]
impl EscapedStatusWriterProbe {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "rmux-status-job-escaped-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create escaped status writer probe root");
        let pid_path = root.join("writer.pid");
        Self { root, pid_path }
    }

    fn command(&self) -> String {
        let executable = std::env::current_exe().expect("resolve current test executable");
        format!(
            "{}={} {} --exact {} --nocapture & \
             while [ ! -s {} ]; do sleep 0.01; done; printf complete",
            ESCAPED_STATUS_WRITER_ENV,
            shell_quote_path(&self.pid_path),
            shell_quote_path(&executable),
            ESCAPED_STATUS_WRITER_TEST,
            shell_quote_path(&self.pid_path),
        )
    }
}

#[cfg(unix)]
impl Drop for EscapedStatusWriterProbe {
    fn drop(&mut self) {
        use rustix::process::{kill_process, Pid, Signal};

        for pid in read_probe_pids(&self.pid_path) {
            if let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) {
                let _ = kill_process(pid, Signal::KILL);
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(unix)]
impl Drop for StatusJobProcessProbe {
    fn drop(&mut self) {
        use rustix::process::{kill_process, kill_process_group, Pid, Signal};

        for process_group in read_probe_pids(&self.process_groups) {
            if let Some(process_group) = i32::try_from(process_group).ok().and_then(Pid::from_raw) {
                let _ = kill_process_group(process_group, Signal::KILL);
            }
        }
        for descendant in read_probe_pids(&self.descendants) {
            if let Some(descendant) = i32::try_from(descendant).ok().and_then(Pid::from_raw) {
                let _ = kill_process(descendant, Signal::KILL);
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(unix)]
fn assert_probe_processes_dead(pids: &[u32], stage: &str) {
    assert!(
        pids.iter().all(|pid| !rmux_os::process::is_live(*pid)),
        "TERM-resistant status descendants survived {stage}: {pids:?}"
    );
}

#[cfg(unix)]
fn read_probe_pids(path: &Path) -> Vec<u32> {
    let mut pids = std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .collect::<Vec<_>>();
    pids.sort_unstable();
    pids.dedup();
    pids
}

#[cfg(unix)]
fn shell_quote_path(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\"'\"'"))
}

#[cfg(unix)]
fn wait_for_status_job_in_flight(runtime: &StatusJobRuntime, key: &StatusJobKey, expected: bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if runtime.cache_entry_in_flight(key) == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "status job in-flight state did not become {expected}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
