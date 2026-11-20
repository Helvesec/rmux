use super::{
    combine_char, text_width, truncate_right_to_width, truncate_to_width, CombineResult,
    CombinedSequenceAction, CombinedSequenceDiscard, Utf8Config,
};
use crate::OptionStore;
use rmux_proto::{OptionName, ScopeSelector, SetOptionMode};

fn utf8_config_with(codepoint_widths: &[&str], vs16_wide: bool) -> Utf8Config {
    let mut options = OptionStore::new();
    for entry in codepoint_widths {
        options
            .set(
                ScopeSelector::Global,
                OptionName::CodepointWidths,
                (*entry).to_owned(),
                SetOptionMode::Append,
            )
            .expect("codepoint-widths append succeeds");
    }
    options
        .set(
            ScopeSelector::Global,
            OptionName::VariationSelectorAlwaysWide,
            if vs16_wide { "on" } else { "off" }.to_owned(),
            SetOptionMode::Replace,
        )
        .expect("variation-selector-always-wide set succeeds");
    Utf8Config::from_options(&options)
}

#[test]
fn codepoint_width_overrides_accept_ranges_and_literal_characters() {
    let config = utf8_config_with(&["U+8868=1", "🧪=1", "U+1F600-U+1F64F=1"], true);

    assert_eq!(config.width('表'), 1);
    assert_eq!(config.width('🧪'), 1);
    assert_eq!(config.width('😀'), 1);
    assert_eq!(config.width('🙏'), 1);
}

#[test]
fn variation_selector_respects_the_server_option() {
    let wide = utf8_config_with(&[], true);
    let narrow = utf8_config_with(&[], false);

    assert_eq!(
        combine_char(Some(("❤", 1)), '\u{FE0F}', &wide),
        CombineResult::Combined {
            text: "❤\u{FE0F}".to_owned(),
            width: 2,
        }
    );
    assert_eq!(
        combine_char(Some(("❤", 1)), '\u{FE0F}', &narrow),
        CombineResult::Combined {
            text: "❤\u{FE0F}".to_owned(),
            width: 1,
        }
    );
}

#[test]
fn regional_indicators_and_skin_tones_force_combined_width() {
    let config = Utf8Config::default();

    assert_eq!(
        combine_char(Some(("🇨", 1)), '🇭', &config),
        CombineResult::Combined {
            text: "🇨🇭".to_owned(),
            width: 2,
        }
    );
    assert_eq!(
        combine_char(Some(("👋", 2)), '🏽', &config),
        CombineResult::Combined {
            text: "👋🏽".to_owned(),
            width: 2,
        }
    );
    assert_eq!(
        combine_char(Some(("👨🏻\u{200D}🤝\u{200D}👨", 2)), '🏿', &config),
        CombineResult::Combined {
            text: "👨🏻\u{200D}🤝\u{200D}👨🏿".to_owned(),
            width: 2,
        }
    );
}

#[test]
fn skin_tone_zwj_continuations_preserve_measured_owner_widths() {
    let config = Utf8Config::default();

    assert_eq!(text_width("A\u{200D}👨🏻", &config), 3);
    assert_eq!(text_width("👨\u{200D}🤝\u{200D}👨🏿", &config), 2);
}

#[test]
fn modifier_rooted_zwj_owners_do_not_absorb_skin_tones() {
    let config = Utf8Config::default();

    assert_eq!(text_width("🏻\u{200D}👩🏽", &config), 4);
    assert_eq!(text_width("A\u{200D}👨🏻\u{200D}👩🏽", &config), 5);
}

#[test]
fn already_combined_sequences_do_not_keep_absorbing_flags_or_modifiers() {
    let config = Utf8Config::default();

    assert_eq!(
        combine_char(Some(("🇨🇭", 2)), '🇩', &config),
        CombineResult::Standalone { width: 1 }
    );
    assert_eq!(
        combine_char(Some(("👋🏽", 2)), '🏻', &config),
        CombineResult::Standalone { width: 2 }
    );
    assert_eq!(text_width("🇨🇭🇩", &config), 3);
    assert_eq!(truncate_to_width("🇨🇭🇩A", 3, &config), "🇨🇭🇩");
}

#[test]
fn modifiers_only_combine_when_appended_after_the_base_character() {
    let config = Utf8Config::default();

    assert_eq!(
        combine_char(Some(("🏽", 2)), '👋', &config),
        CombineResult::Standalone { width: 2 }
    );
}

#[test]
fn zwj_sequences_continue_combining_after_the_joiner() {
    let config = Utf8Config::default();

    assert_eq!(
        combine_char(Some(("👨", 2)), '\u{200D}', &config),
        CombineResult::Combined {
            text: "👨\u{200D}".to_owned(),
            width: 2,
        }
    );
    assert_eq!(
        combine_char(Some(("👨\u{200D}", 2)), '👩', &config),
        CombineResult::Combined {
            text: "👨\u{200D}👩".to_owned(),
            width: 2,
        }
    );
    assert_eq!(text_width("👨\u{200D}👩A", &config), 3);
    assert_eq!(
        truncate_to_width("👨\u{200D}👩A", 2, &config),
        "👨\u{200D}👩"
    );
}

#[test]
fn overlong_zwj_sequence_discards_its_tail_without_shifting_following_text() {
    let config = Utf8Config::default();
    let bounded_prefix = "👩\u{200D}❤\u{FE0F}\u{200D}💋\u{200D}👨\u{200D}";
    let captured_sequence = "👩\u{200D}❤\u{FE0F}\u{200D}💋\u{200D}👨\u{200D}👩";
    let captured_with_ascii = format!("{captured_sequence}A");
    assert!(bounded_prefix.len() <= super::MAX_COMBINED_BYTES);
    assert!(captured_sequence.len() > super::MAX_COMBINED_BYTES);

    assert_eq!(text_width(&captured_with_ascii, &config), 3);
    assert_eq!(
        truncate_to_width(&captured_with_ascii, 2, &config),
        captured_sequence
    );
    assert_eq!(
        truncate_to_width(&captured_with_ascii, 3, &config),
        captured_with_ascii
    );

    let extended_tail = format!("{captured_sequence}🏽\u{200D}👩A");
    assert_eq!(text_width(&extended_tail, &config), 3);
    assert_eq!(truncate_to_width(&extended_tail, 3, &config), extended_tail);
    assert_eq!(text_width(&format!("{captured_sequence}表"), &config), 4);
}

#[test]
fn gh219_discarded_tail_waits_for_a_fresh_spacing_character() {
    let config = Utf8Config::default();
    let mut discard = CombinedSequenceDiscard::Extensions;

    assert!(discard.is_active());
    assert_eq!(
        discard.action_for('\u{FE0F}', &config),
        CombinedSequenceAction::Discard
    );
    assert!(discard.is_active());
    assert_eq!(
        discard.action_for('中', &config),
        CombinedSequenceAction::Standalone
    );
    assert!(!discard.is_active());
}

#[test]
fn gh219_truncation_keeps_discarded_bytes_inside_source_slices() {
    let config = Utf8Config::default();
    let family = "👩\u{200D}❤\u{FE0F}\u{200D}💋\u{200D}👨";
    let status = "👩🏽\u{200D}❤\u{FE0F}\u{200D}👨🏽 #S";
    let left_cases = [
        ("\u{200D}abc", 3, "\u{200D}abc"),
        ("A\u{3164}B", 2, "A\u{3164}B"),
        (&format!("{family}A"), 3, &format!("{family}A")),
        (status, 10, status),
    ];

    for (value, width, expected) in left_cases {
        let clipped = truncate_to_width(value, width, &config);
        assert_eq!(clipped, expected);
        assert!(value.starts_with(&clipped));
        assert!(value.is_char_boundary(clipped.len()));
        assert!(text_width(&clipped, &config) <= width);
    }

    let value = "AB👩\u{200D}❤\u{FE0F}\u{200D}💋\u{200D}👨X";
    let clipped = truncate_right_to_width(value, 4, &config);
    let source_start = value.find('B').expect("B source boundary");
    assert_eq!(clipped, value[source_start..]);
    assert!(value.ends_with(&clipped));
    assert!(value.is_char_boundary(value.len() - clipped.len()));
    assert_eq!(text_width(&clipped, &config), 4);
}

#[test]
fn zwj_at_the_exact_byte_bound_starts_a_following_cjk_cell() {
    let config = Utf8Config::default();
    let bounded_cluster = concat!(
        "👩\u{20D0}",
        "\u{0301}\u{0301}\u{0301}\u{0301}\u{0301}\u{0301}",
        "\u{0301}\u{0301}\u{0301}\u{0301}\u{0301}",
        "\u{200D}",
    );
    let value = format!("{bounded_cluster}中");
    assert_eq!(bounded_cluster.len(), super::MAX_COMBINED_BYTES);

    assert_eq!(text_width(&value, &config), 4);
    assert_eq!(truncate_to_width(&value, 2, &config), bounded_cluster);
    assert_eq!(truncate_to_width(&value, 4, &config), value);
}

#[test]
fn truncation_is_always_a_source_byte_prefix_or_suffix() {
    let config = Utf8Config::default();
    let values = [
        "👨\u{200D}👩\u{200D}👦\u{200D}👦中ABCDEFGH",
        "👩\u{200D}❤\u{FE0F}\u{200D}💋\u{200D}👨中A",
        "\u{3164}A",
        "각A",
        "👋🏽A",
        "🇨🇭A",
        "❤\u{FE0F}A",
        "A\u{0301}B",
        "\u{200D}A",
    ];

    for value in values {
        for width in 0..=text_width(value, &config).saturating_add(1) {
            let left = truncate_to_width(value, width, &config);
            assert!(
                value.starts_with(&left),
                "truncate_to_width({value:?}, {width}) returned non-prefix {left:?}"
            );
            assert!(value.is_char_boundary(left.len()));
            assert!(text_width(&left, &config) <= width);

            let right = truncate_right_to_width(value, width, &config);
            assert!(
                value.ends_with(&right),
                "truncate_right_to_width({value:?}, {width}) returned non-suffix {right:?}"
            );
            assert!(value.is_char_boundary(value.len() - right.len()));
            assert!(text_width(&right, &config) <= width);
        }
    }
}

#[test]
fn hangul_jamo_combines_only_for_valid_sequences() {
    let config = Utf8Config::default();

    assert_eq!(
        combine_char(Some(("ᄀ", 2)), 'ᅡ', &config),
        CombineResult::Combined {
            text: "가".to_owned(),
            width: 2,
        }
    );
    assert_eq!(
        combine_char(Some(("가", 2)), 'ᆨ', &config),
        CombineResult::Combined {
            text: "각".to_owned(),
            width: 2,
        }
    );
    assert_eq!(
        combine_char(Some(("A", 1)), 'ᅡ', &config),
        CombineResult::Discard
    );
}

#[test]
fn text_width_and_truncation_follow_display_width() {
    let config = Utf8Config::default();

    assert_eq!(text_width("表A", &config), 3);
    assert_eq!(truncate_to_width("表A", 2, &config), "表");
    assert_eq!(text_width("🇨🇭A", &config), 3);
    assert_eq!(truncate_to_width("🇨🇭A", 2, &config), "🇨🇭");
}

#[test]
fn ascii_width_and_truncation_use_identity_fast_path() {
    let config = Utf8Config::default();

    assert_eq!(text_width("abcdef", &config), 6);
    assert_eq!(truncate_to_width("abcdef", 3, &config), "abc");
    assert_eq!(truncate_right_to_width("abcdef", 3, &config), "def");
}

#[test]
fn ascii_width_override_disables_identity_fast_path() {
    let config = utf8_config_with(&["A=2"], true);

    assert_eq!(text_width("AB", &config), 3);
    assert_eq!(truncate_to_width("AB", 1, &config), "");
    assert_eq!(truncate_to_width("AB", 2, &config), "A");
    assert_eq!(truncate_right_to_width("AB", 1, &config), "B");
}

#[test]
fn right_truncation_keeps_display_cells_from_the_end() {
    let config = Utf8Config::default();

    assert_eq!(truncate_right_to_width("A表B", 3, &config), "表B");
    assert_eq!(
        truncate_right_to_width("A👨\u{200D}👩B", 3, &config),
        "👨\u{200D}👩B"
    );
    assert_eq!(truncate_right_to_width("AB", 0, &config), "");
}
