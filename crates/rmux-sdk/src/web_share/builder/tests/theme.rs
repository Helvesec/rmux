use super::{read_request, write_response};
use crate::transport::TransportClient;
use crate::{
    Pane, PaneRef, RmuxEndpoint, Session, WebShareBuilder, WebTerminalPalette, WebTerminalTheme,
};
use rmux_proto::{
    encode_frame, CommandOutput, CreateWebShareRequest, ErrorResponse, HandshakeResponse,
    ListPanesResponse, PaneId, PaneTargetRef, Request, Response, SessionName, WebShareRequest,
    WebShareScope, WebShareUrlOptions, WebTerminalPalette as ProtoPalette,
    WebTerminalTheme as ProtoTheme,
};
use tokio::io::AsyncReadExt;

type Configure = for<'a> fn(WebShareBuilder<'a>) -> WebShareBuilder<'a>;

fn palette() -> WebTerminalPalette {
    WebTerminalPalette {
        foreground: "#AaBbCc".to_owned(),
        background: String::new(),
        cursor: " arbitrary color ".to_owned(),
        ansi: std::array::from_fn(|index| format!("#{index:02x}{index:02x}{index:02x}")),
    }
}

// Expected DTO values are independent of the SDK conversion. These expectations
// also pass against the unchanged pre-cutoff builder.
fn expected_palette() -> ProtoPalette {
    ProtoPalette {
        foreground: "#AaBbCc".to_owned(),
        background: String::new(),
        cursor: " arbitrary color ".to_owned(),
        ansi: [
            "#000000", "#010101", "#020202", "#030303", "#040404", "#050505", "#060606", "#070707",
            "#080808", "#090909", "#0a0a0a", "#0b0b0b", "#0c0c0c", "#0d0d0d", "#0e0e0e", "#0f0f0f",
        ]
        .map(str::to_owned),
    }
}

async fn assert_create_frame(
    pane_scope: bool,
    configure: Configure,
    theme: Option<ProtoTheme>,
    with_palette: bool,
) {
    let name = SessionName::new("alpha").unwrap();
    let (client, mut server) = tokio::io::duplex(8192);
    let transport = TransportClient::spawn(client);
    let session = Session::new(
        name.clone(),
        RmuxEndpoint::Default,
        None,
        transport.clone(),
        false,
        None,
    );
    let pane = Pane::new(
        PaneRef::new(name.clone(), 1, 3),
        RmuxEndpoint::Default,
        None,
        transport,
    );
    let share = tokio::spawn(async move {
        let builder = if pane_scope {
            pane.share()
        } else {
            session.share()
        };
        configure(builder).await
    });

    assert!(matches!(
        read_request(&mut server).await,
        Request::Handshake(_)
    ));
    let mut handshake = HandshakeResponse::current();
    if !handshake
        .capabilities
        .iter()
        .any(|value| value == rmux_proto::CAPABILITY_WEB_SHARE)
    {
        handshake
            .capabilities
            .push(rmux_proto::CAPABILITY_WEB_SHARE.to_owned());
    }
    write_response(&mut server, Response::Handshake(handshake)).await;
    if pane_scope {
        let Request::ListPanes(request) = read_request(&mut server).await else {
            panic!("pane slot must resolve before sharing");
        };
        assert_eq!(request.target, name);
        assert_eq!(request.target_window_index, Some(1));
        write_response(
            &mut server,
            Response::ListPanes(ListPanesResponse {
                output: CommandOutput::from_stdout("1:3:%7\n"),
            }),
        )
        .await;
    }

    let expected = Request::WebShare(Box::new(WebShareRequest::Create(CreateWebShareRequest {
        scope: if pane_scope {
            WebShareScope::Pane(PaneTargetRef::by_id(name, PaneId::new(7)))
        } else {
            WebShareScope::Session(name)
        },
        public_base_url: None,
        tunnel_provider: None,
        frontend_url: None,
        ttl_seconds: None,
        expires_at_unix: None,
        max_spectators: None,
        max_operators: None,
        url_options: WebShareUrlOptions {
            no_navbar: false,
            no_disclaimer: false,
            show_viewers: true,
            terminal_theme: theme,
        },
        require_pin: true,
        operator_pin: None,
        spectator_pin: None,
        terminal_palette: with_palette.then(|| Box::new(expected_palette())),
        operator: true,
        spectator: true,
        controls: !pane_scope,
        kill_session_on_expire: false,
    })));
    let expected_frame = encode_frame(&expected).unwrap();
    let mut actual_frame = vec![0; expected_frame.len()];
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        server.read_exact(&mut actual_frame),
    )
    .await
    .expect("create frame arrives")
    .expect("complete frame");
    assert_eq!(actual_frame, expected_frame);

    write_response(
        &mut server,
        Response::Error(ErrorResponse {
            error: rmux_proto::RmuxError::Server("stop after frame assertion".to_owned()),
        }),
    )
    .await;
    let Err(error) = share.await.expect("share task completes") else {
        panic!("mock rejects the observed request");
    };
    assert!(error.to_string().contains("stop after frame assertion"));
}

#[tokio::test]
async fn all_themes_and_optional_palettes_preserve_exact_session_and_pane_frames() {
    let cases: [(Configure, Option<ProtoTheme>, bool); 8] = [
        (|b| b, None, false),
        (|b| b.terminal_palette(palette()), None, true),
        (
            |b| b.theme(WebTerminalTheme::User),
            Some(ProtoTheme::User),
            false,
        ),
        (
            |b| b.theme(WebTerminalTheme::Light),
            Some(ProtoTheme::Light),
            false,
        ),
        (
            |b| b.theme(WebTerminalTheme::Dark),
            Some(ProtoTheme::Dark),
            false,
        ),
        (
            |b| b.theme(WebTerminalTheme::User).terminal_palette(palette()),
            Some(ProtoTheme::User),
            true,
        ),
        (
            |b| b.theme(WebTerminalTheme::Light).terminal_palette(palette()),
            Some(ProtoTheme::Light),
            true,
        ),
        (
            |b| b.theme(WebTerminalTheme::Dark).terminal_palette(palette()),
            Some(ProtoTheme::Dark),
            true,
        ),
    ];
    for pane_scope in [false, true] {
        for (configure, theme, with_palette) in cases {
            assert_create_frame(pane_scope, configure, theme, with_palette).await;
        }
    }
}

#[tokio::test]
async fn theme_aliases_and_overwrites_preserve_exact_session_and_pane_frames() {
    let cases: [(Configure, ProtoTheme); 6] = [
        (|b| b.light_theme().user_theme(), ProtoTheme::User),
        (|b| b.dark_theme().light_theme(), ProtoTheme::Light),
        (|b| b.user_theme().dark_theme(), ProtoTheme::Dark),
        (
            |b| b.dark_theme().terminal_theme(WebTerminalTheme::User),
            ProtoTheme::User,
        ),
        (
            |b| {
                b.terminal_theme(WebTerminalTheme::Dark)
                    .theme(WebTerminalTheme::Light)
            },
            ProtoTheme::Light,
        ),
        (
            |b| {
                b.theme(WebTerminalTheme::Light)
                    .terminal_theme(WebTerminalTheme::Dark)
            },
            ProtoTheme::Dark,
        ),
    ];
    for pane_scope in [false, true] {
        for (configure, expected) in cases {
            assert_create_frame(pane_scope, configure, Some(expected), false).await;
        }
        assert_create_frame(
            pane_scope,
            |b| {
                let first = WebTerminalPalette {
                    foreground: "first foreground".to_owned(),
                    background: "first background".to_owned(),
                    cursor: "first cursor".to_owned(),
                    ansi: std::array::from_fn(|index| format!("first ansi {index}")),
                };
                b.terminal_palette(first)
                    .light_theme()
                    .terminal_palette(palette())
                    .user_theme()
            },
            Some(ProtoTheme::User),
            true,
        )
        .await;
    }
}
