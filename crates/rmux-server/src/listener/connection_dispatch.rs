use super::*;

pub(super) async fn serve_connection(
    stream: LocalStream,
    requester: PeerIdentity,
    handler: Arc<RequestHandler>,
    connection_id: u64,
    mut shutdown: watch::Receiver<()>,
    shutdown_handle: ShutdownHandle,
) -> io::Result<()> {
    let Some(_) = handler.server_access_admission_for_peer(&requester) else {
        let mut conn = Connection::new(stream);
        conn.write_response(&Response::Error(ErrorResponse {
            error: rmux_proto::RmuxError::Server("access not allowed".to_owned()),
        }))
        .await?;
        return Ok(());
    };
    let mut conn = Connection::new(stream);
    let detached_connection_guard = handler.begin_detached_connection(connection_id);
    let mut sdk_wait_armed_ack_enabled = false;
    let mut session_lease_by_id_enabled = false;

    loop {
        tokio::select! {
            request = conn.read_request() => {
                let Some(incoming_request) = request? else {
                    return Ok(());
                };
                let legacy_kill_server_wire = incoming_request.legacy_kill_server_wire;
                let request = incoming_request.request;
                let Some(access_admission) =
                    handler.server_access_admission_for_peer(&requester)
                else {
                    let response = match request {
                        Request::PaneStreamCursor(request) => handler
                            .handle_revoked_pane_stream_cursor(connection_id, request),
                        request => handler
                            .handle_revoked_cleanup_request(connection_id, request)
                            .await
                            .unwrap_or_else(|| {
                                Response::Error(ErrorResponse {
                                    error: rmux_proto::RmuxError::Server(
                                        "access not allowed".to_owned(),
                                    ),
                                })
                            }),
                    };
                    conn.write_response(&response).await?;
                    continue;
                };
                let can_write = access_admission.can_write();
                let request = match apply_access_policy(request, can_write) {
                    Ok(request) => request,
                    Err(error) => {
                        conn.write_response(&Response::Error(ErrorResponse { error })).await?;
                        continue;
                    }
                };
                let attach_client_name = matches!(
                    &request,
                    Request::AttachSession(_)
                        | Request::AttachSessionExt(_)
                        | Request::AttachSessionExt2(_)
                        | Request::AttachSessionExt3(_)
                )
                .then(|| attached_client_name(requester.pid));
                let quiesce_behavior = request_quiesce_behavior(&request);
                let mut request_shutdown = shutdown.clone();
                let mut normal_request_guard: Option<NormalRequestGuard> =
                    match handler.try_begin_normal_request(
                        quiesce_behavior.requires_drain_barrier(),
                    ) {
                        Some(guard) => Some(guard),
                        None => return Ok(()),
                    };
                // The scope carries the authenticated peer as well as its
                // admission: an attach frame is rendered before the
                // registration below publishes this client, and both must
                // describe the same user (issue #182).
                let _requester_access_guard = handler
                    .begin_authenticated_peer_access(&requester, access_admission.clone());
                let mut detached_request_guard = request_counts_as_detached_activity(&request)
                    .then(|| handler.begin_detached_request());

                if request_enables_sdk_wait_armed_ack(&request) {
                    sdk_wait_armed_ack_enabled = true;
                }
                let enables_session_lease_by_id =
                    request_enables_session_lease_by_id(&request);
                let session_lease_create_addressing = if session_lease_by_id_enabled {
                    SessionLeaseCreateAddressing::StableId
                } else {
                    SessionLeaseCreateAddressing::Nominal
                };
                debug!("dispatching {}", request.command_name());
                #[cfg(feature = "web")]
                let mut undelivered_web_share;
                let mut outcome = match request {
                    Request::SdkWaitForOutput(request) => {
                        let prepared = handler
                            .prepare_sdk_wait_for_output(connection_id, request)
                            .await;
                        if write_prepared_sdk_wait(
                            &mut conn,
                            prepared,
                            &mut shutdown,
                            &handler,
                            connection_id,
                            &mut detached_request_guard,
                            sdk_wait_armed_ack_enabled,
                        )
                        .await?
                        {
                            return Ok(());
                        }
                        continue;
                    }
                    Request::SdkWaitForOutputRef(request) => {
                        let prepared = handler
                            .prepare_sdk_wait_for_output_ref(connection_id, request)
                            .await;
                        if write_prepared_sdk_wait(
                            &mut conn,
                            prepared,
                            &mut shutdown,
                            &handler,
                            connection_id,
                            &mut detached_request_guard,
                            sdk_wait_armed_ack_enabled,
                        )
                        .await?
                        {
                            return Ok(());
                        }
                        continue;
                    }
                    request => {
                        let runtime_offer = matches!(request, Request::RuntimeCommandExecution(_) | Request::RuntimeCommandExecutionV2(_));
                        let (runtime_policy, mut resolved_policy) =
                            runtime_offer.then(|| watch::channel(false)).unzip();
                        let shutdown_answer = shutdown_answer::ShutdownAnswer::for_request(&request);
                        // Build the dispatch chain outside this loop's polling
                        // frame: debug builds otherwise reserve each wrapper's
                        // construction copy on every connection poll. The inner
                        // box keeps every scope wrapper's frame pointer-sized.
                        #[cfg(feature = "web")]
                        let dispatch = crate::future::boxed(|| {
                            handler.dispatch_for_connection_with_web_share_guard(
                                requester.pid,
                                connection_id,
                                request,
                            )
                        });
                        #[cfg(not(feature = "web"))]
                        let dispatch = crate::future::boxed(|| {
                            handler.dispatch_for_connection(requester.pid, connection_id, request)
                        });
                        let dispatch = crate::future::boxed(|| {
                            shutdown_answer::scope(
                                shutdown_answer.clone(),
                                runtime_entry::scope(
                                    runtime_policy,
                                    with_session_lease_create_addressing(
                                        session_lease_create_addressing,
                                        dispatch,
                                    ),
                                ),
                            )
                        });
                        tokio::select! {
                            outcome = dispatch => {
                                #[cfg(feature = "web")]
                                {
                                    undelivered_web_share = outcome.1;
                                    outcome.0
                                }
                                #[cfg(not(feature = "web"))]
                                outcome
                            },
                            result = async {
                                let result =
                                    wait_for_request_shutdown(&mut request_shutdown, quiesce_behavior)
                                        .await;
                                // A request whose popup wait ends itself on shutdown answers with
                                // what it acquired; let it finish instead of cancelling it.
                                if shutdown_answer
                                    .as_ref()
                                    .is_some_and(shutdown_answer::ShutdownAnswer::finish_on_shutdown)
                                {
                                    std::future::pending::<()>().await;
                                }
                                result
                            },
                                if quiesce_behavior.cancels_on_shutdown() => {
                                if result.is_ok() {
                                    debug!("closing client connection during shutdown");
                                }
                                return Ok(());
                            }
                            result = wait_for_peer_close(&conn.stream),
                                if quiesce_behavior.cancels_on_peer_disconnect() => {
                                result?;
                                debug!("closing client connection after peer disconnect");
                                return Ok(());
                            }
                            result = runtime_entry::wait_for_peer_close(&conn.stream, resolved_policy.as_mut()),
                                if runtime_offer => {
                                result?;
                                debug!("closing resolved cancel-safe offer after peer disconnect");
                                return Ok(());
                            }
                        }
                    }
                };
                if enables_session_lease_by_id
                    && matches!(&outcome.response, Response::Handshake(_))
                {
                    session_lease_by_id_enabled = true;
                }
                // An attach registration is removed as soon as its last
                // session closes, but its forwarder still has to deliver the
                // terminal exit frame. Bridge that transition before any
                // further await can let exit-empty shut the daemon down.
                let attach_forwarder_guard = outcome
                    .attach
                    .is_some()
                    .then(|| handler.begin_attach_forwarder());
                let pending_control = if let Some(control_upgrade) = outcome.control.take() {
                    let initial_command_count = control_upgrade.initial_command_count as usize;
                    if let Err(error) =
                        control::validate_initial_control_command_count(initial_command_count)
                    {
                        conn.write_response(&Response::Error(ErrorResponse { error }))
                            .await?;
                        drop(detached_request_guard.take());
                        continue;
                    }
                    let control_mode = control_upgrade.mode;
                    let (server_event_tx, server_event_rx) =
                        tokio::sync::mpsc::channel::<ControlServerEvent>(
                            control::CONTROL_SERVER_EVENT_CAPACITY,
                        );
                    let closing = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let control_id = match crate::future::boxed(|| {
                        handler.register_control_with_server_access(
                            requester.pid,
                            control_upgrade,
                            ControlRegistration {
                                event_tx: server_event_tx,
                                closing: closing.clone(),
                                uid: requester.uid,
                                user: requester.user.clone(),
                                can_write,
                            },
                            access_admission.clone(),
                        )
                    })
                    .await
                    {
                        Ok(control_id) => control_id,
                        Err(error) => {
                            conn.write_response(&Response::Error(ErrorResponse {
                                error: error.into_rmux_error(),
                            }))
                            .await?;
                            drop(detached_request_guard.take());
                            continue;
                        }
                    };
                    Some((
                        initial_command_count,
                        control_mode,
                        server_event_rx,
                        closing,
                        control_id,
                    ))
                } else {
                    None
                };

                // Tests park the loop here to make the captured admission stale
                // between a dispatched pane-stream open and its recheck.
                #[cfg(test)]
                inflight_access_tests::pause_before_inflight_pane_stream_recheck(
                    &handler,
                    &outcome.response,
                )
                .await;

                if matches!(
                    outcome.response,
                    Response::SubscribePaneStream(_) | Response::PaneStreamCursor(_)
                ) && !handler
                    .server_access_admission_is_current(&requester, &access_admission)
                {
                    outcome.response = handler.revoke_inflight_pane_stream_response(
                        connection_id,
                        outcome.response,
                    );
                }

                let response_result = match (legacy_kill_server_wire, &outcome.response) {
                    (Some(wire_version), Response::KillServer(_)) => {
                        conn.write_legacy_kill_server_response(wire_version).await
                    }
                    _ => conn.write_response(&outcome.response).await,
                };
                if let Err(error) = response_result {
                    if let Some((_, _, _, _, control_id)) = pending_control.as_ref() {
                        crate::future::boxed(|| handler.finish_control(requester.pid, *control_id))
                            .await;
                        let _ = handler.request_shutdown_if_server_empty().await;
                    }
                    drop(detached_request_guard.take());
                    #[cfg(windows)]
                    let _ = handler
                        .request_shutdown_if_pending_excluding_detached_connection(Some(connection_id));
                    return Err(error);
                }

                #[cfg(feature = "web")]
                if let Some(guard) = undelivered_web_share.as_mut() {
                    guard.disarm();
                }

                if let Some(attach) = outcome.attach {
                    let attach_forwarder_guard = attach_forwarder_guard
                        .expect("attach outcome must hold an attach forwarder guard");
                    let Response::AttachSession(response) = &outcome.response else {
                        return Err(io::Error::other(
                            "attach upgrade requires an attach-session response",
                        ));
                    };
                    let session_name = response.session_name.clone();
                    let terminal_context = attach.target.outer_terminal.context().clone();
                    let client_title = attach_frame_client_title(&attach.target);
                    let client_name = attach_client_name
                        .expect("attach upgrade captures its client name before dispatch");
                    let attach_identity = handler
                        .register_reserved_attach_identity_with_server_access_and_client_name(
                            requester.pid,
                            client_name.clone(),
                            AttachRegistrationTarget {
                                session_name: session_name.clone(),
                                expected_session_id: Some(attach.session_id),
                            },
                            attach.attach_id,
                            AttachRegistration {
                                control_tx: attach.control_tx,
                                control_backlog: attach.control_backlog.clone(),
                                closing: attach.closing.clone(),
                                persistent_overlay_epoch: attach.persistent_overlay_epoch.clone(),
                                terminal_context,
                                client_title,
                                flags: attach.flags,
                                render_stream: attach.render_stream,
                                uid: requester.uid,
                                user: requester.user.clone(),
                                can_write,
                                client_size: attach.client_size,
                            },
                            access_admission.clone(),
                        )
                        .await
                        .ok_or_else(|| {
                            io::Error::other("attach session changed before registration")
                        })?;
                    drop(detached_connection_guard);
                    drop(detached_request_guard.take());
                    handler
                        .emit_client_session_changed(
                            client_name.clone(),
                            session_name.clone(),
                            attach.session_id,
                        )
                        .await;
                    if let Some(publication) = attach.resize_publication {
                        publication.publish().await;
                    }
                    handler
                        .emit_client_attached_identity(
                            client_name,
                            session_name,
                            attach.session_id,
                        )
                        .await;
                    drop(normal_request_guard.take());
                    let (stream, buffered_bytes) = conn.into_raw_parts();
                    if !buffered_bytes.is_empty() {
                        warn!(
                            buffered = buffered_bytes.len(),
                            "preserving buffered bytes at attach upgrade boundary"
                        );
                    }
                    // The upgraded transports are cold, long-lived futures:
                    // keep them on the heap, not in every request poll frame.
                    let result = crate::future::boxed(|| pane_io::forward_attach_with_opt_ins(
                        stream,
                        attach.target,
                        buffered_bytes,
                        shutdown,
                        attach.control_rx,
                        attach.control_backlog,
                        attach.closing,
                        attach.persistent_overlay_epoch,
                        pane_io::LiveAttachInputContext::new(
                            Arc::clone(&handler),
                            attach_identity,
                        ),
                        pane_io::AttachStreamOptIns {
                            render: attach.render_stream,
                            server_exited: attach.server_exit_notice,
                        },
                    ))
                    .await;
                    crate::future::boxed(|| {
                        handler.finish_attach(requester.pid, attach_identity.attach_id())
                    })
                    .await;
                    drop(attach_forwarder_guard);
                    let _ = handler.request_shutdown_if_pending();
                    return result;
                }
                if let Some((
                    initial_command_count,
                    control_mode,
                    server_event_rx,
                    closing,
                    control_id,
                )) = pending_control
                {
                    drop(detached_connection_guard);
                    drop(detached_request_guard.take());
                    drop(normal_request_guard.take());
                    let (stream, buffered_bytes) = conn.into_raw_parts();
                    let result = crate::future::boxed(|| control::forward_control(
                        stream,
                        Arc::clone(&handler),
                        ControlClientIdentity::new(requester.pid, control_id),
                        ControlUpgradeInput::with_mode(
                            buffered_bytes,
                            initial_command_count,
                            control_mode,
                        ),
                        shutdown,
                        server_event_rx,
                        ControlLifecycle {
                            closing,
                            shutdown_handle: shutdown_handle.clone(),
                        },
                    ))
                    .await;
                    crate::future::boxed(|| handler.finish_control(requester.pid, control_id)).await;
                    let _ = handler.request_shutdown_if_server_empty().await;
                    return result;
                }

                drop(detached_request_guard.take());
                if handler
                    .request_shutdown_if_pending_excluding_detached_connection(Some(connection_id))
                {
                    return Ok(());
                }
            }
            result = shutdown.changed() => {
                if result.is_ok() {
                    debug!("closing client connection during shutdown");
                }
                return Ok(());
            }
        }
    }
}

async fn write_prepared_sdk_wait(
    conn: &mut Connection,
    prepared: PreparedSdkWait,
    shutdown: &mut watch::Receiver<()>,
    handler: &Arc<RequestHandler>,
    connection_id: u64,
    detached_request_guard: &mut Option<DetachedRequestGuard>,
    send_armed_ack: bool,
) -> io::Result<bool> {
    let response = match prepared {
        PreparedSdkWait::Immediate(response) => response,
        PreparedSdkWait::Armed(wait) => {
            if send_armed_ack {
                conn.write_response(&wait.armed_response()).await?;
            }
            tokio::select! {
                response = wait.wait() => response,
                result = shutdown.changed() => {
                    if result.is_ok() {
                        debug!("closing client connection during shutdown");
                    }
                    return Ok(true);
                }
                result = wait_for_peer_close(&conn.stream) => {
                    result?;
                    debug!("closing client connection after peer disconnect");
                    return Ok(true);
                }
            }
        }
    };

    if let Err(error) = conn.write_response(&response).await {
        drop(detached_request_guard.take());
        #[cfg(windows)]
        let _ =
            handler.request_shutdown_if_pending_excluding_detached_connection(Some(connection_id));
        return Err(error);
    }

    drop(detached_request_guard.take());
    Ok(handler.request_shutdown_if_pending_excluding_detached_connection(Some(connection_id)))
}
