//! Shared boilerplate for `sdk_demo_*.rs` examples.
//!
//! Each SDK demo scenario only wants to show 2–4 SDK lines, but the
//! capture pipeline needs a deterministic shell (no rc-files, fixed prompt,
//! fixed locale) so the recorded ANSI is reproducible. Pulling that
//! plumbing out of the per-scenario file keeps the source code that ships
//! in the SDK demo UI honest: it shows the SDK call you'd actually write,
//! not the capture support code.
//!
//! This file is brought into each example via `#[path]`. `#[allow(dead_code)]`
//! because individual examples only consume a subset of the helpers.

#![allow(dead_code)]

use std::time::Duration;

use rmux_sdk::{EnsureSession, Rmux, Session, TerminalSizeSpec};

#[cfg(unix)]
fn shell_command() -> [&'static str; 3] {
    [
        "bash",
        "-c",
        "printf 'BANNER\\n'; exec bash --noprofile --norc -i",
    ]
}

#[cfg(windows)]
fn shell_command() -> [&'static str; 3] {
    ["cmd", "/Q", "/K echo BANNER"]
}

/// Colored prompt rmux's daemon paints in freshly-spawned panes. Painting
/// the same prompt from the example keeps every captured frame visually
/// identical regardless of which side spawned the shell.
const PROMPT: &str = "\\033[36muser@rmuxio\\033[0m:\\033[32m~/workspace\\033[0m$ ";

/// Connects to (or starts) the daemon and returns a deterministic session
/// whose pane 0 has finished spawning its shell.
///
/// The shell is interactive but the screen is still blank — call
/// [`paint_idle_prompt`] or one of the `paint_*` helpers to draw the
/// scenario's demo frame.
pub(crate) async fn demo_session(name: &str) -> rmux_sdk::Result<(Rmux, Session)> {
    let rmux = Rmux::builder()
        .default_timeout(Duration::from_secs(5))
        .connect_or_start()
        .await?;
    let session = rmux
        .ensure_session(
            EnsureSession::try_named(name)?
                .create_or_reuse()
                .detached(true)
                .size(TerminalSizeSpec::new(80, 24))
                .argv(shell_command())
                .environment(["LC_ALL=C.UTF-8", "TZ=UTC", "TERM=xterm-256color"]),
        )
        .await?;
    let pane = session.pane(0, 0);
    pane.wait_for_text("BANNER").await?;
    Ok((rmux, session))
}

/// Paints the colored rmux prompt with no command, then hands the
/// shell off to `cat` so any input from a snippet under capture is
/// silently swallowed (the screen stays frozen on the prompt). The
/// earlier `read -r _` form let the snippet's `send_text+Enter`
/// consume the read and drop bash back to its `bash-5.2$` fallback,
/// which corrupted the captured snapshot deterministically.
pub(crate) async fn paint_idle_prompt(session: &Session) -> rmux_sdk::Result<()> {
    let pane = session.pane(0, 0);
    let paint = format!(
        "clear; printf '{PROMPT}'; \
         stty -echo -icanon < /dev/tty 2>/dev/null; \
         exec cat > /dev/null"
    );
    pane.send_text(&paint).await?;
    pane.send_key("Enter").await?;
    Ok(())
}

/// Paints a one-shot `echo hello` demonstration frame on the demo pane.
///
/// Draws the entire `prompt > echo hello / hello / prompt >` sequence in
/// one shot, then blocks the shell on `read -r _` so the captured fixture
/// is reproducible.
pub(crate) async fn paint_echo_hello(session: &Session) -> rmux_sdk::Result<()> {
    paint_command_run(session, "echo hello", "hello").await
}

/// Paints a one-shot `uname -s` demonstration frame on the demo pane.
pub(crate) async fn paint_uname(session: &Session) -> rmux_sdk::Result<()> {
    paint_command_run(session, "uname -s", "Linux").await
}

async fn paint_command_run(
    session: &Session,
    command: &str,
    expected_output: &str,
) -> rmux_sdk::Result<()> {
    let pane = session.pane(0, 0);
    // `exec cat > /dev/null` replaces the shell with a process that
    // silently swallows stdin — the screen is frozen on exactly the
    // two visible rows (`prompt+command` and `output`). The previous
    // `printf '{PROMPT}'; read -r _` form left a third "next prompt"
    // line that the snippet's `send_text+Enter` would then consume,
    // dragging bash back to its raw `bash-5.2$` fallback and breaking
    // the captured snapshot's hash determinism. With `exec cat` the
    // post-snippet state is identical to the pre-snippet state, so
    // `sdk-demo capture` produces stable bytes across runs.
    let cmd = format!(
        "clear; \
         printf '{PROMPT}{command}\\n'; \
         {command}; \
         stty -echo -icanon < /dev/tty 2>/dev/null; \
         exec cat > /dev/null"
    );
    pane.send_text(&cmd).await?;
    pane.send_key("Enter").await?;
    pane.wait_for_text(expected_output).await?;
    Ok(())
}

/// Paints arbitrary literal `lines` onto the demo pane and parks the
/// shell on `read -r _` so the captured fixture is frozen on that
/// state. Each line is emitted via `printf '...\n'` (so escape codes
/// are honoured). Use this when the snippet itself isn't producing
/// the visual you want to record — e.g. "server started" status
/// lines for the detached-app scenario, mock HTTP log lines for the
/// reconnect scenario.
pub(crate) async fn paint_lines(session: &Session, lines: &[&str]) -> rmux_sdk::Result<()> {
    let pane = session.pane(0, 0);
    let mut script = String::from("clear");
    for line in lines {
        // Each line is shell-escaped via single-quote: any embedded
        // single quote is encoded as `'\''`.
        let escaped = line.replace('\'', r"'\''");
        script.push_str("; printf '");
        script.push_str(&escaped);
        script.push_str("\\n'");
    }
    script.push_str("; stty -echo -icanon < /dev/tty 2>/dev/null; exec cat > /dev/null");
    pane.send_text(&script).await?;
    pane.send_key("Enter").await?;
    if let Some(last) = lines.last().and_then(|s| s.split('\\').next()) {
        // Wait for a stable substring of the final line so the
        // capture only runs after the paint has landed.
        let needle: String = last.chars().take(8).collect();
        if !needle.trim().is_empty() {
            pane.wait_for_text(&needle).await?;
        }
    }
    Ok(())
}

/// Kills the session unless `RMUX_SDK_DEMO_KEEP_SESSION` is set in the
/// environment — handy when iterating locally with `rmux attach`.
pub(crate) async fn cleanup(session: Session) -> rmux_sdk::Result<()> {
    if std::env::var_os("RMUX_SDK_DEMO_KEEP_SESSION").is_none() {
        let _ = session.kill().await?;
    }
    Ok(())
}

/// Creates a throwaway companion session that the scenario can safely
/// consume (kill, close, etc.) without disturbing the demo session that
/// the fixture pipeline is recording.
pub(crate) async fn throwaway_session(rmux: &Rmux, name: &str) -> rmux_sdk::Result<Session> {
    rmux.ensure_session(
        EnsureSession::try_named(name)?
            .create_or_reuse()
            .detached(true)
            .size(TerminalSizeSpec::new(80, 24))
            .argv(shell_command())
            .environment(["LC_ALL=C.UTF-8"]),
    )
    .await
}
