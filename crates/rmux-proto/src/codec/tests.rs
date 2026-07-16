use super::{decode_frame, encode_frame, FrameDecoder};
use crate::{
    AttachSessionExt2Request, AttachSessionExt3Request, AttachSessionRequest, BreakPaneRequest,
    CapturePaneRequest, ClockModeRequest, ControlMode, ControlModeRequest, ControlModeResponse,
    CopyModeRequest, DeleteBufferRequest, DetachClientExtRequest, DetachClientRequest,
    DisplayMessageExtRequest, DisplayMessageRequest, DisplayPanesRequest, HasSessionRequest,
    HookLifecycle, HookName, JoinPaneRequest, KillPaneRequest, KillSessionRequest,
    KillWindowRequest, LastPaneRequest, LastWindowRequest, LayoutName, ListBuffersRequest,
    ListClientsRequest, ListPanesRequest, ListSessionsRequest, ListWindowsRequest,
    LoadBufferRequest, MoveWindowRequest, MoveWindowTarget, NewSessionRequest, NewWindowRequest,
    NextLayoutRequest, NextWindowRequest, OptionName, PaneTarget, PasteBufferRequest,
    PreviousLayoutRequest, PreviousWindowRequest, RefreshClientRequest, RenameSessionRequest,
    RenameWindowRequest, Request, ResizePaneAdjustment, ResizePaneRequest, RotateWindowDirection,
    RotateWindowRequest, SaveBufferRequest, ScopeSelector, SelectLayoutRequest, SelectLayoutTarget,
    SelectPaneAdjacentRequest, SelectPaneDirection, SelectPaneRequest, SelectWindowRequest,
    SendKeysExt2Request, SendKeysRequest, SessionName, SetBufferRequest, SetEnvironmentRequest,
    SetHookRequest, SetOptionMode, SetOptionRequest, ShowBufferRequest, ShowMessagesRequest,
    SourceFileRequest, SplitDirection, SplitWindowExtRequest, SplitWindowRequest,
    SplitWindowTarget, SuspendClientRequest, SwapPaneDirection, SwapPaneRequest, SwapWindowRequest,
    SwitchClientExt3Request, SwitchClientRequest, Target, TerminalSize, WindowTarget,
};

#[test]
fn every_request_variant_round_trips_through_the_frame_codec() {
    let alpha = SessionName::new("alpha").expect("valid session");
    let beta = SessionName::new("beta").expect("valid session");
    let pane = PaneTarget::new(alpha.clone(), 2);
    let window = WindowTarget::new(alpha.clone());

    let requests = vec![
        Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize {
                cols: 200,
                rows: 50,
            }),
            environment: None,
        }),
        Request::HasSession(HasSessionRequest {
            target: alpha.clone(),
        }),
        Request::KillSession(KillSessionRequest {
            target: alpha.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }),
        Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
            name: Some("build".to_owned()),
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })),
        Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 1),
            kill_all_others: false,
        }),
        Request::SelectWindow(SelectWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 2),
        }),
        Request::RenameWindow(RenameWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 2),
            name: "logs".to_owned(),
        }),
        Request::NextWindow(NextWindowRequest {
            target: alpha.clone(),
            alerts_only: false,
        }),
        Request::PreviousWindow(PreviousWindowRequest {
            target: alpha.clone(),
            alerts_only: false,
        }),
        Request::LastWindow(LastWindowRequest {
            target: alpha.clone(),
        }),
        Request::ListWindows(Box::new(ListWindowsRequest {
            target: alpha.clone(),
            format: Some("#{window_index}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        })),
        Request::MoveWindow(MoveWindowRequest {
            source: Some(WindowTarget::with_window(alpha.clone(), 1)),
            target: MoveWindowTarget::Window(WindowTarget::with_window(beta.clone(), 3)),
            renumber: false,
            kill_destination: true,
            detached: false,
            after: false,
            before: false,
        }),
        Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(alpha.clone(), 1),
            target: WindowTarget::with_window(beta.clone(), 3),
            detached: true,
        }),
        Request::RotateWindow(RotateWindowRequest {
            target: WindowTarget::with_window(alpha.clone(), 1),
            direction: RotateWindowDirection::Down,
            restore_zoom: false,
        }),
        Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(pane.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }),
        Request::SwapPane(SwapPaneRequest {
            source: pane.clone(),
            target: PaneTarget::with_window(beta.clone(), 2, 3),
            direction: Some(SwapPaneDirection::Down),
            detached: true,
            preserve_zoom: false,
        }),
        Request::LastPane(LastPaneRequest {
            target: WindowTarget::new(beta.clone()),
            preserve_zoom: false,
            input_disabled: None,
        }),
        Request::JoinPane(JoinPaneRequest {
            source: PaneTarget::with_window(beta.clone(), 1, 2),
            target: PaneTarget::with_window(beta.clone(), 3, 4),
            direction: SplitDirection::Horizontal,
            detached: false,
            before: false,
            full_size: false,
            size: None,
        }),
        Request::BreakPane(Box::new(BreakPaneRequest {
            source: PaneTarget::with_window(beta.clone(), 1, 2),
            target: Some(WindowTarget::with_window(beta.clone(), 5)),
            name: Some("scratch".to_owned()),
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })),
        Request::KillPane(KillPaneRequest {
            target: pane.clone(),
            kill_all_except: false,
        }),
        Request::SelectLayout(SelectLayoutRequest {
            target: SelectLayoutTarget::Window(window.clone()),
            layout: LayoutName::MainVertical,
        }),
        Request::NextLayout(NextLayoutRequest {
            target: window.clone(),
        }),
        Request::PreviousLayout(PreviousLayoutRequest { target: window }),
        Request::ResizePane(ResizePaneRequest {
            target: pane.clone(),
            adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 34 },
        }),
        Request::DisplayPanes(Box::new(DisplayPanesRequest {
            target: beta.clone(),
            duration_ms: None,
            non_blocking: false,
            no_command: false,
            template: None,
            target_client: None,
        })),
        Request::SelectPane(Box::new(SelectPaneRequest {
            target: pane.clone(),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })),
        Request::SelectPaneAdjacent(SelectPaneAdjacentRequest {
            target: pane.clone(),
            direction: SelectPaneDirection::Right,
            preserve_zoom: false,
        }),
        Request::SendKeys(SendKeysRequest {
            target: pane,
            keys: vec!["echo".to_owned(), "Enter".to_owned()],
        }),
        Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: SplitWindowTarget::Pane(PaneTarget::with_window(beta.clone(), 0, 1)),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: Some(vec!["FOO=bar".to_owned()]),
            command: Some(vec!["printf done".to_owned()]),
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: None,
            detached: false,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: None,
        })),
        Request::AttachSession(AttachSessionRequest {
            target: alpha.clone(),
        }),
        Request::SwitchClient(SwitchClientRequest { target: alpha }),
        Request::DetachClient(DetachClientRequest),
        Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::DefaultTerminal,
            value: "tmux-256color".to_owned(),
            mode: SetOptionMode::Replace,
        }),
        Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::TerminalFeatures,
            value: "xterm:RGB".to_owned(),
            mode: SetOptionMode::Append,
        }),
        Request::SetEnvironment(Box::new(SetEnvironmentRequest {
            scope: ScopeSelector::Session(beta.clone()),
            name: "TERM".to_owned(),
            value: "xterm-256color".to_owned(),
            mode: None,
            hidden: false,
            format: false,
        })),
        Request::SetHook(SetHookRequest {
            scope: ScopeSelector::Session(beta.clone()),
            hook: HookName::ClientAttached,
            command: "printf ready".to_owned(),
            lifecycle: HookLifecycle::OneShot,
        }),
        Request::SetHookMutation(crate::SetHookMutationRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(
                SessionName::new("alpha").unwrap(),
                2,
            )),
            hook: HookName::WindowLayoutChanged,
            command: Some("display-message updated".to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: true,
            unset: false,
            run_immediately: false,
            index: Some(3),
        }),
        Request::SetBuffer(Box::new(SetBufferRequest {
            name: Some("named".to_owned()),
            content: b"buffer".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
            target_client: None,
        })),
        Request::ShowBuffer(ShowBufferRequest {
            name: Some("named".to_owned()),
        }),
        Request::PasteBuffer(Box::new(PasteBufferRequest {
            name: None,
            target: PaneTarget::with_window(SessionName::new("alpha").unwrap(), 0, 0),
            delete_after: true,
            separator: None,
            linefeed: false,
            raw: false,
            bracketed: false,
        })),
        Request::ListBuffers(ListBuffersRequest::default()),
        Request::DeleteBuffer(DeleteBufferRequest { name: None }),
        Request::LoadBuffer(Box::new(LoadBufferRequest {
            path: "/tmp/input".to_owned(),
            cwd: None,
            name: Some("loaded".to_owned()),
            set_clipboard: false,
            target_client: None,
        })),
        Request::SaveBuffer(SaveBufferRequest {
            path: "/tmp/output".to_owned(),
            cwd: None,
            name: Some("loaded".to_owned()),
            append: false,
        }),
        Request::CapturePane(Box::new(CapturePaneRequest {
            target: PaneTarget::with_window(SessionName::new("alpha").unwrap(), 0, 0),
            start: Some(-5),
            end: Some(-1),
            print: true,
            buffer_name: None,
            alternate: false,
            escape_ansi: false,
            escape_sequences: false,
            include_format: false,
            hyperlinks: false,
            line_numbers: false,
            join_wrapped: false,
            use_mode_screen: false,
            preserve_trailing_spaces: false,
            do_not_trim_spaces: false,
            pending_input: false,
            quiet: false,
            start_is_absolute: false,
            end_is_absolute: false,
        })),
        Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Window(WindowTarget::with_window(
                SessionName::new("alpha").unwrap(),
                0,
            ))),
            print: true,
            message: Some("#{session_name}".to_owned()),
            empty_target_context: false,
        }),
        Request::DisplayMessageExt(Box::new(DisplayMessageExtRequest {
            target: Some(Target::Session(beta.clone())),
            print: true,
            message: Some("#{client_name}".to_owned()),
            target_client: Some("=".to_owned()),
            empty_target_context: false,
        })),
        Request::SendKeysExt2(Box::new(SendKeysExt2Request {
            target: Some(PaneTarget::with_window(beta.clone(), 0, 1)),
            keys: vec!["A".to_owned()],
            expand_formats: false,
            hex: false,
            literal: true,
            dispatch_key_table: false,
            copy_mode_command: false,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
            target_client: Some("=".to_owned()),
        })),
        Request::RenameSession(RenameSessionRequest {
            target: SessionName::new("alpha").unwrap(),
            new_name: SessionName::new("gamma").unwrap(),
        }),
        Request::ListSessions(ListSessionsRequest {
            format: Some("#{session_name}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        }),
        Request::ListPanes(Box::new(ListPanesRequest {
            target: SessionName::new("alpha").unwrap(),
            format: Some("#{pane_id}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
            target_window_index: None,
        })),
        Request::SourceFile(Box::new(SourceFileRequest {
            paths: vec!["/tmp/rmux.conf".to_owned()],
            quiet: false,
            parse_only: false,
            verbose: true,
            expand_paths: false,
            target: None,
            caller_cwd: None,
            stdin: None,
        })),
        Request::ShowHooks(crate::ShowHooksRequest {
            scope: ScopeSelector::Global,
            window: true,
            pane: false,
            hook: Some(HookName::PaneExited),
        }),
        Request::CopyMode(CopyModeRequest {
            target: Some(PaneTarget::with_window(
                SessionName::new("alpha").unwrap(),
                0,
                0,
            )),
            page_down: false,
            exit_on_scroll: true,
            hide_position: true,
            mouse_drag_start: false,
            cancel_mode: false,
            scrollbar_scroll: false,
            source: Some(PaneTarget::with_window(
                SessionName::new("beta").unwrap(),
                1,
                2,
            )),
            page_up: true,
        }),
        Request::ControlMode(ControlModeRequest {
            mode: ControlMode::ControlControl,
            client_terminal: crate::ClientTerminalContext::default(),
            initial_command_count: 2,
        }),
        Request::ClockMode(ClockModeRequest {
            target: Some(PaneTarget::with_window(
                SessionName::new("alpha").unwrap(),
                0,
                0,
            )),
        }),
        Request::ShowMessages(ShowMessagesRequest {
            jobs: true,
            terminals: true,
            target_client: Some("1234".to_owned()),
        }),
        Request::KillServer(crate::KillServerRequest),
        Request::LockServer(crate::LockServerRequest),
        Request::LockSession(crate::LockSessionRequest {
            target: SessionName::new("alpha").unwrap(),
        }),
        Request::LockClient(crate::LockClientRequest {
            target_client: "=".to_owned(),
        }),
        Request::ServerAccess(crate::ServerAccessRequest {
            add: false,
            deny: false,
            list: true,
            read_only: false,
            write: false,
            target: None,
            user: None,
        }),
        Request::RefreshClient(Box::new(RefreshClientRequest {
            target_client: Some("=".to_owned()),
            adjustment: Some(2),
            clear_pan: false,
            pan_left: false,
            pan_right: true,
            pan_up: false,
            pan_down: false,
            status_only: false,
            clipboard_query: false,
            flags: Some("read-only".to_owned()),
            flags_alias: Some("!ignore-size".to_owned()),
            subscriptions: vec!["%0:on".to_owned()],
            subscriptions_format: vec!["name:%0:#{pane_id}".to_owned()],
            control_size: Some("80x24".to_owned()),
            colour_report: Some("%0:rgb".to_owned()),
        })),
        Request::ListClients(Box::new(ListClientsRequest {
            format: Some("#{client_name}".to_owned()),
            filter: Some("#{client_control_mode}".to_owned()),
            sort_order: Some("name".to_owned()),
            reversed: true,
            target_session: Some(beta.clone()),
        })),
        Request::SuspendClient(SuspendClientRequest {
            target_client: Some("=".to_owned()),
        }),
        Request::DetachClientExt(DetachClientExtRequest {
            target_client: Some("=".to_owned()),
            all_other_clients: true,
            target_session: Some(beta.clone()),
            kill_on_detach: true,
            exec_command: Some("printf detached".to_owned()),
        }),
        Request::AttachSessionExt2(Box::new(AttachSessionExt2Request {
            target: Some(beta.clone()),
            target_spec: Some("beta:0.1".to_owned()),
            detach_other_clients: true,
            kill_other_clients: false,
            read_only: true,
            skip_environment_update: true,
            flags: Some(vec!["active-pane".to_owned()]),
            working_directory: Some("/tmp".to_owned()),
            client_terminal: crate::ClientTerminalContext::default(),
            client_size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),
        })),
        Request::AttachSessionExt3(Box::new(AttachSessionExt3Request::from_ext2(
            AttachSessionExt2Request {
                target: Some(beta.clone()),
                target_spec: Some("beta:0.1".to_owned()),
                detach_other_clients: true,
                kill_other_clients: false,
                read_only: true,
                skip_environment_update: true,
                flags: Some(vec!["active-pane".to_owned()]),
                working_directory: Some("/tmp".to_owned()),
                client_terminal: crate::ClientTerminalContext::default(),
                client_size: Some(TerminalSize {
                    cols: 120,
                    rows: 40,
                }),
            },
            vec![crate::CAPABILITY_ATTACH_RENDER.to_owned()],
        ))),
        Request::SwitchClientExt3(Box::new(SwitchClientExt3Request {
            target_client: Some("=".to_owned()),
            target: Some("beta:0.1".to_owned()),
            key_table: Some("prefix".to_owned()),
            last_session: false,
            next_session: false,
            previous_session: false,
            toggle_read_only: true,
            sort_order: Some("size".to_owned()),
            skip_environment_update: true,
            zoom: true,
        })),
        Request::Handshake(crate::HandshakeRequest::requiring([
            crate::CAPABILITY_DETACHED_RPC,
            crate::CAPABILITY_HANDSHAKE,
        ])),
    ];

    for request in requests {
        let frame = encode_frame(&request).expect("encodes");
        let decoded: Request = decode_frame(&frame).expect("decodes");
        assert_eq!(decoded, request);
    }
}

#[test]
fn display_message_request_appends_after_existing_request_variants() {
    let request = Request::DisplayMessage(DisplayMessageRequest {
        target: Some(Target::Session(
            SessionName::new("alpha").expect("valid session"),
        )),
        print: true,
        message: Some("#{session_name}".to_owned()),
        empty_target_context: false,
    });

    let encoded = bincode::serialize(&request).expect("request encodes");

    assert_eq!(&encoded[..4], 44_u32.to_le_bytes().as_slice());
    assert_eq!(
        bincode::deserialize::<Request>(&encoded).expect("request decodes"),
        request
    );
}

#[test]
fn show_hooks_request_appends_after_existing_request_variants() {
    let request = Request::ShowHooks(crate::ShowHooksRequest {
        scope: ScopeSelector::Global,
        window: false,
        pane: false,
        hook: Some(HookName::ClientAttached),
    });

    let encoded = bincode::serialize(&request).expect("request encodes");

    assert_eq!(&encoded[..4], 54_u32.to_le_bytes().as_slice());
    assert_eq!(
        bincode::deserialize::<Request>(&encoded).expect("request decodes"),
        request
    );
}

#[test]
fn copy_mode_request_appends_after_existing_request_variants() {
    let request = Request::CopyMode(CopyModeRequest {
        target: Some(PaneTarget::with_window(
            SessionName::new("alpha").expect("valid session"),
            0,
            0,
        )),
        page_down: false,
        exit_on_scroll: false,
        hide_position: false,
        mouse_drag_start: false,
        cancel_mode: false,
        scrollbar_scroll: false,
        source: None,
        page_up: true,
    });

    let encoded = bincode::serialize(&request).expect("request encodes");

    assert_eq!(&encoded[..4], 62_u32.to_le_bytes().as_slice());
    assert_eq!(
        bincode::deserialize::<Request>(&encoded).expect("request decodes"),
        request
    );
}

#[test]
fn server_lifecycle_request_variants_append_after_spread_layout() {
    let cases = [
        (72_u32, Request::KillServer(crate::KillServerRequest)),
        (73_u32, Request::LockServer(crate::LockServerRequest)),
        (
            74_u32,
            Request::LockSession(crate::LockSessionRequest {
                target: SessionName::new("alpha").expect("valid session"),
            }),
        ),
        (
            75_u32,
            Request::LockClient(crate::LockClientRequest {
                target_client: "=".to_owned(),
            }),
        ),
        (
            76_u32,
            Request::ServerAccess(crate::ServerAccessRequest {
                add: false,
                deny: false,
                list: true,
                read_only: false,
                write: false,
                target: None,
                user: None,
            }),
        ),
    ];

    for (expected_tag, request) in cases {
        let encoded = bincode::serialize(&request).expect("request encodes");
        assert_eq!(&encoded[..4], expected_tag.to_le_bytes().as_slice());
        assert_eq!(
            bincode::deserialize::<Request>(&encoded).expect("request decodes"),
            request
        );
    }
}

#[test]
fn client_surface_request_variants_append_after_server_access() {
    let cases = [
        (
            77_u32,
            Request::RefreshClient(Box::new(RefreshClientRequest {
                target_client: Some("=".to_owned()),
                adjustment: None,
                clear_pan: false,
                pan_left: false,
                pan_right: false,
                pan_up: false,
                pan_down: false,
                status_only: true,
                clipboard_query: false,
                flags: None,
                flags_alias: None,
                subscriptions: Vec::new(),
                subscriptions_format: Vec::new(),
                control_size: None,
                colour_report: None,
            })),
        ),
        (
            78_u32,
            Request::ListClients(Box::new(ListClientsRequest {
                format: None,
                filter: None,
                sort_order: None,
                reversed: false,
                target_session: Some(SessionName::new("alpha").expect("valid session")),
            })),
        ),
        (
            79_u32,
            Request::SuspendClient(SuspendClientRequest {
                target_client: Some("=".to_owned()),
            }),
        ),
        (
            80_u32,
            Request::DetachClientExt(DetachClientExtRequest {
                target_client: Some("=".to_owned()),
                all_other_clients: false,
                target_session: None,
                kill_on_detach: true,
                exec_command: None,
            }),
        ),
        (
            81_u32,
            Request::AttachSessionExt2(Box::new(AttachSessionExt2Request {
                target: Some(SessionName::new("alpha").expect("valid session")),
                target_spec: Some("alpha:2".to_owned()),
                detach_other_clients: false,
                kill_other_clients: false,
                read_only: false,
                skip_environment_update: false,
                flags: None,
                working_directory: Some("/tmp".to_owned()),
                client_terminal: crate::ClientTerminalContext::default(),
                client_size: Some(TerminalSize { cols: 90, rows: 30 }),
            })),
        ),
        (
            82_u32,
            Request::SwitchClientExt3(Box::new(SwitchClientExt3Request {
                target_client: Some("=".to_owned()),
                target: Some("alpha".to_owned()),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                sort_order: None,
                skip_environment_update: false,
                zoom: true,
            })),
        ),
        (
            117_u32,
            Request::AttachSessionExt3(Box::new(AttachSessionExt3Request::from_ext2(
                AttachSessionExt2Request {
                    target: Some(SessionName::new("alpha").expect("valid session")),
                    target_spec: Some("alpha:2".to_owned()),
                    detach_other_clients: false,
                    kill_other_clients: false,
                    read_only: false,
                    skip_environment_update: false,
                    flags: None,
                    working_directory: Some("/tmp".to_owned()),
                    client_terminal: crate::ClientTerminalContext::default(),
                    client_size: Some(TerminalSize { cols: 90, rows: 30 }),
                },
                vec![crate::CAPABILITY_ATTACH_RENDER.to_owned()],
            ))),
        ),
    ];

    for (expected_tag, request) in cases {
        let encoded = bincode::serialize(&request).expect("request encodes");
        assert_eq!(&encoded[..4], expected_tag.to_le_bytes().as_slice());
        assert_eq!(
            bincode::deserialize::<Request>(&encoded).expect("request decodes"),
            request
        );
    }
}

#[test]
fn clock_mode_request_appends_after_control_mode_request() {
    let request = Request::ClockMode(ClockModeRequest {
        target: Some(PaneTarget::with_window(
            SessionName::new("alpha").expect("valid session"),
            0,
            0,
        )),
    });

    let encoded = bincode::serialize(&request).expect("request encodes");

    assert_eq!(&encoded[..4], 64_u32.to_le_bytes().as_slice());
    assert_eq!(
        bincode::deserialize::<Request>(&encoded).expect("request decodes"),
        request
    );
}

#[test]
fn decoder_handles_partial_reads() {
    let request = Request::HasSession(HasSessionRequest {
        target: SessionName::new("alpha").expect("valid session"),
    });
    let frame = encode_frame(&request).expect("encodes");
    let split_at = frame.len() / 2;

    let mut decoder = FrameDecoder::new();
    decoder.push_bytes(&frame[..split_at]);
    assert_eq!(decoder.next_frame::<Request>().expect("not an error"), None);

    decoder.push_bytes(&frame[split_at..]);
    assert_eq!(
        decoder.next_frame::<Request>().expect("complete frame"),
        Some(request)
    );
}

#[test]
fn decoder_rejects_zero_length_payloads() {
    let mut decoder = FrameDecoder::new();
    decoder.push_bytes(&test_frame_header(0));
    assert_eq!(
        decoder.next_frame::<Request>(),
        Err(crate::RmuxError::EmptyFrame)
    );
}

#[test]
fn decoder_rejects_oversized_frames() {
    let mut decoder = FrameDecoder::with_max_frame_length(8);
    decoder.push_bytes(&test_frame_header(16));

    assert_eq!(
        decoder.next_frame::<Request>(),
        Err(crate::RmuxError::FrameTooLarge {
            length: 16,
            maximum: 8,
        })
    );
}

#[test]
fn decoder_clears_buffer_after_unsupported_wire_version() {
    let unsupported_version = if crate::RMUX_WIRE_VERSION == 1 { 2 } else { 1 };
    let mut frame = vec![crate::RMUX_FRAME_MAGIC, unsupported_version as u8];
    frame.extend_from_slice(&4u32.to_le_bytes());
    frame.extend_from_slice(&[0, 0, 0, 0]);

    let mut decoder = FrameDecoder::new();
    decoder.push_bytes(&frame);

    assert!(matches!(
        decoder.next_frame::<Request>(),
        Err(crate::RmuxError::UnsupportedWireVersion { .. })
    ));
    assert!(decoder.remaining_bytes().is_empty());
    assert_eq!(decoder.next_frame::<Request>(), Ok(None));
}

#[test]
fn decoder_clears_buffer_after_invalid_wire_version_varint() {
    let mut decoder = FrameDecoder::new();
    decoder.push_bytes(&[crate::RMUX_FRAME_MAGIC, 0x80, 0x80, 0x80, 0x80, 0x80]);

    assert!(matches!(
        decoder.next_frame::<Request>(),
        Err(crate::RmuxError::Decode(message)) if message.contains("wire-version varint")
    ));
    assert!(decoder.remaining_bytes().is_empty());
    assert_eq!(decoder.next_frame::<Request>(), Ok(None));
}

#[test]
fn decode_frame_rejects_truncated_payloads() {
    let mut frame = test_frame_header(4);
    frame.extend_from_slice(&[1, 2]);

    assert_eq!(
        decode_frame::<Request>(&frame),
        Err(crate::RmuxError::IncompleteFrame {
            expected: 4,
            received: 2,
        })
    );
}

#[test]
fn decode_frame_rejects_trailing_bytes() {
    let request = Request::HasSession(HasSessionRequest {
        target: SessionName::new("alpha").expect("valid session"),
    });
    let mut frame = encode_frame(&request).expect("encodes");
    frame.push(0xFF);

    assert!(matches!(
        decode_frame::<Request>(&frame),
        Err(crate::RmuxError::Decode(_))
    ));
}

#[test]
fn decode_frame_rejects_header_only() {
    assert_eq!(
        decode_frame::<Request>(&[crate::RMUX_FRAME_MAGIC, crate::RMUX_WIRE_VERSION as u8]),
        Err(crate::RmuxError::IncompleteFrame {
            expected: 6,
            received: 2,
        })
    );
}

fn test_frame_header(length: u32) -> Vec<u8> {
    let mut frame = vec![crate::RMUX_FRAME_MAGIC, crate::RMUX_WIRE_VERSION as u8];
    frame.extend_from_slice(&length.to_le_bytes());
    frame
}

#[test]
fn decoder_handles_multiple_consecutive_frames() {
    let req_a = Request::HasSession(HasSessionRequest {
        target: SessionName::new("alpha").expect("valid session"),
    });
    let req_b = Request::KillSession(KillSessionRequest {
        target: SessionName::new("beta").expect("valid session"),
        kill_all_except_target: false,
        clear_alerts: false,
        kill_group: false,
    });
    let frame_a = encode_frame(&req_a).expect("encodes");
    let frame_b = encode_frame(&req_b).expect("encodes");

    let mut decoder = FrameDecoder::new();
    let mut combined = frame_a;
    combined.extend_from_slice(&frame_b);
    decoder.push_bytes(&combined);

    assert_eq!(
        decoder.next_frame::<Request>().expect("first frame"),
        Some(req_a)
    );
    assert_eq!(
        decoder.next_frame::<Request>().expect("second frame"),
        Some(req_b)
    );
    assert_eq!(
        decoder.next_frame::<Request>().expect("no more frames"),
        None
    );
}

#[test]
fn every_response_variant_round_trips_through_the_frame_codec() {
    let alpha = SessionName::new("alpha").expect("valid session");
    let pane = PaneTarget::new(alpha.clone(), 1);

    let responses: Vec<crate::Response> = vec![
        crate::Response::NewSession(crate::NewSessionResponse {
            session_name: alpha.clone(),
            detached: true,
            output: None,
        }),
        crate::Response::HasSession(crate::HasSessionResponse { exists: true }),
        crate::Response::KillSession(crate::KillSessionResponse { existed: false }),
        crate::Response::NewWindow(crate::NewWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 2),
        }),
        crate::Response::KillWindow(crate::KillWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 1),
        }),
        crate::Response::SelectWindow(crate::SelectWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 2),
        }),
        crate::Response::RenameWindow(crate::RenameWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 2),
        }),
        crate::Response::NextWindow(crate::NextWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 3),
        }),
        crate::Response::PreviousWindow(crate::PreviousWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 1),
        }),
        crate::Response::LastWindow(crate::LastWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 0),
        }),
        crate::Response::ListWindows(crate::ListWindowsResponse {
            windows: vec![crate::WindowListEntry {
                target: WindowTarget::new(alpha.clone()),
                window_id: "@0".to_owned(),
                name: Some("shell".to_owned()),
                pane_count: 2,
                size: TerminalSize { cols: 80, rows: 24 },
                layout: LayoutName::MainVertical,
                active: true,
                last: false,
                rendered: "0:shell*".to_owned(),
            }],
            output: crate::CommandOutput::from_stdout("0:shell*\n"),
        }),
        crate::Response::MoveWindow(crate::MoveWindowResponse {
            session_name: alpha.clone(),
            target: Some(WindowTarget::with_window(alpha.clone(), 3)),
        }),
        crate::Response::SwapWindow(crate::SwapWindowResponse {
            source: WindowTarget::with_window(alpha.clone(), 1),
            target: WindowTarget::with_window(alpha.clone(), 3),
        }),
        crate::Response::RotateWindow(crate::RotateWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 1),
        }),
        crate::Response::SplitWindow(crate::SplitWindowResponse { pane: pane.clone() }),
        crate::Response::SwapPane(crate::SwapPaneResponse {
            source: pane.clone(),
            target: PaneTarget::with_window(SessionName::new("beta").unwrap(), 2, 3),
        }),
        crate::Response::LastPane(crate::LastPaneResponse {
            target: PaneTarget::with_window(SessionName::new("beta").unwrap(), 0, 2),
        }),
        crate::Response::JoinPane(crate::JoinPaneResponse {
            target: PaneTarget::with_window(SessionName::new("beta").unwrap(), 3, 2),
        }),
        crate::Response::BreakPane(crate::BreakPaneResponse {
            target: PaneTarget::with_window(SessionName::new("beta").unwrap(), 5, 2),
            output: None,
        }),
        crate::Response::KillPane(crate::KillPaneResponse {
            target: pane.clone(),
            window_destroyed: false,
        }),
        crate::Response::SelectLayout(crate::SelectLayoutResponse {
            layout: LayoutName::MainVertical,
        }),
        crate::Response::NextLayout(crate::NextLayoutResponse {
            layout: LayoutName::Tiled,
        }),
        crate::Response::PreviousLayout(crate::PreviousLayoutResponse {
            layout: LayoutName::EvenVertical,
        }),
        crate::Response::ResizePane(crate::ResizePaneResponse {
            target: pane.clone(),
            adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 80 },
        }),
        crate::Response::DisplayPanes(crate::DisplayPanesResponse {
            target: WindowTarget::new(alpha.clone()),
            pane_count: 2,
        }),
        crate::Response::SelectPane(crate::SelectPaneResponse {
            target: pane.clone(),
        }),
        crate::Response::SendKeys(crate::SendKeysResponse { key_count: 3 }),
        crate::Response::AttachSession(crate::AttachSessionResponse {
            session_name: alpha.clone(),
        }),
        crate::Response::SwitchClient(crate::SwitchClientResponse {
            session_name: alpha.clone(),
        }),
        crate::Response::DetachClient(crate::DetachClientResponse),
        crate::Response::SetOption(crate::SetOptionResponse {
            scope: ScopeSelector::Global,
            option: OptionName::DefaultTerminal,
            mode: SetOptionMode::Replace,
        }),
        crate::Response::SetOption(crate::SetOptionResponse {
            scope: ScopeSelector::Global,
            option: OptionName::TerminalFeatures,
            mode: SetOptionMode::Append,
        }),
        crate::Response::SetEnvironment(crate::SetEnvironmentResponse {
            scope: ScopeSelector::Session(alpha.clone()),
            name: "TERM".to_owned(),
        }),
        crate::Response::SetHook(crate::SetHookResponse {
            scope: ScopeSelector::Session(alpha),
            hook: HookName::ClientAttached,
            lifecycle: HookLifecycle::Persistent,
        }),
        crate::Response::Error(crate::ErrorResponse {
            error: crate::RmuxError::SessionNotFound("gone".to_owned()),
        }),
        crate::Response::SetBuffer(crate::SetBufferResponse {
            buffer_name: "buffer0".to_owned(),
        }),
        crate::Response::ShowBuffer(crate::ShowBufferResponse {
            output: crate::CommandOutput::from_stdout(b"buffer".to_vec()),
        }),
        crate::Response::PasteBuffer(crate::PasteBufferResponse {
            buffer_name: "buffer0".to_owned(),
        }),
        crate::Response::ListBuffers(crate::ListBuffersResponse {
            output: crate::CommandOutput::from_stdout(b"buffer0: 6 bytes: \"buffer\"\n".to_vec()),
        }),
        crate::Response::DeleteBuffer(crate::DeleteBufferResponse {
            buffer_name: "buffer0".to_owned(),
        }),
        crate::Response::LoadBuffer(crate::LoadBufferResponse {
            buffer_name: "loaded".to_owned(),
        }),
        crate::Response::SaveBuffer(crate::SaveBufferResponse {
            buffer_name: "loaded".to_owned(),
        }),
        crate::Response::CapturePane(crate::CapturePaneResponse::from_output(
            crate::CommandOutput::from_stdout(b"captured\n".to_vec()),
        )),
        crate::Response::CapturePane(crate::CapturePaneResponse::from_buffer(
            "capture-buffer".to_owned(),
        )),
        crate::Response::DisplayMessage(crate::DisplayMessageResponse::from_output(
            crate::CommandOutput::from_stdout(b"displayed\n".to_vec()),
        )),
        crate::Response::DisplayMessage(crate::DisplayMessageResponse::no_output()),
        crate::Response::RenameSession(crate::RenameSessionResponse {
            session_name: SessionName::new("gamma").unwrap(),
        }),
        crate::Response::ListSessions(crate::ListSessionsResponse {
            output: crate::CommandOutput::from_stdout(b"alpha\nbeta\n".to_vec()),
        }),
        crate::Response::ListPanes(crate::ListPanesResponse {
            output: crate::CommandOutput::from_stdout(b"%0\n%1\n".to_vec()),
        }),
        crate::Response::SourceFile(crate::SourceFileResponse::from_output(
            crate::CommandOutput::from_stdout(b"display-message ok\n".to_vec()),
        )),
        crate::Response::ShowHooks(crate::ShowHooksResponse {
            scope: ScopeSelector::Global,
            output: crate::CommandOutput::from_stdout(b"client-attached[0] true\n".to_vec()),
        }),
        crate::Response::CopyMode(crate::CopyModeResponse {
            target: PaneTarget::with_window(SessionName::new("alpha").unwrap(), 0, 0),
            active: true,
            view_mode: false,
        }),
        crate::Response::ControlMode(ControlModeResponse {
            mode: ControlMode::ControlControl,
        }),
        crate::Response::ClockMode(crate::ClockModeResponse {
            target: PaneTarget::with_window(SessionName::new("alpha").unwrap(), 0, 0),
            active: true,
        }),
        crate::Response::ShowMessages(crate::ShowMessagesResponse::from_output(
            crate::CommandOutput::from_stdout(b"terminal\n".to_vec()),
        )),
        crate::Response::KillServer(crate::KillServerResponse),
        crate::Response::LockServer(crate::LockServerResponse),
        crate::Response::LockSession(crate::LockSessionResponse {
            target: SessionName::new("alpha").unwrap(),
        }),
        crate::Response::LockClient(crate::LockClientResponse {
            target_client: "=".to_owned(),
        }),
        crate::Response::ServerAccess(crate::ServerAccessResponse {
            output: crate::CommandOutput::from_stdout(b"owner (W)\n".to_vec()),
        }),
        crate::Response::RefreshClient(crate::RefreshClientResponse {
            target_client: "=".to_owned(),
        }),
        crate::Response::ListClients(crate::ListClientsResponse {
            output: crate::CommandOutput::from_stdout(b"= alpha\n".to_vec()),
            match_count: 1,
        }),
        crate::Response::SuspendClient(crate::SuspendClientResponse {
            target_client: "=".to_owned(),
        }),
        crate::Response::Handshake(crate::HandshakeResponse::current()),
    ];

    for response in responses {
        let frame = encode_frame(&response).expect("encodes");
        let decoded: crate::Response = decode_frame(&frame).expect("decodes");
        assert_eq!(decoded, response);
    }
}

#[test]
fn display_message_response_appends_after_existing_response_variants() {
    let response = crate::Response::DisplayMessage(crate::DisplayMessageResponse::no_output());

    let encoded = bincode::serialize(&response).expect("response encodes");

    assert_eq!(&encoded[..4], 44_u32.to_le_bytes().as_slice());
    assert_eq!(
        bincode::deserialize::<crate::Response>(&encoded).expect("response decodes"),
        response
    );
}

#[test]
fn show_hooks_response_appends_after_existing_response_variants() {
    let response = crate::Response::ShowHooks(crate::ShowHooksResponse {
        scope: ScopeSelector::Global,
        output: crate::CommandOutput::from_stdout(Vec::new()),
    });

    let encoded = bincode::serialize(&response).expect("response encodes");

    assert_eq!(&encoded[..4], 53_u32.to_le_bytes().as_slice());
    assert_eq!(
        bincode::deserialize::<crate::Response>(&encoded).expect("response decodes"),
        response
    );
}

#[test]
fn copy_mode_response_appends_after_existing_response_variants() {
    let response = crate::Response::CopyMode(crate::CopyModeResponse {
        target: PaneTarget::with_window(SessionName::new("alpha").expect("valid session"), 0, 0),
        active: true,
        view_mode: false,
    });

    let encoded = bincode::serialize(&response).expect("response encodes");

    assert_eq!(&encoded[..4], 59_u32.to_le_bytes().as_slice());
    assert_eq!(
        bincode::deserialize::<crate::Response>(&encoded).expect("response decodes"),
        response
    );
}

#[test]
fn server_lifecycle_response_variants_append_after_show_messages() {
    let cases = [
        (
            63_u32,
            crate::Response::KillServer(crate::KillServerResponse),
        ),
        (
            64_u32,
            crate::Response::LockServer(crate::LockServerResponse),
        ),
        (
            65_u32,
            crate::Response::LockSession(crate::LockSessionResponse {
                target: SessionName::new("alpha").expect("valid session"),
            }),
        ),
        (
            66_u32,
            crate::Response::LockClient(crate::LockClientResponse {
                target_client: "=".to_owned(),
            }),
        ),
        (
            67_u32,
            crate::Response::ServerAccess(crate::ServerAccessResponse {
                output: crate::CommandOutput::from_stdout(b"owner (W)\n".to_vec()),
            }),
        ),
    ];

    for (expected_tag, response) in cases {
        let encoded = bincode::serialize(&response).expect("response encodes");
        assert_eq!(&encoded[..4], expected_tag.to_le_bytes().as_slice());
        assert_eq!(
            bincode::deserialize::<crate::Response>(&encoded).expect("response decodes"),
            response
        );
    }
}

#[test]
fn client_surface_response_variants_append_after_server_access() {
    let cases = [
        (
            68_u32,
            crate::Response::RefreshClient(crate::RefreshClientResponse {
                target_client: "=".to_owned(),
            }),
        ),
        (
            69_u32,
            crate::Response::ListClients(crate::ListClientsResponse {
                output: crate::CommandOutput::from_stdout(b"= alpha\n".to_vec()),
                match_count: 1,
            }),
        ),
        (
            70_u32,
            crate::Response::SuspendClient(crate::SuspendClientResponse {
                target_client: "=".to_owned(),
            }),
        ),
    ];

    for (expected_tag, response) in cases {
        let encoded = bincode::serialize(&response).expect("response encodes");
        assert_eq!(&encoded[..4], expected_tag.to_le_bytes().as_slice());
        assert_eq!(
            bincode::deserialize::<crate::Response>(&encoded).expect("response decodes"),
            response
        );
    }
}

#[test]
fn clock_mode_response_appends_after_control_mode_response() {
    let response = crate::Response::ClockMode(crate::ClockModeResponse {
        target: PaneTarget::with_window(SessionName::new("alpha").expect("valid session"), 0, 0),
        active: true,
    });

    let encoded = bincode::serialize(&response).expect("response encodes");

    assert_eq!(&encoded[..4], 61_u32.to_le_bytes().as_slice());
    assert_eq!(
        bincode::deserialize::<crate::Response>(&encoded).expect("response decodes"),
        response
    );
}
