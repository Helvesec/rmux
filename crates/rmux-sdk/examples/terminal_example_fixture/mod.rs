use std::time::Duration;

use rmux_sdk::{EnsureSession, Pane, Result, Rmux, Session, TerminalSizeSpec};

#[allow(dead_code)]
pub(crate) struct ExamplePane {
    pub(crate) rmux: Rmux,
    pub(crate) session: Session,
    pub(crate) pane: Pane,
}

pub(crate) async fn ready_pane(example_name: &str) -> Result<ExamplePane> {
    let rmux = Rmux::builder()
        .default_timeout(Duration::from_secs(10))
        .connect_or_start()
        .await?;
    let session = rmux
        .ensure_session(
            EnsureSession::try_named(format!("rmux-sdk-{example_name}-{}", std::process::id()))?
                .create_only()
                .detached(true)
                .size(TerminalSizeSpec::new(80, 24))
                .argv(interactive_shell()),
        )
        .await?;
    let pane = session.pane(0, 0);
    pane.send_text(ready_command()).await?;
    pane.wait_for_text("Ready").await?;

    Ok(ExamplePane {
        rmux,
        session,
        pane,
    })
}

#[cfg(unix)]
fn interactive_shell() -> Vec<String> {
    vec!["sh".to_owned()]
}

#[cfg(windows)]
fn interactive_shell() -> Vec<String> {
    vec!["cmd.exe".to_owned(), "/Q".to_owned(), "/K".to_owned()]
}

#[cfg(unix)]
fn ready_command() -> &'static str {
    "printf 'Ready\\n'\n"
}

#[cfg(windows)]
fn ready_command() -> &'static str {
    "echo Ready\r"
}

#[cfg(unix)]
#[allow(dead_code)]
pub(crate) fn print_command(text: &str) -> String {
    let escaped = text.replace('\'', r"'\''");
    format!("printf '{escaped}\\n'\n")
}

#[cfg(windows)]
#[allow(dead_code)]
pub(crate) fn print_command(text: &str) -> String {
    format!("echo {text}\r")
}
