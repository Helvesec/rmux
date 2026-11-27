#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::{assert_success, stderr, stdout, AttachedSession, CliHarness};
use rmux_pty::TerminalSize;
use serde_json::Value;

#[test]
fn wait_snapshot_and_locator_commands_work_end_to_end() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-wait-snapshot")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "--wait-next-text",
        "AUTOMATION_READY",
        "--timeout",
        "5s",
        "--",
        "printf AUTOMATION_READY",
        "Enter",
    ])?);

    let waited = run_json(
        &harness,
        &[
            "wait-pane",
            "-t",
            "alpha:0.0",
            "--text",
            "AUTOMATION_READY",
            "--timeout",
            "2s",
            "--json",
        ],
    )?;
    assert_eq!(waited["schema_version"], 1);
    assert_eq!(waited["ok"], true);
    assert_eq!(waited["condition"], "text");

    let snapshot = run_json(&harness, &["pane-snapshot", "-t", "alpha:0.0", "--json"])?;
    assert_eq!(snapshot["schema_version"], 1);
    assert_eq!(snapshot["ok"], true);
    assert!(
        snapshot["text"]
            .as_str()
            .expect("snapshot text")
            .contains("AUTOMATION_READY"),
        "snapshot should expose rendered visible text: {snapshot}"
    );

    let locator = run_json(
        &harness,
        &[
            "locator",
            "-t",
            "alpha:0.0",
            "--get-by-text",
            "AUTOMATION_READY",
            "--json",
        ],
    )?;
    assert_eq!(locator["schema_version"], 1);
    assert_eq!(locator["ok"], true);
    assert!(locator["count"].as_u64().unwrap_or_default() >= 1);

    assert_success(&harness.run(&[
        "expect-pane",
        "-t",
        "alpha:0.0",
        "--get-by-text",
        "AUTOMATION_READY",
        "--visible",
    ])?);
    Ok(())
}

#[test]
fn nonzero_pane_base_index_preserves_targeted_automation_and_percent_resize(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-pane-base-index")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);
    assert_success(&harness.run(&[
        "set-window-option",
        "-t",
        "alpha:0",
        "pane-base-index",
        "1",
    ])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.1"])?);

    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.1",
        "printf PANE_ONE_MARKER",
        "Enter",
    ])?);
    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.1",
        "--text",
        "PANE_ONE_MARKER",
        "--timeout",
        "5s",
    ])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.2",
        "--wait-next-text",
        "PANE_TWO_MARKER",
        "--timeout",
        "5s",
        "--",
        "printf PANE_TWO_MARKER",
        "Enter",
    ])?);

    let pane_one = run_json(&harness, &["pane-snapshot", "-t", "alpha:0.1", "--json"])?;
    let pane_two = run_json(&harness, &["pane-snapshot", "-t", "alpha:0.2", "--json"])?;
    let pane_one_text = pane_one["text"].as_str().expect("pane one snapshot text");
    let pane_two_text = pane_two["text"].as_str().expect("pane two snapshot text");
    assert!(pane_one_text.contains("PANE_ONE_MARKER"), "{pane_one}");
    assert!(!pane_one_text.contains("PANE_TWO_MARKER"), "{pane_one}");
    assert!(pane_two_text.contains("PANE_TWO_MARKER"), "{pane_two}");
    assert!(!pane_two_text.contains("PANE_ONE_MARKER"), "{pane_two}");

    assert_success(&harness.run(&["resize-pane", "-t", "alpha:0.1", "-x", "60%"])?);
    let panes = harness.run(&[
        "list-panes",
        "-t",
        "alpha:0",
        "-F",
        "#{pane_index}:#{pane_width}",
    ])?;
    assert_eq!(panes.status.code(), Some(0));
    assert!(stderr(&panes).is_empty());
    let panes = stdout(&panes);
    let pane_one_width = pane_width(&panes, 1)?;
    let pane_two_width = pane_width(&panes, 2)?;
    assert_eq!(pane_one_width, 48, "unexpected pane widths: {panes:?}");
    assert_ne!(pane_two_width, 48, "resize targeted wrong pane: {panes:?}");

    Ok(())
}

/// Regression guard for automation targets under `base-index 1` +
/// `pane-base-index 1`.
///
/// The extension commands resolve a slot target by listing the window's panes
/// and matching `#{pane_index}`, which is rendered in *display* space. Before
/// the `#{pane-base-index}` offset was applied, the internal pane index was
/// compared against the display index directly, so a non-zero
/// `pane-base-index` is what put the two spaces out of step. `base-index` only
/// renumbers windows and is resolved server-side; it is pinned here as
/// combined coverage for a non-zero window index, not as a second cause.
///
/// The old comparison did not fail uniformly either. Under the
/// `pane-base-index 1` pinned here, the first visible pane carries internal
/// index `0`, matched no listed display index and failed loudly:
/// `unable to resolve pane id for target <session>:<window>.0`. Every higher
/// pane still matched a line and was silently bound to the pane one display
/// slot below it, so the caller was served the wrong pane rather than an
/// error. All three spellings run through that same slot lookup — a `%id` and
/// a bare session target are both resolved to a `session:window.pane` slot
/// before the comparison — so none of them escaped it. `capture-pane` was
/// spared because its target stays server-side, not because it is
/// tmux-compatible: `resize-pane -x/-y <n>%` shares that surface yet still
/// ran a sibling client-side lookup, listing `#{pane_index}` to size the
/// window, so its first visible pane failed too, with
/// `resize-pane could not resolve dimensions for pane <session>:<window>.0`.
///
/// The existing `nonzero_pane_base_index_*` test pins `pane-base-index` alone;
/// this one pins both indices at once and covers all three target spellings,
/// including a bare session target which must land on the ACTIVE pane rather
/// than the first one.
/// Scenario and regression coverage contributed by Edwin Hu in #178.
#[test]
fn base_index_one_resolves_every_automation_target_spelling() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-base-index-one")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["set-option", "-g", "base-index", "1"])?);
    assert_success(&harness.run(&["set-window-option", "-g", "pane-base-index", "1"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);

    // Both indices must actually be shifted, otherwise the test would pass
    // vacuously against the zero-based layout.
    let listed = harness.run(&[
        "list-panes",
        "-a",
        "-F",
        "#{session_name}:#{window_index}.#{pane_index}",
    ])?;
    assert_ok(&listed);
    assert_eq!(stdout(&listed).trim(), "alpha:1.1");

    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:1.1"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:1.1",
        "--wait-next-text",
        "PANE_ONE_MARKER",
        "--timeout",
        "5s",
        "--",
        "printf PANE_ONE_MARKER",
        "Enter",
    ])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:1.2",
        "--wait-next-text",
        "PANE_TWO_MARKER",
        "--timeout",
        "5s",
        "--",
        "printf PANE_TWO_MARKER",
        "Enter",
    ])?);

    let pane_one_id = pane_id(&harness, "alpha:1", 1)?;
    let pane_two_id = pane_id(&harness, "alpha:1", 2)?;
    assert_ne!(
        pane_one_id, pane_two_id,
        "fixture pane ids must be distinct"
    );

    // Explicit slot targets must address the pane the user named, and a `%id`
    // must agree with the slot that reported it.
    for (target, expected, unexpected) in [
        ("alpha:1.1", "PANE_ONE_MARKER", "PANE_TWO_MARKER"),
        ("alpha:1.2", "PANE_TWO_MARKER", "PANE_ONE_MARKER"),
        (pane_one_id.as_str(), "PANE_ONE_MARKER", "PANE_TWO_MARKER"),
        (pane_two_id.as_str(), "PANE_TWO_MARKER", "PANE_ONE_MARKER"),
        // `split-window -h` leaves the new pane active, so a bare session
        // target resolves to pane 2 — never to display index 0, which does not
        // exist under `pane-base-index 1`.
        ("alpha", "PANE_TWO_MARKER", "PANE_ONE_MARKER"),
    ] {
        let snapshot = run_json(&harness, &["pane-snapshot", "-t", target, "--json"])?;
        assert_eq!(
            snapshot["ok"], true,
            "pane-snapshot -t {target}: {snapshot}"
        );
        let text = snapshot["text"]
            .as_str()
            .ok_or_else(|| format!("pane-snapshot -t {target} returned no text: {snapshot}"))?;
        assert!(
            text.contains(expected),
            "pane-snapshot -t {target}: {text:?}"
        );
        assert!(
            !text.contains(unexpected),
            "pane-snapshot -t {target} addressed the wrong pane: {text:?}"
        );

        let waited = run_json(
            &harness,
            &[
                "wait-pane",
                "-t",
                target,
                "--text",
                expected,
                "--timeout",
                "5s",
                "--json",
            ],
        )?;
        assert_eq!(waited["ok"], true, "wait-pane -t {target}: {waited}");

        let located = run_json(
            &harness,
            &["locator", "-t", target, "--get-by-text", expected, "--json"],
        )?;
        assert_eq!(located["ok"], true, "locator -t {target}: {located}");
        assert!(
            located["count"].as_u64().unwrap_or_default() >= 1,
            "locator -t {target}: {located}"
        );

        assert_ok(&harness.run(&[
            "expect-pane",
            "-t",
            target,
            "--get-by-text",
            expected,
            "--visible",
        ])?);
    }

    Ok(())
}

/// One CLI flag whose value is parsed by `parse_duration`.
struct DurationFlag {
    command: &'static str,
    flag: &'static str,
    /// An invocation whose only defect is the unitless duration value, so the
    /// parser rejection is the first — and therefore the observed — error.
    unitless: &'static [&'static str],
}

/// Every flag routed through `parse_duration`, and nothing else.
///
/// `wait-pane` and `send-keys` each own a `--stable-for` and a `--timeout`, and
/// `with-session` owns `--ttl`. Adding a sixth duration flag without adding it
/// here leaves its unit contract unpinned.
const DURATION_FLAGS: &[DurationFlag] = &[
    DurationFlag {
        command: "wait-pane",
        flag: "--stable-for",
        unitless: &[
            "wait-pane",
            "-t",
            "alpha:1.1",
            "--quiet",
            "--stable-for",
            "8000",
        ],
    },
    DurationFlag {
        command: "wait-pane",
        flag: "--timeout",
        unitless: &[
            "wait-pane",
            "-t",
            "alpha:1.1",
            "--text",
            "marker",
            "--timeout",
            "8000",
        ],
    },
    DurationFlag {
        command: "send-keys",
        flag: "--stable-for",
        unitless: &[
            "send-keys",
            "-t",
            "alpha:1.1",
            "--wait",
            "quiet",
            "--stable-for",
            "8000",
            "--",
            "printf marker",
            "Enter",
        ],
    },
    DurationFlag {
        command: "send-keys",
        flag: "--timeout",
        unitless: &[
            "send-keys",
            "-t",
            "alpha:1.1",
            "--wait-next-text",
            "marker",
            "--timeout",
            "8000",
            "--",
            "printf marker",
            "Enter",
        ],
    },
    DurationFlag {
        command: "with-session",
        flag: "--ttl",
        unitless: &["with-session", "owned", "--ttl", "8000", "--", "true"],
    },
];

/// `parse_duration` rejects bare integers on purpose, so `--help` has to say
/// which units are accepted — otherwise the requirement is only discoverable by
/// triggering the error.
///
/// Both halves of that contract are pinned for every flag in [`DURATION_FLAGS`],
/// not just for the two `--timeout`s: `--help` must name the value `<DURATION>`
/// and state the accepted units on the flag's own entry, and a unitless value
/// must be rejected with the rule *and* a usable example. Dropping
/// `value_name`, `help` or the example from any single one of the five turns
/// this test red.
#[test]
fn duration_flags_document_their_required_unit() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-duration-help")?;

    for &DurationFlag {
        command,
        flag,
        unitless,
    } in DURATION_FLAGS
    {
        let help = harness.run(&[command, "--help"])?;
        assert_ok(&help);
        let help = stdout(&help);
        let entry = help_entry(&help, flag)
            .ok_or_else(|| format!("{command} --help does not list {flag}:\n{help}"))?;
        assert!(
            entry.contains(&format!("{flag} <DURATION>")),
            "{command} {flag} should name its value <DURATION>: {entry:?}"
        );
        assert!(
            entry.contains("ms, s, or m"),
            "{command} {flag} should state the accepted units: {entry:?}"
        );

        let rejected = harness.run(unitless)?;
        assert_ne!(
            rejected.status.code(),
            Some(0),
            "{command} {flag} should reject a unitless value"
        );
        let message = stderr(&rejected);
        assert!(
            message.contains("duration requires an explicit unit: ms, s, or m"),
            "{command} {flag} should state the rule it enforced: {message}"
        );
        assert!(
            message.contains("for example 500ms, 8s, 2m"),
            "{command} {flag} should show a usable example: {message}"
        );
    }

    Ok(())
}

#[test]
fn automation_slot_lookup_preserves_list_panes_hook_family() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-slot-lookup-hook")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["set-buffer", "-b", "slot-lookup-hook", "seed"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "after-list-panes",
        "set-buffer -b slot-lookup-hook list-panes",
    ])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "after-display-message",
        "set-buffer -b slot-lookup-hook display-message",
    ])?);

    let snapshot = run_json(&harness, &["pane-snapshot", "-t", "alpha:0.0", "--json"])?;
    assert_eq!(snapshot["ok"], true);
    let hook_family = harness.run(&["show-buffer", "-b", "slot-lookup-hook"])?;
    assert_eq!(hook_family.status.code(), Some(0));
    assert_eq!(stdout(&hook_family), "list-panes");
    assert!(stderr(&hook_family).is_empty());

    Ok(())
}

#[test]
fn wait_next_text_times_out_without_history_match() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-next-text-timeout")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "printf HISTORY_ONLY",
        "Enter",
    ])?);
    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--text",
        "HISTORY_ONLY",
        "--timeout",
        "5s",
    ])?);
    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--quiet",
        "--stable-for",
        "100ms",
        "--timeout",
        "5s",
    ])?);

    let output = harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--next-text",
        "HISTORY_ONLY",
        "--timeout",
        "100ms",
        "--json",
    ])?;
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).is_empty());
    let value: Value = serde_json::from_str(&stdout(&output))?;
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"], "timeout");
    assert_eq!(value["condition"], "next-text");
    Ok(())
}

#[test]
fn wait_text_does_not_follow_reused_slot_after_kill_and_split() -> Result<(), Box<dyn Error>> {
    let mut harness = CliHarness::new("automation-stable-pane-id")?;
    let _daemon = harness.start_hidden_daemon()?;

    // The panes live 5 s and the wait 1 s: hold the CLI lock until the slot is reused.
    let window = harness.command_window()?;
    assert_success(&window.run(&["new-session", "-d", "-s", "alpha", "sleep 5"])?);
    assert_success(&window.run(&["split-window", "-h", "-t", "alpha:0.0", "sleep 5"])?);

    let mut wait_command = window.base_command();
    let wait_child = wait_command
        .args([
            "wait-pane",
            "-t",
            "alpha:0.1",
            "--text",
            "SLOT_REUSED_OUTPUT",
            "--timeout",
            "1s",
            "--json",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    std::thread::sleep(Duration::from_millis(150));
    assert_success(&window.run(&["kill-pane", "-t", "alpha:0.1"])?);
    assert_success(&window.run(&[
        "split-window",
        "-h",
        "-t",
        "alpha:0.0",
        "printf SLOT_REUSED_OUTPUT; sleep 1",
    ])?);
    drop(window);

    let output = wait_child.wait_with_output()?;
    assert_eq!(
        output.status.code(),
        Some(1),
        "wait-pane must stay bound to the original pane id\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        !stdout(&output).contains("\"ok\":true"),
        "wait-pane matched output from a reused slot: {}",
        stdout(&output)
    );
    Ok(())
}

#[test]
fn discovery_commands_and_list_commands_extension_filtering_work() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-discovery")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    let sessions = run_json(
        &harness,
        &["find-sessions", "--name-prefix", "al", "--json"],
    )?;
    assert_eq!(sessions["schema_version"], 1);
    assert_eq!(sessions["ok"], true);
    assert_eq!(sessions["sessions"][0]["session_name"], "alpha");

    let panes = run_json(&harness, &["find-panes", "--json"])?;
    assert_eq!(panes["schema_version"], 1);
    assert_eq!(panes["ok"], true);
    assert!(!panes["panes"].as_array().expect("panes").is_empty());
    for pane in panes["panes"].as_array().expect("panes") {
        assert_eq!(
            pane["session_name"], "alpha",
            "find-panes must not leak record-separator newlines into session names: {panes}"
        );
    }

    assert_success(&harness.run(&["select-pane", "-t", "alpha:0.0", "-T", "TAB\tTITLE"])?);
    let panes_with_tab_title =
        run_json(&harness, &["find-panes", "--title-prefix", "TAB", "--json"])?;
    assert_eq!(panes_with_tab_title["ok"], true);
    assert_eq!(
        panes_with_tab_title["panes"][0]["title"], "TAB\tTITLE",
        "find-panes must not drop rows when fields contain tabs"
    );

    for args in [
        &[
            "locator",
            "-t",
            "alpha:0.0",
            "--get-by-text",
            "NO_SUCH_TEXT",
        ][..],
        &["find-panes", "--title", "NO_SUCH_TITLE"][..],
        &["find-sessions", "--name", "NO_SUCH_SESSION"][..],
    ] {
        let output = harness.run(args)?;
        assert_eq!(output.status.code(), Some(0), "args: {args:?}");
        assert_eq!(stdout(&output), "", "args: {args:?}");
        assert_eq!(stderr(&output), "", "args: {args:?}");
    }

    let explicit = harness.run(&["list-commands", "wait-pane"])?;
    assert_eq!(explicit.status.code(), Some(0));
    assert!(stdout(&explicit).starts_with("wait-pane "));

    let abbreviated_extension = harness.run(&["list-commands", "wait-p"])?;
    assert_eq!(abbreviated_extension.status.code(), Some(1));
    assert!(
        stderr(&abbreviated_extension).contains("unknown command: wait-p"),
        "RMUX-only extensions must not gain prefix aliases; stderr:\n{}",
        stderr(&abbreviated_extension)
    );

    let bare = harness.run(&["list-commands", "-F", "#{command_list_name}"])?;
    assert_eq!(bare.status.code(), Some(0));
    assert!(
        !stdout(&bare).lines().any(|line| line == "wait-pane"),
        "bare list-commands must hide RMUX-only automation commands"
    );
    Ok(())
}

#[test]
fn broadcast_keys_targets_multiple_panes() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-broadcast")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "100", "-y", "30"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&[
        "broadcast-keys",
        "-t",
        "alpha:0.0",
        "-t",
        "alpha:0.1",
        "--",
        "printf BROADCAST_OK",
        "Enter",
    ])?);

    for target in ["alpha:0.0", "alpha:0.1"] {
        assert_success(&harness.run(&[
            "wait-pane",
            "-t",
            target,
            "--text",
            "BROADCAST_OK",
            "--timeout",
            "5s",
        ])?);
    }
    Ok(())
}

#[test]
fn send_keys_wait_preserves_synchronize_panes_semantics() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-send-keys-sync")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "100", "-y", "30"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&[
        "set-window-option",
        "-t",
        "alpha:0",
        "synchronize-panes",
        "on",
    ])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "--wait-next-text",
        "SYNC_WAIT_OK",
        "--timeout",
        "5s",
        "--",
        "printf SYNC_WAIT_OK",
        "Enter",
    ])?);
    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.1",
        "--text",
        "SYNC_WAIT_OK",
        "--timeout",
        "5s",
    ])?);
    Ok(())
}

#[test]
fn send_keys_wait_preserves_target_client_current_pane() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-send-keys-target-client")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta"])?);
    let mut attach = AttachedSession::spawn(&harness, "beta", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(Duration::from_secs(2))?;
    let target_client = attach.child_mut().id().to_string();

    assert_success(&harness.run(&[
        "send-keys",
        "-c",
        &target_client,
        "--wait-next-text",
        "TARGET_CLIENT_WAIT_OK",
        "--timeout",
        "5s",
        "--",
        "printf TARGET_CLIENT_WAIT_OK",
        "Enter",
    ])?);
    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "beta:0.0",
        "--text",
        "TARGET_CLIENT_WAIT_OK",
        "--timeout",
        "5s",
    ])?);

    let alpha = run_json(&harness, &["pane-snapshot", "-t", "alpha:0.0", "--json"])?;
    assert!(
        !alpha["text"]
            .as_str()
            .expect("alpha snapshot text")
            .contains("TARGET_CLIENT_WAIT_OK"),
        "send-keys -c without -t must not pre-resolve and write to the detached fallback pane"
    );
    Ok(())
}

#[test]
fn send_keys_wait_rejects_unobservable_target_client() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-send-keys-missing-target-client")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let output = harness.run(&[
        "send-keys",
        "-c",
        "999999",
        "--wait-next-text",
        "NEVER_OBSERVED",
        "--timeout",
        "2s",
        "--",
        "printf NEVER_OBSERVED",
        "Enter",
    ])?;
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("cannot observe a pane for target client"),
        "unexpected stderr: {}",
        stderr(&output)
    );
    let rejected_payload = harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--text",
        "NEVER_OBSERVED",
        "--timeout",
        "100ms",
    ])?;
    assert_eq!(
        rejected_payload.status.code(),
        Some(1),
        "send-keys --wait must not send payload before rejecting an unobservable target client"
    );
    Ok(())
}

#[test]
fn send_keys_text_wait_follows_session_identity_across_rename() -> Result<(), Box<dyn Error>> {
    let mut harness = CliHarness::new("automation-wait-text-rename")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    // The wait times out after 5 s: hold the CLI lock until the release is sent.
    let window = harness.command_window()?;
    let wait_started_at = Instant::now();
    let mut command = window.base_command();
    let mut wait_child = command
        .args([
            "send-keys",
            "-t",
            "alpha:0.0",
            "--wait-text",
            "WAIT_TEXT_FINISHED",
            "--timeout",
            "5s",
            "--",
            "printf WAIT_TEXT_%s STARTED; sleep 1; read marker; printf WAIT_TEXT_%s FINISHED",
            "Enter",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    assert_success(&window.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--text",
        "WAIT_TEXT_STARTED",
        "--timeout",
        "3s",
    ])?);
    let started_observed_after = wait_started_at.elapsed();
    assert_success(&window.run(&["rename-session", "-t", "alpha", "beta"])?);
    std::thread::sleep(Duration::from_millis(150));
    let premature_exit = wait_child.try_wait()?;
    let mut premature_stdout = String::new();
    let mut premature_stderr = String::new();
    if premature_exit.is_some() {
        if let Some(mut stdout) = wait_child.stdout.take() {
            stdout.read_to_string(&mut premature_stdout)?;
        }
        if let Some(mut stderr) = wait_child.stderr.take() {
            stderr.read_to_string(&mut premature_stderr)?;
        }
    }
    assert!(
        premature_exit.is_none(),
        "text wait must remain armed after the target session is renamed; \
         STARTED observed after {started_observed_after:?}, checked after {:?}, \
         status: {premature_exit:?}\nstdout:\n{premature_stdout}\nstderr:\n{premature_stderr}",
        wait_started_at.elapsed(),
    );

    // Keep the match pending until the assertion, regardless of command scheduling.
    assert_success(&window.run(&["send-keys", "-t", "beta:0.0", "release", "Enter"])?);
    drop(window);
    let output = wait_child_with_timeout(wait_child, Duration::from_secs(4))?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "text wait failed after rename\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    Ok(())
}

#[test]
fn send_keys_quiet_wait_follows_session_identity_across_rename() -> Result<(), Box<dyn Error>> {
    let mut harness = CliHarness::new("automation-wait-quiet-rename")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    // The rename must land while the wait is pending: hold the CLI lock until then.
    let window = harness.command_window()?;
    let mut command = window.base_command();
    let mut wait_child = command
        .args([
            "send-keys",
            "-t",
            "alpha:0.0",
            "--wait",
            "quiet",
            "--stable-for",
            "2s",
            "--timeout",
            "5s",
            "--",
            "printf WAIT_QUIET_%s STARTED; sleep 1; printf WAIT_QUIET_%s FINISHED",
            "Enter",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    assert_success(&window.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--text",
        "WAIT_QUIET_STARTED",
        "--timeout",
        "3s",
    ])?);
    assert_success(&window.run(&["rename-session", "-t", "alpha", "beta"])?);
    std::thread::sleep(Duration::from_millis(150));
    let premature_exit = wait_child.try_wait()?;
    let mut premature_stdout = String::new();
    let mut premature_stderr = String::new();
    if premature_exit.is_some() {
        if let Some(mut stdout) = wait_child.stdout.take() {
            stdout.read_to_string(&mut premature_stdout)?;
        }
        if let Some(mut stderr) = wait_child.stderr.take() {
            stderr.read_to_string(&mut premature_stderr)?;
        }
    }
    assert!(
        premature_exit.is_none(),
        "quiet wait must not report false success when the target session is renamed; \
         status: {premature_exit:?}\nstdout:\n{premature_stdout}\nstderr:\n{premature_stderr}"
    );
    drop(window);

    let output = wait_child_with_timeout(wait_child, Duration::from_secs(4))?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "quiet wait failed after rename\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "beta:0.0",
        "--text",
        "WAIT_QUIET_FINISHED",
        "--timeout",
        "1s",
    ])?);
    Ok(())
}

#[test]
fn send_keys_waits_fail_closed_when_observed_session_is_killed() -> Result<(), Box<dyn Error>> {
    let mut harness = CliHarness::new("automation-wait-killed-session")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "keeper", "sleep", "30"])?);

    // The pane goes quiet once it has printed its marker, so a quiet wait
    // shorter than the gap before the kill succeeds while the session is still
    // alive. A stability window longer than the deadline makes the kill the
    // only event that can end either wait before it times out.
    for (session, wait_args, started) in [
        (
            "quiet-target",
            vec!["--wait", "quiet", "--stable-for", "30s"],
            "KILL_QUIET_STARTED",
        ),
        (
            "visible-target",
            vec!["--wait-visible-text", "KILL_VISIBLE_EXPECTED"],
            "KILL_VISIBLE_STARTED",
        ),
    ] {
        assert_success(&harness.run(&["new-session", "-d", "-s", session])?);
        let target = format!("{session}:0.0");
        let mut arguments = vec!["send-keys", "-t", &target];
        arguments.extend(wait_args);
        arguments.extend([
            "--timeout",
            "10s",
            "--",
            "printf KILL_QUIET_%s STARTED; printf KILL_VISIBLE_%s STARTED; sleep 30",
            "Enter",
        ]);
        // The wait times out after 10 s: hold the CLI lock until the session is killed.
        let window = harness.command_window()?;
        let mut command = window.base_command();
        let wait_child = command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        assert_success(&window.run(&[
            "wait-pane",
            "-t",
            &target,
            "--text",
            started,
            "--timeout",
            "3s",
        ])?);
        assert_success(&window.run(&["kill-session", "-t", session])?);
        drop(window);

        let output = wait_child_with_timeout(wait_child, Duration::from_secs(2))?;
        assert_ne!(
            output.status.code(),
            Some(0),
            "wait must not claim success after its stable session identity is destroyed\nstdout:\n{}\nstderr:\n{}",
            stdout(&output),
            stderr(&output)
        );
    }
    Ok(())
}

#[test]
fn quiet_waits_fail_closed_when_observed_pane_is_killed() -> Result<(), Box<dyn Error>> {
    let mut harness = CliHarness::new("automation-wait-killed-pane")?;
    let mut daemon = harness.start_hidden_daemon()?;
    // Each round ends with kill-session. Without another session the daemon
    // would exit empty, and the next round's new-session would race its
    // shutdown or autostart a different server. The keeper outlives every
    // round, so one daemon serves the whole test.
    assert_success(&harness.run(&["new-session", "-d", "-s", "keeper", "sleep", "300"])?);

    // A second pane holds each session alive, so kill-pane removes only the
    // observed pane and leaves no exit status behind. A stability window
    // longer than the deadline makes the kill the only event that can end
    // the wait before it times out.
    for (session, wait_args, started) in [
        (
            "send-quiet",
            vec!["send-keys", "--wait", "quiet", "--stable-for", "30s"],
            Some("KILL_PANE_STARTED"),
        ),
        (
            "send-exit",
            vec!["send-keys", "--wait-pane-exit"],
            Some("KILL_PANE_STARTED"),
        ),
        (
            "wait-quiet",
            vec!["wait-pane", "--quiet", "--stable-for", "30s"],
            None,
        ),
        (
            "wait-quiet-json",
            vec!["wait-pane", "--quiet", "--stable-for", "30s", "--json"],
            None,
        ),
    ] {
        assert_success(&harness.run(&["new-session", "-d", "-s", session, "sleep", "30"])?);
        assert_success(&harness.run(&["split-window", "-t", &format!("{session}:0")])?);
        let target = pane_id(&harness, &format!("{session}:0"), 1)?;
        let mut arguments = wait_args.clone();
        arguments.extend(["-t", target.as_str(), "--timeout", "10s"]);
        if started.is_some() {
            arguments.extend(["--", "printf KILL_PANE_%s STARTED; sleep 30", "Enter"]);
        }
        // The wait times out after 10 s: hold the CLI lock until the pane is killed.
        let window = harness.command_window()?;
        let mut command = window.base_command();
        let wait_child = command
            .args(&arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        match started {
            Some(marker) => assert_success(&window.run(&[
                "wait-pane",
                "-t",
                &target,
                "--text",
                marker,
                "--timeout",
                "3s",
            ])?),
            // wait-pane has no armed signal. A kill before it resolves the
            // pane id fails the lookup instead: still closed, never green.
            None => std::thread::sleep(Duration::from_millis(300)),
        }
        assert_success(&window.run(&["kill-pane", "-t", &target])?);
        drop(window);

        let output = wait_child_with_timeout(wait_child, Duration::from_secs(2))?;
        assert_eq!(
            output.status.code(),
            Some(1),
            "{arguments:?} must fail closed once its pane is killed without an exit status\nstdout:\n{}\nstderr:\n{}",
            stdout(&output),
            stderr(&output)
        );
        assert!(
            !stdout(&output).contains("\"ok\":true"),
            "{arguments:?} reported success for a killed pane: {}",
            stdout(&output)
        );
        assert_success(&harness.run(&["kill-session", "-t", session])?);
    }
    assert!(
        daemon.child_mut().try_wait()?.is_none(),
        "the hidden daemon exited during the test, so a later round ran on another server"
    );
    Ok(())
}

#[test]
fn send_keys_visible_wait_rejects_old_session_name_aba_after_rename() -> Result<(), Box<dyn Error>>
{
    let mut harness = CliHarness::new("automation-wait-rename-aba")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    // The wait times out after 5 s: hold the CLI lock until the release is sent.
    let window = harness.command_window()?;
    let mut command = window.base_command();
    let mut wait_child = command
        .args([
            "send-keys",
            "-t",
            "alpha:0.0",
            "--wait-visible-text",
            "ABA_TARGET_DONE",
            "--timeout",
            "5s",
            "--",
            "printf ABA_%s STARTED; read marker; printf ABA_%s TARGET_DONE",
            "Enter",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    assert_success(&window.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--text",
        "ABA_STARTED",
        "--timeout",
        "3s",
    ])?);
    assert_success(&window.run(&["rename-session", "-t", "alpha", "beta"])?);
    assert_success(&window.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&window.run(&[
        "send-keys",
        "-t",
        "alpha:0.0",
        "printf ABA_%s TARGET_DONE",
        "Enter",
    ])?);
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        wait_child.try_wait()?.is_none(),
        "wait followed a replacement session that reused the original name"
    );

    assert_success(&window.run(&["send-keys", "-t", "beta:0.0", "release", "Enter"])?);
    drop(window);
    let output = wait_child_with_timeout(wait_child, Duration::from_secs(3))?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "wait did not follow the original session identity through rename and name reuse\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    Ok(())
}

#[test]
fn send_keys_quiet_wait_rejects_old_session_name_aba_after_rename() -> Result<(), Box<dyn Error>> {
    let mut harness = CliHarness::new("automation-quiet-wait-rename-aba")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    // The 5 s of quiet must outlast the gap between STARTED and the release,
    // while the timeout only bounds a failure: a loaded host may take several
    // seconds to deliver the release, so the timeout leaves it 25 s.
    // Hold the CLI lock until the release is sent.
    let window = harness.command_window()?;
    let mut command = window.base_command();
    let mut wait_child = command
        .args([
            "send-keys",
            "-t",
            "alpha:0.0",
            "--wait",
            "quiet",
            "--stable-for",
            "5s",
            "--timeout",
            "30s",
            "--",
            "printf ABA_QUIET_%s STARTED; read marker; printf ABA_QUIET_%s DONE",
            "Enter",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    assert_success(&window.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--text",
        "ABA_QUIET_STARTED",
        "--timeout",
        "3s",
    ])?);
    assert_success(&window.run(&[
        "rename-session",
        "-t",
        "alpha",
        "beta",
        ";",
        "new-session",
        "-d",
        "-s",
        "alpha",
    ])?);
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        wait_child.try_wait()?.is_none(),
        "quiet wait mistook a replacement session without the stable pane id for pane exit"
    );

    assert_success(&window.run(&["send-keys", "-t", "beta:0.0", "release", "Enter"])?);
    drop(window);
    let output = wait_child_with_timeout(wait_child, Duration::from_secs(30))?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "quiet wait did not remain bound to the original pane identity\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    Ok(())
}

#[test]
fn wait_pane_visible_text_follows_session_identity_across_rename() -> Result<(), Box<dyn Error>> {
    let mut harness = CliHarness::new("wait-pane-visible-rename")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    // The wait times out after 5 s: hold the CLI lock until the marker is printed.
    let window = harness.command_window()?;
    let mut command = window.base_command();
    let mut wait_child = command
        .args([
            "wait-pane",
            "-t",
            "alpha:0.0",
            "--visible-text",
            "DIRECT_WAIT_DONE",
            "--timeout",
            "5s",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        wait_child.try_wait()?.is_none(),
        "wait-pane must be armed before the rename"
    );

    assert_success(&window.run(&["rename-session", "-t", "alpha", "beta"])?);
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        wait_child.try_wait()?.is_none(),
        "wait-pane must remain armed after the target session is renamed"
    );
    assert_success(&window.run(&[
        "send-keys",
        "-t",
        "beta:0.0",
        "printf DIRECT_WAIT_%s DONE",
        "Enter",
    ])?);
    drop(window);

    let output = wait_child_with_timeout(wait_child, Duration::from_secs(3))?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "wait-pane failed to follow the stable session identity\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    Ok(())
}

#[test]
fn collect_pane_output_drains_until_pane_exit_eof() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-collect-output")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "keeper", "sleep 10"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "set-window-option",
        "-t",
        "alpha:0",
        "remain-on-exit",
        "on",
    ])?);
    assert_success(&harness.run(&[
        "respawn-pane",
        "-k",
        "-t",
        "alpha:0.0",
        "printf COLLECT_FINAL",
    ])?);
    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--pane-exit",
        "--timeout",
        "5s",
    ])?);

    let output = harness.run(&[
        "collect-pane-output",
        "-t",
        "alpha:0.0",
        "--until-pane-exit",
        "--max-bytes",
        "1024",
    ])?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "collect-pane-output failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(stdout(&output), "COLLECT_FINAL");
    assert!(stderr(&output).is_empty());
    Ok(())
}

// R3630 M1: `collect-pane-output --until-pane-exit` reports the collected
// pane's real exit status when its session is renamed during collection.

/// Blocks on a line from the pane's terminal, then exits non-zero. Echo is
/// off, so the gating Enter adds nothing to the collected output.
const GATED_EXIT_7: &str = "stty -echo; read gate; printf COLLECT_DONE; exit 7";
/// Longer than several collector polls, so it probes between two steps.
const COLLECTOR_POLLS: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy)]
enum SessionChange {
    None,
    Rename,
    RenameBack,
    RenameThenReuseOldName,
}

#[test]
fn collect_until_exit_reports_status_without_rename() -> Result<(), Box<dyn Error>> {
    assert_collected_exit_status(SessionChange::None)
}

#[test]
fn collect_until_exit_reports_status_after_session_rename() -> Result<(), Box<dyn Error>> {
    assert_collected_exit_status(SessionChange::Rename)
}

#[test]
fn collect_until_exit_reports_status_after_rename_back_to_old_name() -> Result<(), Box<dyn Error>> {
    assert_collected_exit_status(SessionChange::RenameBack)
}

#[test]
fn collect_until_exit_ignores_another_session_reusing_old_name() -> Result<(), Box<dyn Error>> {
    assert_collected_exit_status(SessionChange::RenameThenReuseOldName)
}

fn assert_collected_exit_status(change: SessionChange) -> Result<(), Box<dyn Error>> {
    let mut harness = CliHarness::new("automation-collect-rename")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", GATED_EXIT_7])?);
    assert_success(&harness.run(&[
        "set-window-option",
        "-t",
        "alpha:0",
        "remain-on-exit",
        "on",
    ])?);
    let pane_id =
        stdout(&harness.run(&["display-message", "-p", "-t", "alpha:0.0", "#{pane_id}"])?)
            .trim()
            .to_owned();

    let window = harness.command_window()?;
    let mut collector = window
        .base_command()
        .args([
            "collect-pane-output",
            "-t",
            "alpha:0.0",
            "--json",
            "--until-pane-exit",
            "--max-bytes",
            "4096",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    std::thread::sleep(COLLECTOR_POLLS);
    assert!(
        collector.try_wait()?.is_none(),
        "collect-pane-output must be armed before the session changes"
    );

    let final_name = match change {
        SessionChange::None => "alpha",
        SessionChange::Rename => {
            assert_success(&window.run(&["rename-session", "-t", "alpha", "beta"])?);
            "beta"
        }
        SessionChange::RenameBack => {
            assert_success(&window.run(&["rename-session", "-t", "alpha", "beta"])?);
            std::thread::sleep(COLLECTOR_POLLS);
            assert_success(&window.run(&["rename-session", "-t", "beta", "alpha"])?);
            "alpha"
        }
        SessionChange::RenameThenReuseOldName => {
            assert_success(&window.run(&["rename-session", "-t", "alpha", "beta"])?);
            assert_success(&window.run(&["new-session", "-d", "-s", "alpha", "sleep 30"])?);
            "beta"
        }
    };
    std::thread::sleep(COLLECTOR_POLLS);
    assert!(
        collector.try_wait()?.is_none(),
        "collect-pane-output must still wait for the running pane"
    );
    let gate_target = format!("{final_name}:0.0");
    assert_success(&window.run(&["send-keys", "-t", &gate_target, "Enter"])?);
    drop(window);

    let output = wait_child_with_timeout(collector, Duration::from_secs(10))?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "collect-pane-output failed ({change:?})\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let report: Value = serde_json::from_str(&stdout(&output))?;
    assert_eq!(report["ok"], Value::Bool(true), "{change:?}: {report}");
    assert_eq!(
        report["output_utf8_lossy"], "COLLECT_DONE",
        "{change:?}: {report}"
    );
    assert_eq!(
        report["pane_exit"],
        serde_json::json!({"stale": false, "exit_status": 7, "exit_signal": null}),
        "{change:?}: the collected pane's real exit status must survive: {report}"
    );

    // The pane is retained dead with that very status under its current name.
    let listed = harness.run(&[
        "list-panes",
        "-t",
        final_name,
        "-F",
        "#{pane_id} #{pane_dead} #{pane_dead_status}",
    ])?;
    assert_eq!(listed.status.code(), Some(0), "{}", stderr(&listed));
    assert_eq!(
        stdout(&listed).trim(),
        format!("{pane_id} 1 7"),
        "{change:?}"
    );
    Ok(())
}

// Hold the first cursor request at a transparent RPC proxy. The subscription
// is real, but cannot consume output before the rename and ring overflow.
#[test]
fn stream_lag_seed_without_rename_raw() -> Result<(), Box<dyn Error>> {
    assert_stream_lag_seed(SessionChange::None, "--raw")
}

#[test]
fn stream_lag_seed_without_rename_lines() -> Result<(), Box<dyn Error>> {
    assert_stream_lag_seed(SessionChange::None, "--lines")
}

#[test]
fn stream_lag_seed_after_rename_raw() -> Result<(), Box<dyn Error>> {
    assert_stream_lag_seed(SessionChange::Rename, "--raw")
}

#[test]
fn stream_lag_seed_after_rename_lines() -> Result<(), Box<dyn Error>> {
    assert_stream_lag_seed(SessionChange::Rename, "--lines")
}

#[test]
fn stream_lag_seed_after_name_reuse_raw() -> Result<(), Box<dyn Error>> {
    assert_stream_lag_seed(SessionChange::RenameThenReuseOldName, "--raw")
}

#[test]
fn stream_lag_seed_after_name_reuse_lines() -> Result<(), Box<dyn Error>> {
    assert_stream_lag_seed(SessionChange::RenameThenReuseOldName, "--lines")
}

fn assert_stream_lag_seed(change: SessionChange, mode: &str) -> Result<(), Box<dyn Error>> {
    use rmux_proto::{encode_frame, FrameDecoder, Request, Response};
    use std::io::Write;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;

    let mut harness = CliHarness::new("stream-lag-rename")?;
    let _daemon = harness.start_hidden_daemon()?;
    let flood_done = harness.tmpdir().join("flood-done");
    // Darwin's native PTY reads split this output into much smaller events.
    // Bound the flood below the client's RPC timeout on both native backends;
    // the observed PaneOutputLag below is mandatory, never inferred from bytes.
    let blocks = if cfg!(target_os = "macos") { 256 } else { 2050 };
    let producer = format!(
        "stty -echo; printf LAG_SEED_ORIGINAL; read gate; dd if=/dev/zero bs=65536 count={blocks} 2>/dev/null; touch {}; read finish",
        flood_done.display()
    );
    // NULs overflow the output ring without moving the visible marker. Keeping
    // that marker out of the retained bytes proves it came from the snapshot.
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", &producer])?);
    assert_success(&harness.run(&[
        "set-window-option",
        "-t",
        "alpha:0",
        "remain-on-exit",
        "on",
    ])?);
    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--text",
        "LAG_SEED_ORIGINAL",
        "--timeout",
        "10s",
    ])?);
    let proxy_path = harness.tmpdir().join("proxy.sock");
    let listener = UnixListener::bind(&proxy_path)?;
    listener.set_nonblocking(true)?;
    let daemon_path = harness.socket_path().to_owned();
    let (armed_tx, armed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (snapshot_tx, snapshot_rx) = mpsc::channel();
    let proxy = std::thread::spawn(move || -> Result<usize, String> {
        let serve = || -> Result<usize, Box<dyn Error>> {
            'connections: loop {
                let deadline = Instant::now() + Duration::from_secs(15);
                let (mut peer, _) = loop {
                    match listener.accept() {
                        Ok(peer) => break peer,
                        Err(error)
                            if error.kind() == io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(10))
                        }
                        Err(error) => return Err(error.into()),
                    }
                };
                peer.set_nonblocking(false)?;
                peer.set_read_timeout(Some(Duration::from_secs(90)))?;
                let mut connection = rmux_client::connect(&daemon_path)?;
                let mut decoder = FrameDecoder::new();
                let mut bytes = [0; 8192];
                let mut first_cursor = true;
                let mut lags = 0;
                loop {
                    if let Some(request) = decoder.next_frame::<Request>()? {
                        if first_cursor && matches!(request, Request::PaneOutputCursor(_)) {
                            armed_tx.send(())?;
                            release_rx.recv_timeout(Duration::from_secs(90))?;
                            first_cursor = false;
                        }
                        let response = connection.roundtrip(&request)?;
                        lags += usize::from(matches!(response, Response::PaneOutputLag(_)));
                        peer.write_all(&encode_frame(&response)?)?;
                        if matches!(response, Response::PaneSnapshot(_)) {
                            let _ = snapshot_tx.send(());
                        }
                    } else {
                        let count = peer.read(&mut bytes)?;
                        if count == 0 {
                            // CLI startup may probe the socket before its RPC connection.
                            if first_cursor {
                                continue 'connections;
                            }
                            return Ok(lags);
                        }
                        decoder.push_bytes(&bytes[..count]);
                    }
                }
            }
        };
        serve().map_err(|error| error.to_string())
    });
    let output_path = harness.tmpdir().join("stream.out");
    let window = harness.command_window()?;
    let mut stream = window
        .base_command()
        .arg("-S")
        .arg(&proxy_path)
        .args(["stream-pane", "-t", "alpha:0.0", mode])
        .stdin(Stdio::null())
        .stdout(fs::File::create(&output_path)?)
        .stderr(Stdio::piped())
        .spawn()?;
    let arrange = (|| -> Result<(), Box<dyn Error>> {
        armed_rx.recv_timeout(Duration::from_secs(15))?;
        let final_name = match change {
            SessionChange::None => "alpha",
            SessionChange::Rename | SessionChange::RenameThenReuseOldName => {
                assert_success(&window.run(&["rename-session", "-t", "alpha", "beta"])?);
                if matches!(change, SessionChange::RenameThenReuseOldName) {
                    assert_success(&window.run(&[
                        "new-session",
                        "-d",
                        "-s",
                        "alpha",
                        "printf WRONG_SESSION; sleep 30",
                    ])?);
                }
                "beta"
            }
            SessionChange::RenameBack => unreachable!(),
        };
        let target = format!("{final_name}:0.0");
        assert_success(&window.run(&["send-keys", "-t", &target, "Enter"])?);
        let deadline = Instant::now() + Duration::from_secs(60);
        while !flood_done.exists() {
            if Instant::now() >= deadline {
                return Err("pane did not finish the flood".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        release_tx.send(())?;
        match snapshot_rx.recv_timeout(Duration::from_secs(15)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
            Err(error) => return Err(error.into()),
        }
        assert_success(&window.run(&["send-keys", "-t", &target, "Enter"])?);
        Ok(())
    })();
    drop(release_tx);
    drop(window);
    if arrange.is_err() {
        let _ = stream.kill();
    }
    let output = wait_child_with_timeout(stream, Duration::from_secs(90));
    let lags = proxy.join().expect("proxy thread");
    if let Err(error) = arrange {
        return Err(format!("fixture failed: {error}; proxy: {lags:?}; stream: {output:?}").into());
    }
    let output = output?;
    assert!(
        lags.map_err(|error| io::Error::other(format!("{error}; stream: {output:?}")))? > 0,
        "fixture must observe a real daemon lag"
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{change:?} {mode}: {}",
        stderr(&output)
    );
    let bytes = fs::read(output_path)?;
    assert!(
        bytes.starts_with(b"LAG_SEED_ORIGINAL"),
        "snapshot must seed the original pane ({change:?} {mode}): {:?}",
        String::from_utf8_lossy(&bytes[..bytes.len().min(128)])
    );
    assert!(!String::from_utf8_lossy(&bytes).contains("WRONG_SESSION"));
    Ok(())
}

#[test]
fn stream_pane_lines_flushes_final_partial_line() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-stream-lines")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "keeper", "sleep 10"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&[
        "set-window-option",
        "-t",
        "alpha:0",
        "remain-on-exit",
        "on",
    ])?);
    assert_success(&harness.run(&[
        "respawn-pane",
        "-k",
        "-t",
        "alpha:0.0",
        "printf STREAM_FINAL",
    ])?);
    assert_success(&harness.run(&[
        "wait-pane",
        "-t",
        "alpha:0.0",
        "--pane-exit",
        "--timeout",
        "5s",
    ])?);

    let output = harness.run(&["stream-pane", "-t", "alpha:0.0", "--lines"])?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "stream-pane failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(stdout(&output), "STREAM_FINAL\n");
    assert!(stderr(&output).is_empty());
    Ok(())
}

#[test]
fn stream_pane_exits_cleanly_when_stdout_pipe_closes() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-stream-broken-pipe")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "yes BROKEN_PIPE"])?);
    let mut command = harness.base_command();
    let mut child = command
        .args(["stream-pane", "-t", "alpha:0.0", "--raw"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout_pipe = child.stdout.take().expect("stream stdout");
    let mut first_byte = [0_u8; 1];
    stdout_pipe.read_exact(&mut first_byte)?;
    drop(stdout_pipe);

    let output = wait_child_with_timeout(child, Duration::from_secs(2))?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "stream-pane should exit 0 after downstream closes stdout\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn stream_pane_pipeline_emits_before_hot_live_output_lag_starves_head() -> Result<(), Box<dyn Error>>
{
    let steps = StepClock::start();
    let harness = CliHarness::new("automation-stream-hot-head")?;
    steps.mark("harness");
    let _daemon = harness.start_hidden_daemon()?;
    steps.mark("daemon");
    let output_path = harness.tmpdir().join("stream-head.out");
    let error_path = harness.tmpdir().join("stream-head.err");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "yes BROKEN_PIPE"])?);
    steps.mark("new-session");

    let child = head_pipeline_command(
        "\"$RMUX_BIN\" -S \"$RMUX_SOCKET\" stream-pane -t alpha:0.0 --raw",
        &error_path,
        &output_path,
    )
    .env(
        "RMUX_BIN",
        rmux_os::build_identity::cargo_binary(env!("CARGO_BIN_EXE_rmux")),
    )
    .env("RMUX_SOCKET", harness.socket_path())
    .spawn()?;
    steps.mark(&format!("spawned pipeline group {}", child.id()));

    let output =
        wait_pipeline_group_with_timeout(child, Duration::from_secs(8), &error_path, &steps)?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "stream-pane | head should finish once the first byte is delivered\nstdout:\n{}\nstderr:\n{}\nstream stderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(&error_path).unwrap_or_default()
    );
    assert_eq!(
        fs::read(&output_path)?.len(),
        1,
        "head should capture exactly one byte"
    );
    Ok(())
}

/// A first stage that never writes stdout keeps `head` (and the shell's
/// stderr pipe it inherits) alive. On timeout the helper must kill the whole
/// pipeline group at once and surface the first stage's stderr, instead of
/// killing only `sh` and then blocking on the pipe the orphans still hold.
#[test]
fn pipeline_group_timeout_kills_every_stage_and_reports_stage_stderr() -> Result<(), Box<dyn Error>>
{
    let steps = StepClock::start();
    let harness = CliHarness::new("automation-pipeline-group-timeout")?;
    let output_path = harness.tmpdir().join("stage-head.out");
    let error_path = harness.tmpdir().join("stage-head.err");

    let child = head_pipeline_command(
        "{ echo STAGE_STDERR_MARKER >&2; exec sleep 30; }",
        &error_path,
        &output_path,
    )
    .spawn()?;
    let group = libc::pid_t::try_from(child.id())?;

    let started = Instant::now();
    let error =
        wait_pipeline_group_with_timeout(child, Duration::from_secs(1), &error_path, &steps)
            .expect_err("a first stage that never writes stdout must time out")
            .to_string();
    let returned_after = started.elapsed();

    assert!(
        returned_after < Duration::from_secs(10),
        "the timeout must not wait for orphaned stages; returned after {returned_after:?}\n{error}"
    );
    assert!(
        error.contains("STAGE_STDERR_MARKER"),
        "the timeout error must carry the first stage's stderr\n{error}"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_group_alive(group) {
        assert!(
            Instant::now() < deadline,
            "pipeline group {group} still has live members after the timeout\n{error}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[test]
fn with_session_kill_on_owner_exit_releases_name_immediately() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-with-session-kill")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "owned", "sleep", "60"])?);
    assert_success(&harness.run(&[
        "with-session",
        "owned",
        "--kill-on-owner-exit",
        "--ttl",
        "30s",
        "--",
        "sh",
        "-c",
        "true",
    ])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "owned", "sleep", "60"])?);
    Ok(())
}

fn pane_width(output: &str, pane_index: u32) -> Result<u16, Box<dyn Error>> {
    let prefix = format!("{pane_index}:");
    let width = output
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .ok_or_else(|| format!("pane {pane_index} missing from {output:?}"))?;
    Ok(width.parse()?)
}

/// Like [`assert_success`], but for commands that legitimately write to stdout.
fn assert_ok(output: &std::process::Output) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected successful command\nstdout:\n{}\nstderr:\n{}",
        stdout(output),
        stderr(output)
    );
    assert!(stderr(output).is_empty(), "stderr should be empty");
}

/// Returns `flag`'s own entry in a `--help` listing: the line that declares it
/// plus any wrapped continuation, with runs of whitespace collapsed.
///
/// Scoping to the entry is what makes the assertions per-flag: searching the
/// whole listing would let one documented flag vouch for its silent siblings.
/// The five duration flags are long-only, so clap always starts their
/// declaration at the beginning of the (indented) line.
fn help_entry(help: &str, flag: &str) -> Option<String> {
    let declares = |line: &str| {
        line.trim_start()
            .strip_prefix(flag)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    };
    let mut lines = help.lines().skip_while(|line| !declares(line));
    let declaration = lines.next()?;
    let continuation = lines.take_while(|line| {
        let trimmed = line.trim();
        !trimmed.is_empty() && !trimmed.starts_with('-')
    });
    let entry = std::iter::once(declaration)
        .chain(continuation)
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ");
    Some(entry)
}

/// Looks up the stable `%id` of a pane by its *display* index in `window`.
fn pane_id(harness: &CliHarness, window: &str, pane_index: u32) -> Result<String, Box<dyn Error>> {
    let output = harness.run(&["list-panes", "-t", window, "-F", "#{pane_index} #{pane_id}"])?;
    assert_ok(&output);
    let output = stdout(&output);
    let prefix = format!("{pane_index} ");
    output
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(str::to_owned)
        .ok_or_else(|| format!("pane {pane_index} missing from {output:?}").into())
}

fn run_json(harness: &CliHarness, args: &[&str]) -> Result<Value, Box<dyn Error>> {
    let output = harness.run(args)?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(stderr(&output).is_empty());
    Ok(serde_json::from_str(&stdout(&output))?)
}

/// Timestamps the steps of a test on stderr, so a slow or failing run shows
/// where its seconds went (harness lock, daemon start, command, pipeline).
struct StepClock {
    started: Instant,
}

impl StepClock {
    fn start() -> Self {
        Self {
            started: Instant::now(),
        }
    }

    fn mark(&self, step: &str) {
        eprintln!("STEP {:>8.3}s {step}", self.started.elapsed().as_secs_f64());
    }
}

/// `sh -c '<first_stage> 2>"$STAGE_ERR" | head -c 1 >"$HEAD_OUT"'`, with the
/// shell as leader of a new process group so a timeout can signal every stage.
fn head_pipeline_command(first_stage: &str, stage_err: &Path, head_out: &Path) -> Command {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(format!(
            "{first_stage} 2>\"$STAGE_ERR\" | head -c 1 >\"$HEAD_OUT\""
        ))
        .env("STAGE_ERR", stage_err)
        .env("HEAD_OUT", head_out)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    command
}

/// Waits for a pipeline spawned by [`head_pipeline_command`]. On timeout it
/// records the group's process tree, kills the whole group (the shell alone
/// would leave the stages orphaned, still holding its stderr pipe), and
/// returns an error carrying the first stage's stderr.
fn wait_pipeline_group_with_timeout(
    mut child: Child,
    timeout: Duration,
    stage_err: &Path,
    steps: &StepClock,
) -> Result<std::process::Output, Box<dyn Error>> {
    let group = libc::pid_t::try_from(child.id())?;
    let deadline = Instant::now() + timeout;
    while child.try_wait()?.is_none() {
        if Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        // The shell is unreaped here, so `group` still names this pipeline.
        let tree = process_group_tree(group);
        // SAFETY: killpg only sends a signal; it touches no memory.
        let killed = unsafe { libc::killpg(group, libc::SIGKILL) };
        let kill_error = (killed != 0).then(io::Error::last_os_error);
        steps.mark("timeout; killed pipeline group");
        let output = child.wait_with_output()?;
        steps.mark("pipes closed after group kill");
        let report = format!(
            "pipeline did not exit within {timeout:?}; killpg error: {kill_error:?}; \
             sh status: {:?}\nsh stderr:\n{}\nstage stderr:\n{}\npipeline group {group}:\n{tree}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
            fs::read_to_string(stage_err).unwrap_or_else(|error| format!("<{error}>")),
        );
        // `Result` errors print in Debug form; keep a readable copy on stderr.
        eprintln!("{report}");
        return Err(report.into());
    }
    steps.mark("sh exited; collecting pipes");
    let output = child.wait_with_output()?;
    steps.mark("pipes closed");
    Ok(output)
}

fn process_group_tree(group: libc::pid_t) -> String {
    let listing = match Command::new("ps")
        .args(["-axo", "pid,ppid,pgid,etime,stat,time,command"])
        .output()
    {
        Ok(listing) => listing,
        Err(error) => return format!("<ps failed: {error}>"),
    };
    let group = group.to_string();
    String::from_utf8_lossy(&listing.stdout)
        .lines()
        .enumerate()
        .filter(|(index, line)| {
            *index == 0 || line.split_whitespace().nth(2) == Some(group.as_str())
        })
        .map(|(_, line)| format!("{line}\n"))
        .collect()
}

/// Whether any process still belongs to `group`. A zombie counts until its
/// new parent reaps it, so callers poll.
fn process_group_alive(group: libc::pid_t) -> bool {
    // SAFETY: signal 0 only checks for existence and permission.
    unsafe { libc::killpg(group, 0) == 0 }
}

fn wait_child_with_timeout(
    mut child: Child,
    timeout: Duration,
) -> Result<std::process::Output, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output()?;
            return Err(format!(
                "child did not exit within {timeout:?}; status: {:?}; stderr:\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// issue #62: `with-session` given no child command must fail with a message,
/// never panic on the empty command vector.
#[test]
fn issue_62_with_session_without_child_command_fails_without_panic() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("automation-with-session-no-command")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "x", "sleep", "60"])?);

    for args in [&["with-session", "x"][..], &["with-session", "x", "--"][..]] {
        let output = harness.run(args)?;
        let error = stderr(&output);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{args:?} must fail with status 1\nstderr:\n{error}"
        );
        assert!(
            error.contains("child command"),
            "{args:?} must name the missing child command\nstderr:\n{error}"
        );
        assert!(
            !error.contains("panicked"),
            "{args:?} must not panic\nstderr:\n{error}"
        );
    }
    assert_success(&harness.run(&["has-session", "-t", "x"])?);
    Ok(())
}
