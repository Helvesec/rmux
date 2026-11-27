use super::*;

pub(crate) fn run<I, T>(args: I) -> Result<i32, ExitFailure>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let mut automation = automation::InvocationBudget::from_args(&args);
    let result = run_inner(args, &mut automation);
    automation.finish(result)
}

fn run_inner(
    args: Vec<OsString>,
    automation: &mut automation::InvocationBudget,
) -> Result<i32, ExitFailure> {
    let tmux_compatible = invoked_as_tmux(&args);
    if let Some(decision) = top_level_decision(args.get(1..).unwrap_or(&[]), tmux_compatible) {
        return Err(decision.into_exit(tmux_compatible));
    }
    let args = crate::cli_top_level::normalize_legacy_q(args);
    let claude_invocation = scan_claude_top_level_invocation(args.get(1..).unwrap_or(&[]));
    validate_claude_top_level_invocation(claude_invocation.as_ref())?;
    if let Some(invocation) = claude_launcher::parse_internal_runner(args.get(1..).unwrap_or(&[])) {
        return claude_launcher::run_internal_runner(invocation);
    }
    if let Some(invocation) = diagnose::parse_invocation(args.get(1..).unwrap_or(&[]))? {
        return diagnose::run(invocation);
    }
    if let Some(invocation) = tmux_dropin::parse_invocation(args.get(1..).unwrap_or(&[]))? {
        return tmux_dropin::run(invocation, args.first());
    }
    if let Some(claude_invocation) = claude_invocation {
        if let Some(invocation) = claude_skill::parse_invocation(claude_invocation.arguments())? {
            return claude_skill::run(invocation);
        }
        return claude_launcher::run(claude_launcher::ClaudeInvocation::new(
            claude_invocation.into_arguments(),
        ));
    }
    if let Some(invocation) = capabilities::parse_invocation(args.get(1..).unwrap_or(&[]))? {
        return capabilities::run(invocation);
    }
    let runtime_resolution = alias_fallback::runtime_command_resolution_for_invocation(
        &args,
        invoked_as_tmux(&args),
        automation.deadline(),
    )?;
    if let Some(alias_fallback::RuntimeCommandResolution::LegacyServerDispatch(exit_code)) =
        runtime_resolution.as_ref()
    {
        return Ok(*exit_code);
    }
    let parsed_cli = parse_with_runtime_resolution(&args, runtime_resolution.as_ref());
    let mut prestarted_endpoint = None;
    let mut cli = match parsed_cli {
        Ok(cli) => cli,
        Err(error) if runtime_resolution.is_some() => {
            let control_mode = scan_top_level_command(args.get(1..).unwrap_or(&[]))
                .map_or(0, |scan| scan.control_mode);
            if control_mode != 0 {
                return Err(control_mode_parse_failure(error, control_mode));
            }
            return Err(ExitFailure::from_clap(error));
        }
        Err(error) if error.kind() == clap::error::ErrorKind::InvalidSubcommand => {
            match parse_cold_alias_queue_after_startup(&args, error)? {
                ColdAliasParseOutcome::NotApplicable(error) => {
                    return parse_failure_or_absent_server(&args, error);
                }
                ColdAliasParseOutcome::Parsed(cold_cli, endpoint) => {
                    prestarted_endpoint = Some(endpoint);
                    *cold_cli
                }
                ColdAliasParseOutcome::Dispatched(exit_code) => return Ok(exit_code),
            }
        }
        Err(error) => return parse_failure_or_absent_server(&args, error),
    };
    cli.utf8 |= infer_client_utf8_from_env();
    let command_was_provided = cli.command.is_some();
    validate_top_level_invocation(&cli, command_was_provided)?;
    accept_compatibility_options(&cli);
    let mut startup_config = startup_config_from_cli(&cli);

    let mut socket_path = if invoked_as_tmux(&args) {
        resolve_tmux_compatible_socket_path(cli.socket_name(), cli.socket_path())
    } else {
        resolve_socket_path(cli.socket_name(), cli.socket_path())
    }
    .map_err(ExitFailure::from_client)?;

    if let Some(crate::cli_args::Command::AttachSession(args)) = cli.command.as_ref() {
        validate_nested_attach_before_connect(args, &socket_path)?;
    }

    // A start-server command may create the daemon that loads command-alias
    // definitions from `-f`. Resolve the original argv only after that config
    // is ready, while retaining the startup connection so the empty daemon
    // cannot exit between alias resolution and typed dispatch.
    if prestarted_endpoint.is_none()
        && runtime_resolution.is_none()
        && cli.control_mode == 0
        && !cli.no_fork
        && !cli.no_start_server
        && cli.shell_command.is_none()
        && server_starting_command(&cli).is_some()
    {
        let outcome = ensure_server_running_with_config_outcome(
            &socket_path,
            startup_config.auto_start.clone(),
        )
        .map_err(ExitFailure::from_auto_start)
        .map_err(|error| error.with_socket_context(&socket_path))?;
        let endpoint = StartupEndpoint::prestarted(outcome);
        let selected_socket_path = endpoint.socket_path();
        let cold_resolution = endpoint.with_connection_mut(|connection| {
            alias_fallback::runtime_command_resolution_after_startup(
                &args,
                &selected_socket_path,
                connection,
            )
        })?;
        prestarted_endpoint = Some(endpoint);
        if let Some(alias_fallback::RuntimeCommandResolution::LegacyServerDispatch(exit_code)) =
            cold_resolution.as_ref()
        {
            return Ok(*exit_code);
        }
        if cold_resolution.is_some() {
            cli = parse_with_runtime_resolution(&args, cold_resolution.as_ref())
                .map_err(ExitFailure::from_clap)?;
            cli.utf8 |= infer_client_utf8_from_env();
            let command_was_provided = cli.command.is_some();
            validate_top_level_invocation(&cli, command_was_provided)?;
            accept_compatibility_options(&cli);
            startup_config = startup_config_from_cli(&cli);
        }
    }

    let startup_endpoint =
        prestarted_endpoint.unwrap_or_else(|| StartupEndpoint::resolved(socket_path.clone()));
    socket_path = startup_endpoint.socket_path();

    if let Some(shell_command) = cli.shell_command.as_deref() {
        return run_shell_startup(
            &socket_path,
            StartupOptions::new(
                cli.no_start_server,
                startup_config.auto_start.clone(),
                startup_endpoint,
            ),
            shell_command,
            cli.login_shell,
        )
        .map_err(|error| error.with_socket_context(&socket_path));
    }

    if cli.no_fork {
        return run_foreground_server(&socket_path, &startup_config);
    }

    let mut startup = StartupOptions::new(
        cli.no_start_server,
        startup_config.auto_start,
        startup_endpoint,
    );
    if cli.control_mode != 0 {
        return run_control_mode(&cli, &socket_path, startup)
            .map_err(|error| error.with_socket_context(&socket_path));
    }
    let client_terminal = client_terminal_context_from_cli(&cli);
    let commands = cli.into_command_queue();
    automation.reconcile(&commands);
    startup.automation_deadline = automation.deadline();
    dispatch_command_queue(commands, &socket_path, startup, client_terminal)
        .map_err(|error| error.with_socket_context(&socket_path))
}
