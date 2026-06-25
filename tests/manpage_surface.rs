#![cfg(unix)]

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

use rmux_core::command_parser::COMMAND_TABLE;

#[test]
fn rmux_manpage_renders_with_system_formatter() -> Result<(), Box<dyn Error>> {
    let manpage = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/man/rmux.1");
    let rendered = render_manpage(&manpage)?;
    let rendered = strip_backspace_overstrikes(&rendered);

    assert!(rendered.contains("RMUX"));
    assert!(rendered.contains("list-commands"));
    assert!(rendered.contains("choose-window"));
    assert!(rendered.contains("display-menu"));
    assert!(rendered.contains("display-popup"));
    assert!(rendered.contains("clear-prompt-history"));
    assert!(rendered.contains("show-prompt-history"));
    for entry in COMMAND_TABLE {
        assert!(
            rendered.contains(entry.name),
            "expected rendered manpage to expose command {}",
            entry.name
        );
    }
    Ok(())
}

fn render_manpage(manpage: &Path) -> Result<String, Box<dyn Error>> {
    let man_output = Command::new("man")
        .arg("-l")
        .arg(manpage)
        .env("MANPAGER", "cat")
        .env("PAGER", "cat")
        .output()?;

    if man_output.status.success() {
        return Ok(String::from_utf8_lossy(&man_output.stdout).into_owned());
    }

    let mandoc_output = Command::new("mandoc").arg(manpage).output()?;
    if mandoc_output.status.success() {
        return Ok(String::from_utf8_lossy(&mandoc_output.stdout).into_owned());
    }

    Err(format!(
        "failed to render manpage with man -l ({}) or mandoc ({})",
        String::from_utf8_lossy(&man_output.stderr),
        String::from_utf8_lossy(&mandoc_output.stderr)
    )
    .into())
}

fn strip_backspace_overstrikes(input: &str) -> String {
    let mut stripped = Vec::with_capacity(input.len());
    for ch in input.chars() {
        if ch == '\u{8}' {
            let _ = stripped.pop();
        } else {
            stripped.push(ch);
        }
    }
    stripped.into_iter().collect()
}
