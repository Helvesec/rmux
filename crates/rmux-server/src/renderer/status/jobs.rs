use std::time::Duration;

use crate::status_jobs::StatusJobRuntime;
use crate::terminal::TerminalProfile;

pub(super) fn render_template_with_status_jobs<C, T>(
    template: &str,
    profile: Option<&TerminalProfile>,
    cache_ttl: Duration,
    status_jobs: Option<&StatusJobRuntime>,
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
        let output = status_jobs
            .map(|runtime| runtime.cached_output(&command, profile, cache_ttl))
            .unwrap_or_default();
        replacements.push((placeholder.clone(), output));
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

#[cfg(test)]
mod tests {
    use super::{find_job_end, render_template_with_status_jobs};
    use crate::status_jobs::StatusJobRuntime;
    use std::time::{Duration, Instant};

    #[test]
    fn status_jobs_replace_stdout_and_trim_trailing_newlines_from_cache() {
        let runtime = StatusJobRuntime::new();
        let command = format!("echo job-ok-{}", std::process::id());
        let template = format!("a#({command})b");

        assert_eq!(render(&runtime, &template), "ab");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let rendered = render(&runtime, &template);
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
        runtime.shutdown_and_join();
    }

    #[test]
    fn status_jobs_scan_nested_parentheses() {
        assert_eq!(find_job_end(b"#(echo (ok))", 2), Some(11));
    }

    #[test]
    fn status_jobs_drop_unclosed_job_and_stop_expansion() {
        let runtime = StatusJobRuntime::new();
        assert_eq!(render(&runtime, "before#(echo no close"), "before");
    }

    #[test]
    fn status_jobs_render_commands_but_not_job_output() {
        let runtime = StatusJobRuntime::new();
        let command = format!("cached-job-#{{session_name}}-{}", std::process::id());
        runtime.seed_completed_output(
            &format!("cached-job-alpha-{}", std::process::id()),
            &format!("#{{session_name}}-{}", std::process::id()),
        );
        let template = format!("plain #{{session_name}} #({command})");

        assert_eq!(
            render_template_with_status_jobs(
                &template,
                None,
                Duration::from_secs(1),
                Some(&runtime),
                render_alpha,
                render_alpha,
            ),
            format!("plain alpha #{{session_name}}-{}", std::process::id())
        );
    }

    fn render(runtime: &StatusJobRuntime, template: &str) -> String {
        render_template_with_status_jobs(
            template,
            None,
            Duration::from_secs(1),
            Some(runtime),
            str::to_owned,
            str::to_owned,
        )
    }

    fn render_alpha(segment: &str) -> String {
        segment.replace("#{session_name}", "alpha")
    }
}
