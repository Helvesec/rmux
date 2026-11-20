#![cfg(unix)]

mod common;

use std::error::Error;

use common::{assert_success, stderr, stdout, CliHarness};
use serde_json::Value;

#[test]
fn window_json_layout_matches_direct_all_and_format_paths() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("window-json-layout")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["rename-window", "-t", "alpha:0", "window\x1frow\x1e\n雪"])?);

    for scene in ["single", "split", "zoomed"] {
        if scene == "split" {
            assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0"])?);
            assert_success(&harness.run(&["select-layout", "-t", "alpha:0", "main-vertical"])?);
        } else if scene == "zoomed" {
            assert_success(&harness.run(&["resize-pane", "-Z", "-t", "alpha:0.0"])?);
        }
        let formatted = harness.run(&["list-windows", "-t", "alpha", "-F", "#{window_layout}"])?;
        assert!(formatted.status.success(), "{}", stderr(&formatted));
        assert!(stderr(&formatted).is_empty());
        let layout = stdout(&formatted);
        if scene == "single" {
            // tmux 3.7b: detached 80x24 window with pane %0.
            assert_eq!(layout.trim_end(), "b25d,80x24,0,0,0");
        }
        let mut rows = Vec::new();
        for args in [
            vec!["list-windows", "-t", "alpha", "--json"],
            vec!["list-windows", "-a", "--json"],
            vec!["list-windows", "-t", "alpha", "--json", "-f", "1"],
            vec!["list-windows", "-a", "--json", "-f", "1"],
            vec![
                "list-windows",
                "-t",
                "alpha",
                "--json",
                "-f",
                "#{window_name}",
            ],
            vec!["list-windows", "-a", "--json", "-f", "#{window_name}"],
        ] {
            let output = harness.run(&args)?;
            assert!(output.status.success(), "{}", stderr(&output));
            assert!(stderr(&output).is_empty());
            let value: Value = serde_json::from_str(&stdout(&output))?;
            assert_eq!(value.as_array().unwrap().len(), 1, "{scene} {args:?}");
            assert_eq!(
                value[0]["window_layout"],
                layout.trim_end(),
                "{scene} {args:?}"
            );
            assert_eq!(value[0]["window_name"], "window\x1frow\x1e\n雪");
            rows.push(value);
        }
        for row in &rows[1..] {
            assert_eq!(row, &rows[0], "{scene}");
        }
    }
    Ok(())
}

#[test]
fn window_json_layout_preserves_filter_and_sort() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("window-json-layout-options")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-n", "a"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:", "-n", "雪\x1f\x1e\n"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:", "-n", "z"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:1"])?);
    let mut rows = Vec::new();
    for scope in [vec!["-t", "alpha"], vec!["-a"]] {
        let mut args = vec!["list-windows"];
        args.extend(scope);
        args.extend([
            "--json",
            "-f",
            "#{!=:#{window_index},0}",
            "-O",
            "index",
            "-r",
        ]);
        let output = harness.run(&args)?;
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(stderr(&output).is_empty());
        let value: Value = serde_json::from_str(&stdout(&output))?;
        let windows = value.as_array().unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0]["window_index"], 2);
        assert_eq!(windows[1]["window_index"], 1);
        assert_eq!(windows[1]["window_name"], "雪\x1f\x1e\n");
        for window in windows {
            let target = format!("alpha:{}", window["window_index"]);
            let formatted =
                harness.run(&["display-message", "-p", "-t", &target, "#{window_layout}"])?;
            assert!(formatted.status.success(), "{}", stderr(&formatted));
            assert!(stderr(&formatted).is_empty());
            assert_eq!(window["window_layout"], stdout(&formatted).trim_end());
        }
        rows.push(value);
        let filter = args.iter().position(|arg| *arg == "-f").unwrap();
        args[filter + 1] = "0";
        let output = harness.run(&args)?;
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(stderr(&output).is_empty());
        assert_eq!(
            serde_json::from_str::<Value>(&stdout(&output))?,
            serde_json::json!([])
        );
    }
    assert_eq!(rows[0], rows[1]);
    Ok(())
}

#[test]
fn window_json_filter_preserves_full_value_truth_and_sort() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("window-json-filter")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:", "-n", "z"])?);

    // Empty and exactly "0" are false; a separator after either makes it true.
    for name in [
        "",
        "0",
        "00",
        "false",
        "\x1f",
        "0\x1frow\x1e\n雪",
        "雪,}:#\n",
    ] {
        assert_success(&harness.run(&["rename-window", "-t", "alpha:0", name])?);
        let expected_count = if name.is_empty() || name == "0" { 1 } else { 2 };
        for scope in [vec!["-t", "alpha"], vec!["-a"]] {
            for order in ["index", "name", "size", "activity", "creation"] {
                let mut args = vec!["list-windows"];
                args.extend_from_slice(&scope);
                args.extend(["--json", "-f", "#{window_name}", "-O", order, "-r"]);
                let output = harness.run(&args)?;
                assert!(
                    output.status.success(),
                    "{name:?} {args:?}: {}",
                    stderr(&output)
                );
                assert!(stderr(&output).is_empty());
                let value: Value = serde_json::from_str(&stdout(&output))?;
                let rows = value.as_array().unwrap();
                assert_eq!(rows.len(), expected_count, "{name:?} {args:?}");
                assert_eq!(
                    rows.iter().filter(|row| row["window_name"] == name).count(),
                    expected_count - 1
                );
                if order == "index" {
                    assert_eq!(rows[0]["window_index"], 1);
                }

                // Text uses the same prefix but does not pass through the JSON decoder.
                let mut text_args = vec!["list-windows"];
                text_args.extend_from_slice(&scope);
                text_args.extend([
                    "-F",
                    "#{window_index}",
                    "-f",
                    "#{window_name}",
                    "-O",
                    order,
                    "-r",
                ]);
                let output = harness.run(&text_args)?;
                assert!(output.status.success(), "{}", stderr(&output));
                assert!(stderr(&output).is_empty());
                let expected = rows
                    .iter()
                    .map(|row| format!("{}\n", row["window_index"]))
                    .collect::<String>();
                assert_eq!(stdout(&output), expected, "{name:?} {text_args:?}");
            }
            for filter in ["", "0", "#{missing_filter_variable}"] {
                let mut args = vec!["list-windows"];
                args.extend_from_slice(&scope);
                args.extend(["--json", "-f", filter]);
                let output = harness.run(&args)?;
                assert!(output.status.success(), "{}", stderr(&output));
                assert!(stderr(&output).is_empty());
                assert_eq!(
                    serde_json::from_str::<Value>(&stdout(&output))?,
                    serde_json::json!([])
                );
            }
        }
    }
    Ok(())
}
