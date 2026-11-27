use std::{any::TypeId, error::Error, fmt::Debug, str::FromStr};

use rmux_sdk::{LayoutName, ParseLayoutNameError};
use serde::{de::DeserializeOwned, Serialize};

// The old public spellings, serde names and binary indices are independent
// expectations, not values derived from the new implementation.
const LAYOUTS: [(LayoutName, &str, &str, u32); 7] = [
    (LayoutName::MainVertical, "main-vertical", "MainVertical", 0),
    (
        LayoutName::MainHorizontal,
        "main-horizontal",
        "MainHorizontal",
        1,
    ),
    (
        LayoutName::EvenHorizontal,
        "even-horizontal",
        "EvenHorizontal",
        2,
    ),
    (LayoutName::EvenVertical, "even-vertical", "EvenVertical", 3),
    (LayoutName::Tiled, "tiled", "Tiled", 4),
    (
        LayoutName::MainHorizontalMirrored,
        "main-horizontal-mirrored",
        "MainHorizontalMirrored",
        5,
    ),
    (
        LayoutName::MainVerticalMirrored,
        "main-vertical-mirrored",
        "MainVerticalMirrored",
        6,
    ),
];

#[test]
fn layout_and_parse_error_have_sdk_type_identity() {
    fn public_traits<T: Clone + Copy + Debug + PartialEq + Eq + Serialize + DeserializeOwned>() {}
    public_traits::<LayoutName>();
    assert_ne!(
        TypeId::of::<LayoutName>(),
        TypeId::of::<rmux_proto::LayoutName>()
    );
    assert_ne!(
        TypeId::of::<ParseLayoutNameError>(),
        TypeId::of::<rmux_proto::RmuxError>()
    );
    let error: ParseLayoutNameError = LayoutName::from_str("unknown").unwrap_err();
    assert!(error.source().is_none());
}

#[test]
fn every_layout_preserves_display_parsing_and_serde() {
    for (layout, name, variant, index) in LAYOUTS {
        assert_eq!(layout.to_string(), name);
        assert_eq!(format!("{layout:?}"), variant);
        assert_eq!(name.parse::<LayoutName>().unwrap(), layout);

        let proto = name.parse::<rmux_proto::LayoutName>().unwrap();
        assert_eq!(proto.to_string(), name);
        let json = format!("\"{variant}\"");
        assert_eq!(serde_json::to_string(&layout).unwrap(), json);
        assert_eq!(serde_json::to_string(&proto).unwrap(), json);
        assert_eq!(serde_json::from_str::<LayoutName>(&json).unwrap(), layout);
        // Serde also accepts an externally tagged unit-variant map.
        let map = format!("{{\"{variant}\":null}}");
        assert_eq!(serde_json::from_str::<LayoutName>(&map).unwrap(), layout);
        assert_eq!(
            serde_json::from_str::<rmux_proto::LayoutName>(&map).unwrap(),
            proto
        );

        let bytes = index.to_le_bytes();
        assert_eq!(bincode::serialize(&layout).unwrap(), bytes);
        assert_eq!(bincode::serialize(&proto).unwrap(), bytes);
        assert_eq!(bincode::deserialize::<LayoutName>(&bytes).unwrap(), layout);
        assert_eq!(
            bincode::deserialize::<rmux_proto::LayoutName>(&bytes).unwrap(),
            proto
        );
    }
}

#[test]
fn parsing_rejects_noncanonical_names_with_the_existing_diagnostic() {
    let mut rejected: Vec<String> = [
        "", "main", "even", "t", "0", "7", "unknown", " tiled", "tiled\n", "tiled\0", "tíléd",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for (_, name, variant, _) in LAYOUTS {
        rejected.extend(
            [
                variant.to_owned(),
                name.to_uppercase(),
                format!(" {name}"),
                format!("{name} "),
                name.replace('-', "_"),
            ]
            .into_iter()
            .filter(|value| value != name),
        );
    }
    for value in rejected {
        let error: ParseLayoutNameError = value.parse::<LayoutName>().unwrap_err();
        let old_error = value.parse::<rmux_proto::LayoutName>().unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("server error: unknown layout: {value}")
        );
        assert_eq!(error.to_string(), old_error.to_string());
    }
}

#[test]
fn serde_rejects_unknown_variants_and_keeps_parsing_distinct() {
    for (_, name, _, _) in LAYOUTS {
        let json = serde_json::to_string(name).unwrap();
        assert!(serde_json::from_str::<LayoutName>(&json).is_err());
        assert!(serde_json::from_str::<rmux_proto::LayoutName>(&json).is_err());
    }
    for json in [
        "\"Unknown\"",
        "\"\"",
        "null",
        "0",
        "7",
        "[]",
        "{}",
        "{\"Tiled\":1}",
    ] {
        assert!(serde_json::from_str::<LayoutName>(json).is_err(), "{json}");
        assert!(
            serde_json::from_str::<rmux_proto::LayoutName>(json).is_err(),
            "{json}"
        );
    }
    for index in [7_u32, u32::MAX] {
        assert!(bincode::deserialize::<LayoutName>(&index.to_le_bytes()).is_err());
        assert!(bincode::deserialize::<rmux_proto::LayoutName>(&index.to_le_bytes()).is_err());
    }
    assert!(bincode::deserialize::<LayoutName>(&[0, 0, 0]).is_err());
    assert!(bincode::deserialize::<rmux_proto::LayoutName>(&[0, 0, 0]).is_err());
}
