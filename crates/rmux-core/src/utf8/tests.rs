use super::{
    combine_char, text_width, truncate_right_to_width, truncate_to_width, CombineResult, Utf8Config,
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
fn right_truncation_keeps_display_cells_from_the_end() {
    let config = Utf8Config::default();

    assert_eq!(truncate_right_to_width("A表B", 3, &config), "表B");
    assert_eq!(
        truncate_right_to_width("A👨\u{200D}👩B", 3, &config),
        "👨\u{200D}👩B"
    );
    assert_eq!(truncate_right_to_width("AB", 0, &config), "");
}
