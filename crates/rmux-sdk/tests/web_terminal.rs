#![cfg(feature = "web")]

use std::{any::TypeId, fmt::Debug};

use rmux_sdk::{WebTerminalPalette, WebTerminalTheme};
use serde::{de::DeserializeOwned, Serialize};

// Literal expectations from the pre-cutoff contract, including binary order.
const THEMES: [(WebTerminalTheme, &str, &str, u32); 3] = [
    (WebTerminalTheme::User, "user", "User", 0),
    (WebTerminalTheme::Light, "light", "Light", 1),
    (WebTerminalTheme::Dark, "dark", "Dark", 2),
];
const URL_VALUES: [&str; 3] = [
    WebTerminalTheme::User.as_url_value(),
    WebTerminalTheme::Light.as_url_value(),
    WebTerminalTheme::Dark.as_url_value(),
];
const PALETTE_JSON: &str = r##"{"foreground":"#AaBbCc","background":"","cursor":" arbitrary color ","ansi":["#000000","#010101","#020202","#030303","#040404","#050505","#060606","#070707","#080808","#090909","#0a0a0a","#0b0b0b","#0c0c0c","#0d0d0d","#0e0e0e","#0f0f0f"]}"##;

fn palette() -> WebTerminalPalette {
    WebTerminalPalette {
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

#[test]
fn web_types_are_owned_by_sdk_and_preserve_public_traits() {
    fn public_traits<T: Debug + Clone + PartialEq + Eq + Serialize + DeserializeOwned>() {}
    fn copy_trait<T: Copy>() {}
    public_traits::<WebTerminalTheme>();
    copy_trait::<WebTerminalTheme>();
    public_traits::<WebTerminalPalette>();
    assert_ne!(
        TypeId::of::<WebTerminalTheme>(),
        TypeId::of::<rmux_proto::WebTerminalTheme>()
    );
    assert_ne!(
        TypeId::of::<WebTerminalPalette>(),
        TypeId::of::<rmux_proto::WebTerminalPalette>()
    );
}

#[test]
fn all_theme_url_values_and_serde_representations_are_preserved() {
    assert_eq!(URL_VALUES, ["user", "light", "dark"]);
    for (theme, name, debug, index) in THEMES {
        assert_eq!(theme.as_url_value(), name);
        assert_eq!(format!("{theme:?}"), debug);
        let json = format!("\"{name}\"");
        assert_eq!(serde_json::to_string(&theme).unwrap(), json);
        assert_eq!(
            serde_json::from_str::<WebTerminalTheme>(&json).unwrap(),
            theme
        );
        let map = format!("{{\"{name}\":null}}");
        assert_eq!(
            serde_json::from_str::<WebTerminalTheme>(&map).unwrap(),
            theme
        );
        let bytes = index.to_le_bytes();
        assert_eq!(bincode::serialize(&theme).unwrap(), bytes);
        assert_eq!(
            bincode::deserialize::<WebTerminalTheme>(&bytes).unwrap(),
            theme
        );
    }
}

#[test]
fn theme_serde_keeps_rejecting_noncanonical_values() {
    for json in [
        r#""User""#,
        r#""LIGHT""#,
        r#"" dark""#,
        r#""dark ""#,
        r#""system""#,
        r#""""#,
        "null",
        "0",
        "[]",
        "{}",
        r#"{"dark":1}"#,
    ] {
        assert!(
            serde_json::from_str::<WebTerminalTheme>(json).is_err(),
            "{json}"
        );
    }
    for bytes in [&[3, 0, 0, 0][..], &[255, 255, 255, 255], &[0, 0, 0]] {
        assert!(bincode::deserialize::<WebTerminalTheme>(bytes).is_err());
    }
}

#[test]
fn complete_palette_serde_preserves_fields_order_and_unvalidated_strings() {
    let palette = palette();
    assert_eq!(serde_json::to_string(&palette).unwrap(), PALETTE_JSON);
    assert_eq!(
        serde_json::from_str::<WebTerminalPalette>(PALETTE_JSON).unwrap(),
        palette
    );

    // Bincode's existing struct order: three strings, then exactly 16 strings
    // without an array length. Build the expectation independently of serde.
    let fields = ["#AaBbCc", "", " arbitrary color "];
    let ansi = [
        "#000000", "#010101", "#020202", "#030303", "#040404", "#050505", "#060606", "#070707",
        "#080808", "#090909", "#0a0a0a", "#0b0b0b", "#0c0c0c", "#0d0d0d", "#0e0e0e", "#0f0f0f",
    ];
    let mut expected = Vec::new();
    for value in fields.into_iter().chain(ansi) {
        expected.extend_from_slice(&(value.len() as u64).to_le_bytes());
        expected.extend_from_slice(value.as_bytes());
    }
    assert_eq!(bincode::serialize(&palette).unwrap(), expected);
    assert_eq!(
        bincode::deserialize::<WebTerminalPalette>(&expected).unwrap(),
        palette
    );
}

#[test]
fn palette_serde_keeps_required_fields_array_length_and_unknown_field_behavior() {
    let value: serde_json::Value = serde_json::from_str(PALETTE_JSON).unwrap();
    for field in ["foreground", "background", "cursor", "ansi"] {
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<WebTerminalPalette>(missing).is_err());
        let mut wrong_type = value.clone();
        wrong_type[field] = serde_json::Value::Null;
        assert!(serde_json::from_value::<WebTerminalPalette>(wrong_type).is_err());
    }
    for length in [0, 15, 17] {
        let mut wrong_length = value.clone();
        wrong_length["ansi"] = serde_json::json!(vec!["#000000"; length]);
        assert!(serde_json::from_value::<WebTerminalPalette>(wrong_length).is_err());
    }
    let mut extra = value;
    extra["future"] = serde_json::json!(true);
    assert_eq!(
        serde_json::from_value::<WebTerminalPalette>(extra).unwrap(),
        palette()
    );
}
