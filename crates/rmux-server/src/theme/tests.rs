use super::*;

fn client(report: Theme, background: Option<[u8; 3]>) -> ClientTheme {
    ClientTheme { report, background }
}

#[test]
fn theme_consensus_ignores_unknowns_but_never_votes_or_uses_last_writer() {
    for clients in [
        vec![],
        vec![client(Theme::Unknown, None)],
        vec![client(Theme::Dark, None), client(Theme::Light, None)],
        vec![
            client(Theme::Dark, None),
            client(Theme::Dark, None),
            client(Theme::Light, None),
        ],
    ] {
        assert_eq!(
            ThemeInputs::from_clients(clients.into_iter()).consensus,
            Theme::Unknown
        );
    }
    assert_eq!(
        ThemeInputs::from_clients(
            [client(Theme::Unknown, None), client(Theme::Dark, None)].into_iter()
        )
        .consensus,
        Theme::Dark
    );
}

#[test]
fn theme_sources_follow_r_application_first_background_then_consensus() {
    let inputs = ThemeInputs::from_clients(
        [
            client(Theme::Dark, Some([255; 3])),
            client(Theme::Light, Some([0; 3])),
        ]
        .into_iter(),
    );
    let mut pane = PaneTheme {
        inputs,
        ..PaneTheme::default()
    };
    assert_eq!(
        pane.resolved(),
        (Theme::Light, ThemeSource::OuterBackground)
    );
    pane.application = Some([0; 3]);
    assert_eq!(pane.resolved(), (Theme::Dark, ThemeSource::Application));
    pane.control = Some([255; 3]);
    assert_eq!(pane.resolved(), (Theme::Light, ThemeSource::Control));
    pane.control = None;
    assert_eq!(pane.resolved().0, Theme::Dark);
    pane.application = None;
    pane.inputs = ThemeInputs {
        background: None,
        consensus: Theme::Dark,
    };
    assert_eq!(pane.resolved(), (Theme::Dark, ThemeSource::ClientConsensus));
}

#[test]
fn theme_rgb_boundary_uses_frozen_unweighted_oracle() {
    for (rgb, expected) in [
        ([127, 127, 128], Theme::Dark),
        ([127, 128, 128], Theme::Light),
        ([255, 0, 0], Theme::Dark),
        ([0, 255, 0], Theme::Dark),
        ([0, 0, 255], Theme::Dark),
    ] {
        assert_eq!(Theme::from_rgb(rgb), expected);
    }
}

#[test]
fn theme_notifications_deduplicate_values_not_sources_and_track_unknown() {
    let mut pane = PaneTheme {
        subscribed: true,
        ..PaneTheme::default()
    };
    let light = ThemeInputs {
        consensus: Theme::Light,
        ..ThemeInputs::default()
    };
    assert_eq!(pane.refresh(light, None), b"\x1b[?997;2n");
    assert!(pane.refresh(light, Some([255; 3])).is_empty());
    assert!(pane.refresh(ThemeInputs::default(), None).is_empty());
    assert_eq!(pane.refresh(light, None), b"\x1b[?997;2n");
    pane.subscribed = false;
    assert!(pane
        .refresh(ThemeInputs::default(), Some([0; 3]))
        .is_empty());
}

#[test]
fn theme_query_rechecks_closing_before_a_followup_publication() {
    let closing = Arc::new(AtomicBool::new(false));
    let mut pane = PaneTheme::default();
    pane.refresh_clients(
        vec![ThemeClient {
            value: client(Theme::Dark, Some([255; 3])),
            closing: closing.clone(),
        }],
        None,
    );
    let query = || {
        vec![ThemeEvent {
            reply_offset: 0,
            action: ThemeAction::Query,
        }]
    };
    assert_eq!(pane.apply(Vec::new(), query(), None), b"\x1b[?997;2n");
    closing.store(true, Ordering::SeqCst);
    assert!(pane.apply(Vec::new(), query(), None).is_empty());
}
