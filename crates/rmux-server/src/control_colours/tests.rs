use super::*;

#[test]
fn colour_reports_c_whitespace_covers_every_numeric_entry_path() {
    let mut failures = Vec::new();
    for space in [' ', '\t', '\n', '\x0b', '\x0c', '\r'] {
        for (pane, colour, expected) in [
            (
                format!("{space}0"),
                "#abcdef".to_owned(),
                [0xab, 0xcd, 0xef],
            ),
            ("0".into(), format!("{space}1,2,3"), [1, 2, 3]),
            ("0".into(), format!("1,{space}2,3"), [1, 2, 3]),
            ("0".into(), format!("1,2,{space}3"), [1, 2, 3]),
            ("0".into(), format!("cmy:{space}0/0/0"), [255; 3]),
            ("0".into(), format!("cmy:0/{space}0/0"), [255; 3]),
            ("0".into(), format!("cmy:0/0/{space}0"), [255; 3]),
            ("0".into(), format!("cmyk:0/0/0/{space}0.5"), [127; 3]),
            ("0".into(), format!("cmyk:0/0/0/{space}0x1p-1"), [127; 3]),
            ("0".into(), format!("gray{space}50"), [127; 3]),
        ] {
            for (slot, osc) in [(0, 10), (1, 11)] {
                for end in ["\x07", "\x1b\\"] {
                    let value = format!("%{pane}:\x1b]{osc};{colour}{end}");
                    let actual = ColourReport::parse(&value).map(|r| (r.pane, r.slot, r.rgb));
                    if actual != Some((0, slot, expected)) {
                        failures.push(value);
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "numeric whitespace failures: {failures:?}"
    );
    for value in [
        "%\u{00a0}0:\x1b]10;red\x07",
        "%0:\x1b]10;\u{00a0}1,2,3\x07",
        "%0:\x1b]11;cmy:0/0/\u{00a0}0.5\x1b\\",
        "%0:\x1b]11;gray\u{00a0}50\x07",
    ] {
        assert!(ColourReport::parse(value).is_none(), "{value:?}");
    }
}

#[test]
fn colour_reports_cmy_hex_components_match_frozen_oracle_in_each_slot() {
    for (text, rgb) in [
        ("cmy:0x1p-1/0/0", [127, 255, 255]),
        ("cmy:0/0x1p-1/0", [255, 127, 255]),
        ("cmy:0/0/0x1p-1", [255, 255, 127]),
        ("cmyk:0/0/0/0x1p-1", [127; 3]),
        ("cmyk:0x1p-1/0/0/0.5", [63, 127, 127]),
        ("cmyk:0/0x1p-1/0/0.5", [127, 63, 127]),
        ("cmyk:0/0/0x1p-1/0.5", [127, 127, 63]),
        ("cmy:0X.8P0/0x4p-3/+0x1.0p-1", [127; 3]),
        ("cmy:0/0.5/1", [255, 127, 0]),
        ("cmyk:0/0/0/0.5", [127; 3]),
        ("cmy:0/0/0x", [255; 3]),
        ("cmyk:0/0/0/0x", [255; 3]),
    ] {
        for (slot, osc) in [(0, 10), (1, 11)] {
            for end in ["\x07", "\x1b\\"] {
                let report =
                    ColourReport::parse(&format!("%0:\x1b]{osc};{text}{end}")).expect(text);
                assert_eq!((report.slot, report.rgb), (slot, rgb), "{text}");
            }
        }
    }
    for text in [
        "cmy:0x/0/0",
        "cmy:0x1p1/0/0",
        "cmyk:0/0/0/-0x1p-1",
        "cmy:0/0/NaN",
        "cmy:0/0/1.01",
        "cmyk:0/0/0/-0.01",
    ] {
        for osc in [10, 11] {
            for end in ["\x07", "\x1b\\"] {
                assert!(
                    ColourReport::parse(&format!("%0:\x1b]{osc};{text}{end}")).is_none(),
                    "{text}"
                );
            }
        }
    }
}

#[test]
fn colour_reports_decode_measured_x11_values_and_slots() {
    for (text, rgb) in [
        ("rgb:12/34/ab", [0x12, 0x34, 0xab]),
        ("rgb:1234/5678/abcd", [0x12, 0x56, 0xab]),
        ("#123456789abc", [0x12, 0x56, 0x9a]),
        ("LightBlue", [0xad, 0xd8, 0xe6]),
        (" grey50 ", [0x7f; 3]),
        ("gray100", [255; 3]),
        ("256,257,258", [0, 1, 2]),
        ("-1,2,3", [255, 2, 3]),
        ("1,2,3tail", [1, 2, 3]),
        ("cmy:0/0.5/1", [255, 127, 0]),
        ("cmyk:0/0/0/0.5", [127; 3]),
        ("rgb: 1/ 2/ 3", [1, 2, 3]),
    ] {
        for (slot, osc) in [(0, 10), (1, 11)] {
            for end in ["\x07", "\x1b\\"] {
                let report =
                    ColourReport::parse(&format!("%+42suffix:\x1b]{osc};{text}{end}ignored"))
                        .expect(text);
                assert_eq!(
                    (report.pane, report.slot, report.rgb),
                    (42, slot, rgb),
                    "{text}"
                );
            }
        }
    }
}

#[test]
fn colour_reports_are_bounded_complete_and_do_not_accept_unrelated_osc() {
    for text in [
        "rgb:1/2/3",
        "rgb:123/456/789",
        "RGB:12/34/56",
        "default",
        "colour42",
        "?",
        "",
        "rgbi:1/0/0",
    ] {
        assert!(
            ColourReport::parse(&format!("%0:\x1b]10;{text}\x07")).is_none(),
            "{text}"
        );
    }
    for report in [
        "%0:rgb:12/34/56",
        "%0:\x1b]12;red\x07",
        "%0:\x1b]4;0;red\x07",
        "%0:\x1b]10;red",
        "%0:\x1b]10;red\0\x07",
        "%0\0:\x1b]10;red\x07",
        "%4294967296:\x1b]10;red\x07",
    ] {
        assert!(ColourReport::parse(report).is_none(), "{report:?}");
    }
    for (end, limit) in [("\x07", 126), ("\x1b\\", 125)] {
        assert!(
            ColourReport::parse(&format!("%0:\x1b]10;{}red{end}", " ".repeat(limit - 3))).is_some()
        );
        assert!(
            ColourReport::parse(&format!("%0:\x1b]10;{}red{end}", " ".repeat(limit - 2))).is_none()
        );
    }
    assert!(ColourReport::parse(&format!("%0:\x1b]10;{}\x07", "x".repeat(1_000_000))).is_none());
    let first = ColourReport::parse("%0:\x1b]10;red\x07\x1b]11;blue\x07").unwrap();
    assert_eq!((first.slot, first.rgb), (0, [255, 0, 0]));
}

#[test]
fn control_colour_presence_follows_registration_identity_not_reporter_or_session() {
    let clients = ControlColourClients::default();
    let closing_a = Arc::new(AtomicBool::new(false));
    let a = clients.register(closing_a.clone());
    let b = clients.register(Arc::new(AtomicBool::new(false)));
    let mut pane = PaneControlColours::default();
    let other = PaneControlColours::default();
    pane.apply(ColourReport::parse("%0:\x1b]10;red\x07").unwrap(), &clients);
    assert_eq!(pane.query_colours(), [Some([255, 0, 0]), None]);
    assert_eq!(other.query_colours(), [None, None]);
    closing_a.store(true, Ordering::SeqCst);
    drop(a);
    assert_eq!(pane.query_colours(), [Some([255, 0, 0]), None]);
    drop(b);
    assert_eq!(pane.query_colours(), [None, None]);
    let replacement = clients.register(Arc::new(AtomicBool::new(false)));
    assert_eq!(pane.query_colours(), [Some([255, 0, 0]), None]);
    drop(replacement);
    assert_eq!(pane.query_colours(), [None, None]);
}
