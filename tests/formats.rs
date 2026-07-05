#![cfg(unix)]

mod common;

use std::time::Duration;

use common::{
    FrozenTmuxBinary, TmuxCompatHarness, TmuxCompatRun, TmuxCompatRunConfig, FROZEN_TMUX_ENV,
};
use rmux_core::formats::{
    is_known_format_variable_name, is_truthy, render_template, FormatContext, FormatVariable,
    FormatVariables, DEFAULT_DISPLAY_MESSAGE_FORMAT, DEFAULT_LIST_PANES_FORMAT,
    DEFAULT_LIST_SESSIONS_FORMAT, DEFAULT_LIST_WINDOWS_FORMAT, FORMAT_VARIABLES,
};
use rmux_core::{OptionStore, Session};
use rmux_proto::{OptionName, TerminalSize};

fn session_name(value: &str) -> rmux_proto::SessionName {
    rmux_proto::SessionName::new(value).expect("valid session name")
}

fn assert_closed_format(value: &str) {
    let bytes = value.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'#' if bytes.get(index + 1) == Some(&b'#') => {
                index += 2;
            }
            b'#' if bytes.get(index + 1) == Some(&b'[') => {
                let end = value[index + 2..]
                    .find(']')
                    .map(|offset| index + 2 + offset)
                    .expect("style sequence must close");
                index = end + 1;
            }
            b'#' if matches!(
                bytes.get(index + 1).copied(),
                Some(b'D' | b'F' | b'H' | b'I' | b'P' | b'S' | b'T' | b'W' | b'h')
            ) =>
            {
                index += 2;
            }
            b'#' if bytes.get(index + 1) == Some(&b'{') => {
                let end =
                    find_expression_end(value, index + 2).expect("format expression must close");
                assert_closed_expression(&value[index + 2..end], value);
                index = end + 1;
            }
            _ => {
                let character = value[index..]
                    .chars()
                    .next()
                    .expect("remaining format slice must be non-empty");
                index += character.len_utf8();
            }
        }
    }
}

fn find_expression_end(value: &str, start: usize) -> Option<usize> {
    let bytes = value.as_bytes();
    let mut depth = 0;
    let mut index = start;

    while index < bytes.len() {
        match bytes[index] {
            b'#' if bytes.get(index + 1) == Some(&b'{') => {
                depth += 1;
                index += 2;
            }
            b'}' => {
                if depth == 0 {
                    return Some(index);
                }
                depth -= 1;
                index += 1;
            }
            b'#' if matches!(
                bytes.get(index + 1).copied(),
                Some(b'#' | b',' | b':' | b'}' | b'{')
            ) =>
            {
                index += 2;
            }
            _ => {
                index += 1;
            }
        }
    }

    None
}

fn split_top_level(value: &str, delimiter: u8) -> Vec<&str> {
    let bytes = value.as_bytes();
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'#' if bytes.get(index + 1) == Some(&b'{') => {
                depth += 1;
                index += 2;
            }
            b'}' if depth > 0 => {
                depth -= 1;
                index += 1;
            }
            b'#' if matches!(
                bytes.get(index + 1).copied(),
                Some(b'#' | b',' | b':' | b'}' | b'{')
            ) =>
            {
                index += 2;
            }
            current if current == delimiter && depth == 0 => {
                parts.push(&value[start..index]);
                index += 1;
                start = index;
            }
            _ => {
                index += 1;
            }
        }
    }

    parts.push(&value[start..]);
    parts
}

fn assert_closed_expression(expression: &str, template: &str) {
    if let Some(conditional) = expression.strip_prefix('?') {
        let parts = split_top_level(conditional, b',');
        assert!(!parts.is_empty(), "empty conditional in {template:?}");
        assert_closed_expression(parts[0], template);
        for branch in parts.into_iter().skip(1) {
            assert_closed_format(branch);
        }
        return;
    }

    for prefix in [
        "==:", "!=:", "<=:", ">=:", "<:", ">:", "||:", "&&:", "!:", "!!:", "m:",
    ] {
        if let Some(rest) = expression.strip_prefix(prefix) {
            for operand in split_top_level(rest, b',') {
                assert_closed_format(operand);
            }
            return;
        }
    }

    if expression.contains("#{") {
        assert_closed_format(expression);
        return;
    }

    let variable = expression.rsplit(':').next().unwrap_or(expression).trim();
    if variable.is_empty()
        || variable.starts_with('@')
        || variable.chars().all(|character| {
            character.is_ascii_digit() || matches!(character, '-' | '+' | '.' | '/')
        })
    {
        return;
    }

    assert!(
        is_known_format_variable_name(variable),
        "unsupported format variable {variable:?} in {template:?}"
    );
}

struct StaticWindowValues;

fn sample_window_layout() -> String {
    let body = "120x40,0,0{80x40,0,0,4,39x40,81,0[39x20,81,0,5,39x19,81,21,6]}";
    let mut checksum = 0_u16;
    for byte in body.bytes() {
        checksum = (checksum >> 1) + ((checksum & 1) << 15);
        checksum = checksum.wrapping_add(u16::from(byte));
    }
    format!("{checksum:04x},{body}")
}

impl FormatVariables for StaticWindowValues {
    fn format_value(&self, variable: FormatVariable) -> Option<String> {
        Some(match variable {
            FormatVariable::SessionName => "alpha".to_owned(),
            FormatVariable::SessionWindows => "7".to_owned(),
            FormatVariable::SessionAttached => "2".to_owned(),
            FormatVariable::SessionWidth => "120".to_owned(),
            FormatVariable::SessionHeight => "40".to_owned(),
            FormatVariable::WindowIndex => "5".to_owned(),
            FormatVariable::WindowId => "@9".to_owned(),
            FormatVariable::WindowName => "logs".to_owned(),
            FormatVariable::WindowRawFlags => "*".to_owned(),
            FormatVariable::WindowPanes => "3".to_owned(),
            FormatVariable::WindowWidth => "120".to_owned(),
            FormatVariable::WindowHeight => "40".to_owned(),
            FormatVariable::WindowLayout => sample_window_layout(),
            FormatVariable::WindowActive => "1".to_owned(),
            FormatVariable::WindowLastFlag => "0".to_owned(),
            FormatVariable::PaneIndex => "0".to_owned(),
            FormatVariable::PaneId => "%4".to_owned(),
            FormatVariable::PaneActive => "1".to_owned(),
            FormatVariable::PaneWidth => "80".to_owned(),
            FormatVariable::PaneHeight => "24".to_owned(),
        })
    }

    fn format_value_by_name(&self, name: &str) -> Option<String> {
        match name {
            "history_bytes" => Some("512".to_owned()),
            "history_limit" => Some("2000".to_owned()),
            "history_size" => Some("10".to_owned()),
            "pane_dead" => Some("0".to_owned()),
            "pane_title" => Some("build logs".to_owned()),
            "session_created" => Some("1713180600".to_owned()),
            "session_group" => Some("dev".to_owned()),
            "session_grouped" => Some("0".to_owned()),
            "window_bigger" => Some("0".to_owned()),
            "window_flags" => Some("*".to_owned()),
            "window_offset_x" => Some("3".to_owned()),
            "window_offset_y" => Some("4".to_owned()),
            _ => FormatVariable::from_name(name).and_then(|variable| self.format_value(variable)),
        }
    }
}

#[test]
fn default_list_windows_format_matches_existing_shape() {
    let rendered = render_template(DEFAULT_LIST_WINDOWS_FORMAT, &StaticWindowValues);

    assert_eq!(
        rendered,
        format!(
            "5: logs* (3 panes) [120x40] [layout {}] @9 (active)",
            sample_window_layout()
        )
    );
}

#[test]
fn default_list_sessions_format_matches_existing_shape() {
    let rendered = render_template(DEFAULT_LIST_SESSIONS_FORMAT, &StaticWindowValues);
    let created = render_template("#{t:session_created}", &StaticWindowValues);

    assert_eq!(
        rendered,
        format!("alpha: 7 windows (created {created}) (attached)")
    );
}

#[test]
fn default_list_panes_format_matches_existing_shape() {
    let rendered = render_template(DEFAULT_LIST_PANES_FORMAT, &StaticWindowValues);

    assert_eq!(
        rendered,
        "alpha:5.0: [80x24] [history 10/2000, 512 bytes] %4 (active)"
    );
}

#[test]
fn format_variable_inventory_is_closed_to_twenty_names() {
    let names = FORMAT_VARIABLES
        .iter()
        .map(|variable| variable.name())
        .collect::<Vec<_>>();

    assert_eq!(
        names,
        vec![
            "session_name",
            "session_windows",
            "session_attached",
            "session_width",
            "session_height",
            "window_index",
            "window_id",
            "window_name",
            "window_raw_flags",
            "window_panes",
            "window_width",
            "window_height",
            "window_layout",
            "window_active",
            "window_last_flag",
            "pane_index",
            "pane_id",
            "pane_active",
            "pane_width",
            "pane_height",
        ]
    );

    for variable in FORMAT_VARIABLES {
        assert_eq!(FormatVariable::from_name(variable.name()), Some(variable));
    }
    assert_eq!(FormatVariable::from_name("window_flags"), None);
    assert_eq!(FormatVariable::from_name("pane_title"), None);
}

#[test]
fn compatibility_engine_keeps_hash_escaping_conditionals_and_unknown_empty() {
    let rendered = render_template(
        "##{literal}:#{session_name}:#{missing}:#{?window_active,yes,no}:#{?window_last_flag,last,not-last}",
        &StaticWindowValues,
    );

    assert_eq!(rendered, "#{literal}:alpha::yes:not-last");
}

#[test]
fn tmux_compatible_malformed_and_empty_conditional_behavior() {
    // Unclosed `#{` — tmux breaks out of the expansion loop, dropping the rest.
    assert_eq!(render_template("#{window_name", &StaticWindowValues), "");
    // Conditional with empty branches still works.
    assert_eq!(
        render_template(
            "#{?window_active,,fallback}:#{?window_last_flag,last,}:#{?missing,yes,}",
            &StaticWindowValues,
        ),
        "::"
    );
    // Broken nesting: format_skip cannot find a matching `}` for the outer
    // `#{`, so tmux drops everything from the `#` onward.
    assert_eq!(
        render_template(
            "#{window_name/#{?window_active,yes}/tail",
            &StaticWindowValues
        ),
        ""
    );
}

#[test]
fn tmux_compatible_modifiers_and_aliases() {
    // Single-character aliases now resolve through `format_value_by_name`, and
    // named runtime variables remain available without enlarging the enum.
    let rendered = render_template(
        "#I/#W/#S/#T/#F/#{=21:pane_title}/#{E:session_name}/#{T:session_name}/#{window_flags}",
        &StaticWindowValues,
    );

    assert_eq!(
        rendered,
        "5/logs/alpha/build logs/*/build logs/alpha/alpha/*"
    );
}

#[test]
fn truthiness_is_shared_non_empty_and_not_exactly_zero() {
    assert!(!is_truthy(""));
    assert!(!is_truthy("0"));
    assert!(is_truthy("00"));
    assert!(is_truthy("false"));
    assert!(is_truthy("1"));
}

#[test]
fn format_context_populates_session_window_and_pane_values() {
    let mut session = Session::new(
        session_name("alpha"),
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    );
    session.split_active_pane().expect("split succeeds");
    let window = session.window();
    let pane = window.active_pane().expect("active pane exists");
    let geometry = pane.geometry();
    let context = FormatContext::from_session(&session)
        .with_session_attached(2)
        .with_window(session.active_window_index(), window, true, false)
        .with_window_pane(window, pane);

    let rendered = render_template(
        "#{session_name}:#{session_windows}:#{session_attached}:#{session_width}x#{session_height}:#{window_index}:#{window_id}:#{window_panes}:#{window_width}x#{window_height}:#{window_layout}:#{window_active}:#{window_last_flag}:#{pane_index}:#{pane_id}:#{pane_active}:#{pane_width}x#{pane_height}",
        &context,
    );

    assert_eq!(
        rendered,
        format!(
            "alpha:1:2:100x30:0:@0:2:100x30:{}:1:0:{}:%{}:1:{}x{}",
            window.layout_dump(),
            pane.index(),
            pane.id().as_u32(),
            geometry.cols(),
            geometry.rows()
        )
    );
}

#[test]
fn format_context_session_only_omits_window_and_pane_variables() {
    let session = Session::new(session_name("gamma"), TerminalSize { cols: 80, rows: 24 });
    let context = FormatContext::from_session(&session).with_session_attached(0);

    let rendered = render_template(
        "#{session_name}:#{session_attached}:#{window_index}:#{window_raw_flags}:#{pane_index}:#{?window_active,yes,no}",
        &context,
    );

    assert_eq!(rendered, "gamma:0::::no");
}

#[test]
fn format_context_inactive_non_last_window_has_empty_raw_flags() {
    let session = Session::new(session_name("delta"), TerminalSize { cols: 80, rows: 24 });
    let context = FormatContext::from_session(&session).with_window(
        session.active_window_index(),
        session.window(),
        false,
        false,
    );

    let rendered = render_template("[#{window_raw_flags}]", &context);

    assert_eq!(rendered, "[]");
}

#[test]
fn format_consuming_option_defaults_use_only_the_closed_surface() {
    let options = OptionStore::new();

    assert_closed_format(DEFAULT_DISPLAY_MESSAGE_FORMAT);
    assert_closed_format(DEFAULT_LIST_SESSIONS_FORMAT);
    assert_closed_format(DEFAULT_LIST_PANES_FORMAT);

    let session_name = session_name("alpha");
    let status_left = options
        .resolve(Some(&session_name), OptionName::StatusLeft)
        .expect("status-left default resolves");
    let status_right = options
        .resolve(Some(&session_name), OptionName::StatusRight)
        .expect("status-right default resolves");
    let window_status_current = options
        .resolve(Some(&session_name), OptionName::WindowStatusCurrentFormat)
        .expect("window-status-current-format default resolves");
    let window_status = options
        .resolve(Some(&session_name), OptionName::WindowStatusFormat)
        .expect("window-status-format default resolves");

    assert_eq!(status_left, "[#{session_name}] ");
    assert_eq!(
        status_right,
        r##"#{?window_bigger,[#{window_offset_x}#,#{window_offset_y}] ,}"#{=21:pane_title}" %H:%M %d-%b-%y"##
    );
    assert_eq!(
        window_status_current,
        "#I:#W#{?window_flags,#{window_flags}, }"
    );
    assert_eq!(window_status, "#I:#W#{?window_flags,#{window_flags}, }");

    assert_closed_format(status_left);
    assert_closed_format(status_right);
    assert_closed_format(window_status_current);
    assert_closed_format(window_status);
}

#[test]
fn tmux_format_table_names_sorted_invariant() {
    use rmux_core::formats::TMUX_FORMAT_TABLE_NAMES;
    for pair in TMUX_FORMAT_TABLE_NAMES.windows(2) {
        assert!(
            pair[0] < pair[1],
            "TMUX_FORMAT_TABLE_NAMES not sorted: {:?} >= {:?}",
            pair[0],
            pair[1]
        );
    }
    assert_eq!(TMUX_FORMAT_TABLE_NAMES.len(), 192);
}

#[test]
fn is_known_covers_all_192_table_entries() {
    use rmux_core::formats::TMUX_FORMAT_TABLE_NAMES;
    for name in TMUX_FORMAT_TABLE_NAMES {
        assert!(
            is_known_format_variable_name(name),
            "table entry {name:?} not recognized"
        );
    }
}

#[test]
fn window_name_format_matches_frozen_tmux_when_available() -> Result<(), Box<dyn std::error::Error>>
{
    let harness = TmuxCompatHarness::new("formats-window-name-tmux-compat")?;
    let tmux_binary = match FrozenTmuxBinary::discover() {
        FrozenTmuxBinary::Available(path) => path,
        FrozenTmuxBinary::Unavailable {
            checked_path,
            reason,
        } => {
            eprintln!(
                "runtime skip: frozen tmux binary unavailable via {FROZEN_TMUX_ENV} or default '{}': {reason}",
                checked_path.display()
            );
            return Ok(());
        }
    };
    let config = TmuxCompatRunConfig::default()
        .with_env("LC_CTYPE", "C.UTF-8")
        .with_env("LC_ALL", "C.UTF-8")
        .with_env("TERM_PROGRAM", "tmux");

    let create = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&create);

    let mut last = None;
    for _ in 0..100 {
        let display = harness.run_pair_with(
            &tmux_binary,
            &["display-message", "-p", "-t", "alpha", "#{window_name}"],
            config.clone(),
        )?;
        if display.tmux.stdout == display.rmux.stdout
            && display.tmux.stderr == display.rmux.stderr
            && display.tmux.status_code == display.rmux.status_code
            && !display.rmux.stdout.is_empty()
        {
            assert_exact_tmux_compat(&display);
            return Ok(());
        }
        last = Some(display);
        std::thread::sleep(Duration::from_millis(20));
    }

    let last = last.expect("window-name compatibility was attempted");
    assert_exact_tmux_compat(&last);
    assert!(
        !last.rmux.stdout.is_empty(),
        "window_name remained empty after convergence: {:?}",
        last.rmux.stdout_string()
    );
    Ok(())
}

#[test]
fn anchored_substitution_matches_frozen_tmux_when_available(
) -> Result<(), Box<dyn std::error::Error>> {
    let harness = TmuxCompatHarness::new("formats-anchored-substitution-tmux-compat")?;
    let tmux_binary = match FrozenTmuxBinary::discover() {
        FrozenTmuxBinary::Available(path) => path,
        FrozenTmuxBinary::Unavailable {
            checked_path,
            reason,
        } => {
            eprintln!(
                "runtime skip: frozen tmux binary unavailable via {FROZEN_TMUX_ENV} or default '{}': {reason}",
                checked_path.display()
            );
            return Ok(());
        }
    };
    let config = TmuxCompatRunConfig::default();

    let create = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "foofoo"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&create);

    let display = harness.run_pair_with(
        &tmux_binary,
        &[
            "display-message",
            "-p",
            "-t",
            "foofoo",
            "#{s/^foo/bar/:session_name}",
        ],
        config,
    )?;
    assert_exact_tmux_compat(&display);
    assert_eq!(display.rmux.stdout_string().trim(), "barfoo");
    Ok(())
}

#[test]
fn linked_window_format_variables_match_frozen_tmux_when_available(
) -> Result<(), Box<dyn std::error::Error>> {
    let harness = TmuxCompatHarness::new("formats-linked-window-tmux-compat")?;
    let tmux_binary = match FrozenTmuxBinary::discover() {
        FrozenTmuxBinary::Available(path) => path,
        FrozenTmuxBinary::Unavailable {
            checked_path,
            reason,
        } => {
            eprintln!(
                "runtime skip: frozen tmux binary unavailable via {FROZEN_TMUX_ENV} or default '{}': {reason}",
                checked_path.display()
            );
            return Ok(());
        }
    };
    let config = TmuxCompatRunConfig::default();

    let create_alpha = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&create_alpha);

    let create_beta = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "beta"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&create_beta);

    let link = harness.run_pair_with(
        &tmux_binary,
        &["link-window", "-s", "alpha:0", "-t", "beta:1"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&link);

    let display = harness.run_pair_with(
        &tmux_binary,
        &[
            "display-message",
            "-p",
            "-t",
            "alpha:0",
            "#{window_linked}:#{window_linked_sessions}:#{window_linked_sessions_list}",
        ],
        config,
    )?;
    assert_exact_tmux_compat(&display);
    assert_eq!(display.rmux.stdout_string().trim(), "1:2:alpha,beta");
    Ok(())
}

fn assert_exact_tmux_compat(run: &TmuxCompatRun) {
    assert_eq!(run.tmux.status_code, run.rmux.status_code);
    assert_eq!(run.tmux.timed_out, run.rmux.timed_out);
    assert_eq!(run.tmux.stdout, run.rmux.stdout);
    assert_eq!(run.tmux.stderr, run.rmux.stderr);
}
