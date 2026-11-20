use super::*;
use rmux_core::formats::TMUX_FORMAT_TABLE_NAMES;

#[test]
fn pane_xyz_tokens_match_frozen_tmux_in_80x24_sleep_scene() -> Result<(), Box<dyn std::error::Error>>
{
    let tmux_binary = match FrozenTmuxBinary::discover() {
        FrozenTmuxBinary::Available(path) => path,
        FrozenTmuxBinary::Unavailable {
            checked_path,
            reason,
        } => {
            eprintln!(
                "runtime skip: frozen tmux binary '{}' unavailable: {reason}",
                checked_path.display()
            );
            return Ok(());
        }
    };
    let harness = TmuxCompatHarness::new("formats-pane-xyz-tmux-compat")?;
    let create = harness.run_pair(
        &tmux_binary,
        &[
            "new-session",
            "-d",
            "-s",
            "oracle",
            "-x",
            "80",
            "-y",
            "24",
            "/bin/sleep",
            "120",
        ],
    )?;
    assert_exact_tmux_compat(&create);
    let status = harness.run_pair(
        &tmux_binary,
        &["set-option", "-t", "oracle", "status", "off"],
    )?;
    assert_exact_tmux_compat(&status);
    let dimensions = harness.run_pair(
        &tmux_binary,
        &[
            "display-message",
            "-p",
            "-t",
            "oracle:0.0",
            "#{window_width}x#{window_height}|#{pane_width}x#{pane_height}",
        ],
    )?;
    assert_exact_tmux_compat(&dimensions);
    assert_eq!(dimensions.rmux.stdout_string(), "80x24|80x24\n");

    // tmux 3.7b: one pane, 80x24, /bin/sleep 120, status off.
    for (name, expected) in [("pane_x", "0\n"), ("pane_y", "0\n"), ("pane_z", "1\n")] {
        assert!(
            TMUX_FORMAT_TABLE_NAMES.contains(&name),
            "missing table token {name}"
        );
        assert!(is_known_format_variable_name(name), "unknown token {name}");
        let template = format!("#{{{name}}}");
        let display = harness.run_pair(
            &tmux_binary,
            &["display-message", "-p", "-t", "oracle:0.0", &template],
        )?;
        assert_exact_tmux_compat(&display);
        assert_eq!(display.tmux.stdout_string(), expected, "tmux {name}");
        assert_eq!(display.rmux.stdout_string(), expected, "rmux {name}");
        assert!(
            include_str!("../conformance/format_tokens.txt")
                .lines()
                .any(|line| line == name),
            "conformance fixture omits {name}"
        );
    }
    Ok(())
}
