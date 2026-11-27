use super::*;

pub(super) fn run_attach_session(
    original_args: &[OsString],
    socket_path: &Path,
    mut request: AttachSessionExt2Request,
) -> Result<i32, String> {
    let connection_outcome = match connect_with_validated_startup_outcome(socket_path) {
        Ok(outcome) => outcome,
        Err(_) => return exec_full_helper(original_args),
    };
    let started_by_caller =
        connection_outcome.provenance() == ServerConnectionProvenance::StartedByCaller;
    let mut connection = connection_outcome.into_connection();
    if !server_has_sessions(&mut connection)? {
        if started_by_caller {
            let _ = connection.shutdown_if_idle();
            drop(connection);
            let _ = wait_for_killed_server_socket_cleanup(socket_path);
        }
        return Err("no sessions".to_owned());
    }
    if let Some(target_spec) = request.target_spec.as_ref() {
        let response = connection
            .resolve_target(
                Some(target_spec.clone()),
                ResolveTargetType::Session,
                false,
                false,
            )
            .map_err(|error| error.to_string())?;
        request.target = Some(attach_resolved_session(response)?);
    }
    require_attach_terminal().map_err(str::to_owned)?;

    let attach_resize_geometry = connection
        .supports_capability(CAPABILITY_ATTACH_RESIZE_GEOMETRY)
        .map_err(|error| error.to_string())?;
    let attach_render = connection
        .supports_capability(CAPABILITY_ATTACH_RENDER)
        .map_err(|error| error.to_string())?;
    #[cfg(windows)]
    let attach_windows_console_key = connection
        .supports_capability(CAPABILITY_ATTACH_WINDOWS_CONSOLE_KEY)
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    let attach_server_exited = connection
        .supports_capability(CAPABILITY_ATTACH_SERVER_EXITED)
        .map_err(|error| error.to_string())?;
    let mut attach_capabilities = Vec::new();
    if attach_render {
        attach_capabilities.push(CAPABILITY_ATTACH_RENDER.to_owned());
    }
    #[cfg(windows)]
    if attach_windows_console_key {
        attach_capabilities.push(CAPABILITY_ATTACH_WINDOWS_CONSOLE_KEY.to_owned());
    }
    #[cfg(unix)]
    if attach_server_exited {
        attach_capabilities.push(CAPABILITY_ATTACH_SERVER_EXITED.to_owned());
    }
    let transition = if !attach_capabilities.is_empty() {
        connection.begin_attach_with_capabilities(AttachSessionExt3Request::from_ext2(
            request,
            attach_capabilities,
        ))
    } else {
        connection.begin_attach_with_target_spec(request)
    }
    .map_err(|error| error.to_string())?;

    match transition {
        AttachTransition::Upgraded(upgrade) => {
            let (stream, initial_bytes) = upgrade.into_parts();
            #[cfg(unix)]
            let attached = if attach_resize_geometry {
                attach_terminal_with_initial_bytes_and_resize_geometry(stream, initial_bytes)
            } else {
                attach_terminal_with_initial_bytes(stream, initial_bytes)
            };
            #[cfg(windows)]
            let attached = {
                let _ = attach_resize_geometry;
                attach_terminal_with_initial_bytes_and_windows_console_key(
                    stream,
                    initial_bytes,
                    attach_windows_console_key,
                )
            };
            #[cfg(all(not(unix), not(windows)))]
            let attached = {
                let _ = attach_resize_geometry;
                attach_terminal_with_initial_bytes(stream, initial_bytes)
            };
            match attached {
                Ok(()) => Ok(0),
                // tmux exits 1 after `[server exited]`; the banner is the whole report.
                Err(ClientError::ServerExited) => Ok(1),
                Err(error) => Err(error.to_string()),
            }
        }
        AttachTransition::Rejected(response) if response_is_decode_error(&response) => {
            exec_full_helper(original_args)
        }
        AttachTransition::Rejected(response) => {
            write_response_output_or_error(response, "attach-session")
        }
    }
}

pub(super) fn attach_resolved_session(
    response: Response,
) -> Result<rmux_proto::SessionName, String> {
    match response {
        Response::ResolveTarget(response) => match response.target {
            Target::Session(target) => Ok(target),
            _ => Err(
                "protocol error: resolve-target returned a non-session target for attach-session"
                    .to_owned(),
            ),
        },
        Response::Error(error) => Err(tmux_cli_error_message("attach-session", &error.error)),
        _ => Err(
            "protocol error: unexpected response while resolving attach-session target".to_owned(),
        ),
    }
}
