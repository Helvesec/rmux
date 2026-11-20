use super::super::support::*;
use super::copy_viewport::{wait_for_viewports, CopyViewport, ViewportPhase};

/// A shell that draws its prompt late must not change the fixture. Keys sent to a
/// pane before its line editor starts are echoed by the tty and stay in scrollback,
/// so the top of history would differ between the two servers.
#[test]
fn copy_top_matches_tmux_when_only_the_rmux_shell_prompt_is_late() -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("copy-mode-late-prompt")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let _guard = pty_tmux_compat_lock();
    let deadline = AttachedClientDeadline::new();
    let config = attached_client_config().without_env("RMUX");

    // Only rmux panes export RMUX. Every startup file a login shell may read
    // delays that pane alone.
    let home = harness.tmpdir().join("home");
    for file_name in [".bash_profile", ".bashrc", ".profile", ".zprofile"] {
        let path = home.join(file_name);
        let startup = fs::read_to_string(&path)?;
        fs::write(&path, format!("[ -n \"$RMUX\" ] && sleep 1\n{startup}"))?;
    }

    attached_client_new_session(&harness, &tmux_binary, config.clone(), &deadline)?;
    let mut rmux_attach = spawn_rmux_attached_input_client(&harness, "alpha")?;
    let mut tmux_attach = spawn_tmux_attached_input_client(&harness, &tmux_binary, "alpha")?;
    wait_for_attached_clients(&harness, &tmux_binary, config.clone(), &deadline)?;
    let mut observed = [CopyViewport::new()?, CopyViewport::new()?];
    wait_for_viewports(
        rmux_attach.master_mut(),
        tmux_attach.master_mut(),
        &mut observed,
        ViewportPhase::Live,
        TMUX_COMPAT_TIMEOUT.min(deadline.remaining()?),
    )?;

    let copy_mode_u = harness.run_pair_with(
        &tmux_binary,
        &["copy-mode", "-u", "-t", "alpha:0.0"],
        config,
    )?;
    assert_quiet_success(&copy_mode_u);
    wait_for_viewports(
        rmux_attach.master_mut(),
        tmux_attach.master_mut(),
        &mut observed,
        ViewportPhase::CopyTop,
        TMUX_COMPAT_TIMEOUT.min(deadline.remaining()?),
    )?;

    rmux_attach.assert_running("rmux")?;
    tmux_attach.assert_running("tmux")?;
    drop(rmux_attach);
    drop(tmux_attach);
    shutdown_attached_rmux(&harness)?;
    Ok(())
}
