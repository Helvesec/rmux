use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use rmux_os::process_tree::{ConsoleWindowBehavior, ProcessTreeChild, ProcessTreeController};
#[cfg(unix)]
use rustix::process::Signal;

use crate::terminal::TerminalProfile;

#[cfg(windows)]
const STATUS_JOB_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(not(windows))]
const STATUS_JOB_TIMEOUT: Duration = Duration::from_millis(750);
const STATUS_JOB_POLL_INTERVAL: Duration = Duration::from_millis(10);
#[cfg(unix)]
const STATUS_JOB_TERMINATION_GRACE: Duration = Duration::from_millis(100);
#[cfg(not(unix))]
const STATUS_JOB_TERMINATION_GRACE: Duration = Duration::ZERO;
const STATUS_JOB_CACHE_LIMIT: usize = 256;
const STATUS_JOB_OUTPUT_LIMIT: usize = 64 * 1024;
const STATUS_JOB_ACTIVE_LIMIT: usize = 32;

static STATUS_JOB_CACHE: OnceLock<Mutex<HashMap<StatusJobKey, StatusJobCacheEntry>>> =
    OnceLock::new();
static ACTIVE_STATUS_JOBS: AtomicUsize = AtomicUsize::new(0);

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

#[cfg(test)]
fn expand_status_jobs(template: &str) -> String {
    render_template_with_status_jobs(
        template,
        None,
        Duration::from_secs(1),
        str::to_owned,
        str::to_owned,
    )
}

pub(super) fn render_template_with_status_jobs<C, T>(
    template: &str,
    profile: Option<&TerminalProfile>,
    cache_ttl: Duration,
    mut render_command: C,
    mut render_template: T,
) -> String
where
    C: FnMut(&str) -> String,
    T: FnMut(&str) -> String,
{
    if !template.contains("#(") {
        return render_template(template);
    }

    let bytes = template.as_bytes();
    let mut prepared = String::with_capacity(template.len());
    let mut replacements = Vec::new();
    let mut index = 0;
    let mut segment_start = 0;
    let mut job_index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'#' || bytes.get(index + 1) != Some(&b'(') {
            index += 1;
            continue;
        }

        if segment_start < index {
            prepared.push_str(&template[segment_start..index]);
        }
        let command_start = index + 2;
        let Some(command_end) = find_job_end(bytes, command_start) else {
            return render_template(&prepared);
        };
        let command = render_command(&template[command_start..command_end]);
        let placeholder = status_job_placeholder(job_index);
        replacements.push((
            placeholder.clone(),
            cached_status_job_output(&command, profile, cache_ttl),
        ));
        prepared.push_str(&placeholder);
        index = command_end + 1;
        segment_start = index;
        job_index += 1;
    }
    if segment_start < template.len() {
        prepared.push_str(&template[segment_start..]);
    }
    let mut rendered = render_template(&prepared);
    for (placeholder, output) in replacements {
        rendered = rendered.replace(&placeholder, &output);
    }
    rendered
}

fn status_job_placeholder(index: usize) -> String {
    format!("\u{E000}rmux-status-job-{index}\u{E001}")
}

fn find_job_end(bytes: &[u8], mut index: usize) -> Option<usize> {
    let mut depth = 1usize;
    while index < bytes.len() {
        match bytes[index] {
            b'(' => depth = depth.saturating_add(1),
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn cached_status_job_output(
    command: &str,
    profile: Option<&TerminalProfile>,
    cache_ttl: Duration,
) -> String {
    let now = Instant::now();
    let cache = STATUS_JOB_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut jobs = cache.lock().expect("status job cache mutex poisoned");
    let key = StatusJobKey::new(command, profile);
    ensure_status_job_cache_capacity(&mut jobs, &key, now);
    let entry = jobs.entry(key.clone()).or_default();
    let cached = entry.output.clone();
    let stale = entry
        .updated_at
        .is_none_or(|updated_at| now.duration_since(updated_at) >= cache_ttl);
    if stale && !entry.in_flight {
        let Some(slot) = StatusJobSlot::reserve(&ACTIVE_STATUS_JOBS, STATUS_JOB_ACTIVE_LIMIT)
        else {
            return cached;
        };
        entry.in_flight = true;
        let command = command.to_owned();
        let profile = profile.cloned();
        let spawn_result = thread::Builder::new()
            .name("rmux-status-job".to_owned())
            .spawn(move || {
                let _slot = slot;
                run_and_store_status_job(command, profile);
            });
        if spawn_result.is_err() {
            entry.in_flight = false;
        }
    }
    cached
}

fn run_and_store_status_job(command: String, profile: Option<TerminalProfile>) {
    let output = run_status_job(&command, profile.as_ref());
    let cache = STATUS_JOB_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut jobs = cache.lock().expect("status job cache mutex poisoned");
    let key = StatusJobKey::new(&command, profile.as_ref());
    let entry = jobs.entry(key).or_default();
    entry.output = output;
    entry.updated_at = Some(Instant::now());
    entry.in_flight = false;
}

struct StatusJobSlot<'a> {
    active: &'a AtomicUsize,
}

impl<'a> StatusJobSlot<'a> {
    fn reserve(active_jobs: &'a AtomicUsize, limit: usize) -> Option<Self> {
        active_jobs
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |active| {
                (active < limit).then_some(active + 1)
            })
            .is_ok()
            .then(|| Self {
                active: active_jobs,
            })
    }
}

impl Drop for StatusJobSlot<'_> {
    fn drop(&mut self) {
        let mut active = self.active.load(Ordering::Relaxed);
        while active > 0 {
            match self.active.compare_exchange_weak(
                active,
                active - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(current) => active = current,
            }
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

fn run_status_job(command: &str, profile: Option<&TerminalProfile>) -> String {
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
    let Some(mut stdout) = child.child_mut().stdout.take() else {
        terminate_status_job(child, &process_group);
        return String::new();
    };
    let (stdout_sender, stdout_receiver) = mpsc::channel();
    let stdout_reader = thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let remaining = STATUS_JOB_OUTPUT_LIMIT.saturating_sub(output.len());
                    if remaining == 0 {
                        break;
                    }
                    output.extend_from_slice(&buffer[..read.min(remaining)]);
                    if output.len() >= STATUS_JOB_OUTPUT_LIMIT {
                        break;
                    }
                }
            }
        }
        let _ = stdout_sender.send(output);
    });

    let started = Instant::now();
    let mut stdout = None;
    loop {
        if stdout.is_none() {
            match stdout_receiver.try_recv() {
                Ok(output) => stdout = Some(output),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => stdout = Some(Vec::new()),
            }
        }

        let child_exited = match child.has_exited() {
            Ok(exited) => exited,
            Err(_) => {
                terminate_status_job(child, &process_group);
                let _ = stdout_reader.join();
                return String::new();
            }
        };
        if child_exited {
            if let Some(stdout) = stdout.take() {
                let _ = child.wait();
                let _ = stdout_reader.join();
                return status_job_stdout(stdout);
            }
        }

        if started.elapsed() >= STATUS_JOB_TIMEOUT {
            terminate_status_job(child, &process_group);
            let _ = stdout_reader.join();
            return String::new();
        }

        thread::sleep(STATUS_JOB_POLL_INTERVAL);
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
    let _ = process_group.terminate();
    let _ = child.terminate();
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
fn test_profile(environment: &[(&str, &str)]) -> TerminalProfile {
    use rmux_core::{EnvironmentStore, OptionStore};
    use rmux_proto::SessionName;
    use std::collections::HashMap;
    use std::path::Path;

    let mut spawn_environment = HashMap::new();
    for (name, value) in environment {
        spawn_environment.insert((*name).to_owned(), (*value).to_owned());
    }
    let session_name = SessionName::new("alpha").expect("valid session name");
    TerminalProfile::for_run_shell_with_base_environment(
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

#[cfg(test)]
mod tests {
    use super::{
        ensure_status_job_cache_capacity, expand_status_jobs, render_template_with_status_jobs,
        StatusJobCacheEntry, StatusJobKey, StatusJobSlot, STATUS_JOB_CACHE, STATUS_JOB_CACHE_LIMIT,
    };
    #[cfg(unix)]
    use super::{run_status_job, test_profile, STATUS_JOB_OUTPUT_LIMIT};
    use std::collections::HashMap;
    #[cfg(unix)]
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    #[cfg(unix)]
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn status_jobs_replace_stdout_and_trim_trailing_newlines_from_cache() {
        let command = format!("echo job-ok-{}", std::process::id());
        let template = format!("a#({command})b");

        assert_eq!(expand_status_jobs(&template), "ab");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let rendered = expand_status_jobs(&template);
            if rendered.contains("job-ok-") {
                assert_eq!(rendered, format!("ajob-ok-{}b", std::process::id()));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "status job cache was not populated; last render was {rendered:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_status_job_preserves_quoted_command_arguments() {
        assert_eq!(
            super::run_status_job(r#"echo "RMUX STATUS JOB""#, None),
            r#""RMUX STATUS JOB""#
        );
    }

    #[test]
    fn status_jobs_scan_nested_parentheses() {
        assert_eq!(super::find_job_end(b"#(echo (ok))", 2), Some(11));
    }

    #[test]
    fn status_jobs_drop_unclosed_job_and_stop_expansion() {
        assert_eq!(expand_status_jobs("before#(echo no close"), "before");
    }

    #[test]
    fn status_jobs_render_commands_but_not_job_output() {
        let command = format!("cached-job-#{{session_name}}-{}", std::process::id());
        seed_cached_status_job(
            &format!("cached-job-alpha-{}", std::process::id()),
            &format!("#{{session_name}}-{}", std::process::id()),
        );
        let template = format!("plain #{{session_name}} #({command})");

        assert_eq!(
            render_template_with_status_jobs(
                &template,
                None,
                Duration::from_secs(1),
                render_alpha,
                render_alpha,
            ),
            format!("plain alpha #{{session_name}}-{}", std::process::id())
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
        let command = format!("ttl-job-{}", std::process::id());
        let template = format!("a#({command})b");
        let key = StatusJobKey::new(&command, None);
        let cache = STATUS_JOB_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
        {
            let mut jobs = cache.lock().expect("status job cache mutex poisoned");
            jobs.insert(
                key.clone(),
                StatusJobCacheEntry {
                    output: "cached".to_owned(),
                    updated_at: Some(Instant::now()),
                    in_flight: false,
                },
            );
        }

        let rendered = render_template_with_status_jobs(
            &template,
            None,
            Duration::from_secs(3600),
            str::to_owned,
            str::to_owned,
        );

        assert_eq!(rendered, "acachedb");
        let jobs = cache.lock().expect("status job cache mutex poisoned");
        assert!(
            !jobs.get(&key).expect("cache entry exists").in_flight,
            "fresh cache entries must not spawn a replacement job"
        );
    }

    #[test]
    fn status_job_slots_are_bounded_and_released() {
        let active = AtomicUsize::new(0);
        let first = StatusJobSlot::reserve(&active, 2).expect("first slot");
        let second = StatusJobSlot::reserve(&active, 2).expect("second slot");

        assert!(StatusJobSlot::reserve(&active, 2).is_none());
        assert_eq!(active.load(Ordering::Relaxed), 2);

        drop(first);
        assert_eq!(active.load(Ordering::Relaxed), 1);
        let third = StatusJobSlot::reserve(&active, 2).expect("released slot");
        assert_eq!(active.load(Ordering::Relaxed), 2);

        drop(second);
        drop(third);
        assert_eq!(active.load(Ordering::Relaxed), 0);
    }

    #[cfg(unix)]
    #[test]
    fn status_job_drains_stdout_while_child_is_running() {
        let output = super::run_status_job("printf '%70000s' x", None);

        assert_eq!(output.len(), STATUS_JOB_OUTPUT_LIMIT);
    }

    #[cfg(unix)]
    #[test]
    fn status_job_timeout_kills_descendants_holding_stdout() {
        let started = Instant::now();
        let output = super::run_status_job("sleep 5 &", None);

        assert_eq!(output, "");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "status job should time out instead of waiting for background descendants"
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
        let descendants = probe.wait_for_descendant_count(1);
        assert!(
            descendants
                .iter()
                .all(|pid| !rmux_os::process::is_live(*pid)),
            "TERM-resistant status descendants survived cleanup: {descendants:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn status_job_cache_releases_only_after_process_tree_cleanup() {
        let probe = StatusJobProcessProbe::new("cache-cleanup");
        let command = probe.command();
        let template = format!("#({command})");
        let key = StatusJobKey::new(&command, None);

        let _ = render_template_with_status_jobs(
            &template,
            None,
            Duration::ZERO,
            str::to_owned,
            str::to_owned,
        );
        let first_generation = probe.wait_for_descendant_count(1);
        wait_for_status_job_in_flight(&key, false);
        assert!(
            first_generation
                .iter()
                .all(|pid| !rmux_os::process::is_live(*pid)),
            "cache entry was released before first tree cleanup: {first_generation:?}"
        );

        let _ = render_template_with_status_jobs(
            &template,
            None,
            Duration::ZERO,
            str::to_owned,
            str::to_owned,
        );
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

        wait_for_status_job_in_flight(&key, false);
        assert!(
            second_generation
                .iter()
                .all(|pid| !rmux_os::process::is_live(*pid)),
            "status tree remained live after in-flight cleanup: {second_generation:?}"
        );
        STATUS_JOB_CACHE
            .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
            .lock()
            .expect("status job cache mutex poisoned")
            .remove(&key);
    }

    #[cfg(unix)]
    #[test]
    fn status_job_uses_profile_environment() {
        let profile = test_profile(&[
            ("RMUX_STATUS_PROBE", "from-profile"),
            ("TMUX_PROGRAM", "/tmp/rmux-shim/tmux"),
        ]);

        let output = super::run_status_job(
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
    impl Drop for StatusJobProcessProbe {
        fn drop(&mut self) {
            use rustix::process::{kill_process, kill_process_group, Pid, Signal};

            for process_group in read_probe_pids(&self.process_groups) {
                if let Some(process_group) =
                    i32::try_from(process_group).ok().and_then(Pid::from_raw)
                {
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
    fn wait_for_status_job_in_flight(key: &StatusJobKey, expected: bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let in_flight = STATUS_JOB_CACHE
                .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
                .lock()
                .expect("status job cache mutex poisoned")
                .get(key)
                .is_some_and(|entry| entry.in_flight);
            if in_flight == expected {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "status job in-flight state did not become {expected}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn render_alpha(segment: &str) -> String {
        segment.replace("#{session_name}", "alpha")
    }

    fn seed_cached_status_job(command: &str, output: &str) {
        let cache = STATUS_JOB_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
        let mut jobs = cache.lock().expect("status job cache mutex poisoned");
        jobs.insert(
            StatusJobKey::new(command, None),
            StatusJobCacheEntry {
                output: output.to_owned(),
                updated_at: Some(Instant::now()),
                in_flight: false,
            },
        );
    }
}
