use super::super::support::*;
use super::copy_viewport::{wait_for_viewports, CopyViewport, ViewportPhase};

/// Both servers start without SHELL, whatever the runner exports. tmux 3.7b then
/// takes the account shell from getpwuid, and the shell's startup output decides
/// the scrollback that copy mode reports. On a macOS account absent from
/// /etc/passwd, a passwd-file lookup would start /bin/sh instead, which leaves one
/// history line fewer and never matches the CopyBottom or CopyTop viewport.
#[test]
fn copy_mode_viewports_match_tmux_when_both_servers_start_without_shell(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("copy-mode-shell-absent")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let _guard = pty_tmux_compat_lock();
    let deadline = AttachedClientDeadline::new();
    let config = attached_client_config().without_env("SHELL");

    attached_client_new_session(&harness, &tmux_binary, config.clone(), &deadline)?;
    let default_shell = harness.run_pair_with(
        &tmux_binary,
        &["show-options", "-gv", "default-shell"],
        config.clone(),
    )?;
    assert_eq!(default_shell.tmux.status_code, Some(0));
    assert_eq!(
        default_shell.rmux.stdout_string(),
        default_shell.tmux.stdout_string(),
        "without SHELL, rmux must start the account shell that tmux starts"
    );

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

    let copy_mode = harness.run_pair_with(
        &tmux_binary,
        &["copy-mode", "-t", "alpha:0.0"],
        config.clone(),
    )?;
    assert_quiet_success(&copy_mode);
    wait_for_viewports(
        rmux_attach.master_mut(),
        tmux_attach.master_mut(),
        &mut observed,
        ViewportPhase::CopyBottom,
        TMUX_COMPAT_TIMEOUT.min(deadline.remaining()?),
    )?;

    deadline.check()?;
    rmux_attach.master_mut().write_all(b"\x1b")?;
    tmux_attach.master_mut().write_all(b"\x1b")?;
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
