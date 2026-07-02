mod common_cross;

use std::error::Error;
use std::ffi::OsStr;
use std::fs;

use common_cross::CrossPlatformHarness;

#[test]
fn target_formats_and_split_cwd_are_consistent_across_surfaces() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("target-format-matrix")?;
    let cwd = harness.tmpdir().join("pane cwd café");
    fs::create_dir_all(&cwd)?;

    harness.success([
        OsStr::new("new-session"),
        OsStr::new("-d"),
        OsStr::new("-s"),
        OsStr::new("fmt"),
        OsStr::new("-n"),
        OsStr::new("main"),
        OsStr::new("-c"),
        cwd.as_os_str(),
        OsStr::new("-x"),
        OsStr::new("100"),
        OsStr::new("-y"),
        OsStr::new("30"),
    ])?;

    harness.success([
        "split-window",
        "-d",
        "-t",
        "fmt:0.0",
        "-c",
        "#{pane_current_path}",
    ])?;

    let panes = harness.stdout([
        "list-panes",
        "-t",
        "fmt:0",
        "-F",
        "#{session_name}|#{window_index}|#{window_name}|#{pane_index}|#{pane_active}|#{pane_current_path}",
    ])?;
    let lines = panes.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2, "expected two panes, got {panes:?}");

    let parsed = lines
        .iter()
        .map(|line| line.split('|').collect::<Vec<_>>())
        .collect::<Vec<_>>();
    for fields in &parsed {
        assert_eq!(fields.len(), 6, "bad list-panes format line: {fields:?}");
        assert_eq!(fields[0], "fmt");
        assert_eq!(fields[1], "0");
        assert_eq!(fields[2], "main");
        assert!(
            fields[3] == "0" || fields[3] == "1",
            "bad pane_index: {fields:?}"
        );
        assert!(
            fields[4] == "0" || fields[4] == "1",
            "bad pane_active: {fields:?}"
        );
    }
    assert_eq!(
        parsed[0][5], parsed[1][5],
        "split-window -c '#{{pane_current_path}}' should inherit the target pane cwd; panes={panes:?}"
    );
    assert!(
        !parsed[0][5].is_empty(),
        "pane_current_path must be populated on list-panes"
    );

    let display = harness.stdout([
        "display-message",
        "-p",
        "-t",
        "fmt:0.0",
        "#{session_name}|#{window_index}|#{window_name}|#{pane_index}|#{pane_current_path}",
    ])?;
    let fields = display.trim().split('|').collect::<Vec<_>>();
    assert_eq!(fields.len(), 5, "bad display-message format: {display:?}");
    assert_eq!(&fields[..4], &["fmt", "0", "main", "0"]);
    assert_eq!(fields[4], parsed[0][5]);

    Ok(())
}

#[test]
fn rename_session_preserves_capture_and_format_targeting() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("rename-format-target")?;
    let marker = format!("rename_capture_marker_{}", std::process::id());

    harness.success(["new-session", "-d", "-s", "before"])?;
    harness.success(["send-keys", "-t", "before:0.0", &marker])?;
    harness.wait_for_capture_contains("before:0.0", &marker)?;

    harness.success(["rename-session", "-t", "before", "after"])?;

    let old = harness.run(["has-session", "-t", "before"])?;
    assert_eq!(
        old.status.code(),
        Some(1),
        "old session target should be gone"
    );
    harness.success(["has-session", "-t", "after"])?;
    harness.wait_for_capture_contains("after:0.0", &marker)?;

    let session = harness.stdout([
        "display-message",
        "-p",
        "-t",
        "after:0.0",
        "#{session_name}",
    ])?;
    assert_eq!(session.trim(), "after");

    Ok(())
}
