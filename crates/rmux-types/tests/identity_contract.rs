//! Fixed identity contract, characterized on eb6dc711 before moving ownership.
use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::hash::{Hash, Hasher};

use rmux_types::{PaneId, SessionId, SessionName, WindowId};

fn hash(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn constructors(raw: &str, canonical: &str) {
    let name = SessionName::new(raw).unwrap();
    assert_eq!(name.as_str(), canonical);
    assert_eq!(name.as_ref(), canonical);
    assert_eq!(name.to_string(), canonical);
    assert_eq!(name.clone().into_inner(), canonical);
    assert_eq!(raw.parse::<SessionName>().unwrap(), name);
    assert_eq!(SessionName::try_from(raw).unwrap(), name);
    assert_eq!(SessionName::try_from(raw.to_owned()).unwrap(), name);
    assert_eq!(hash(&name), hash(&name.clone()));
    assert_eq!(hash(&name), hash(&canonical));
}

const C0: [&str; 32] = [
    "\\000", "\\001", "\\002", "\\003", "\\004", "\\005", "\\006", "\\a", "\\b", "\\t", "\\n",
    "\\v", "\\f", "\\r", "\\016", "\\017", "\\020", "\\021", "\\022", "\\023", "\\024", "\\025",
    "\\026", "\\027", "\\030", "\\031", "\\032", "\\033", "\\034", "\\035", "\\036", "\\037",
];

const FORMATS: &[(char, &str)] = &[
    ('\u{00ad}', "\\302\\255"),
    ('\u{061c}', "\\330\\234"),
    ('\u{200b}', "\\342\\200\\213"),
    ('\u{200c}', "\\342\\200\\214"),
    ('\u{200d}', "\\342\\200\\215"),
    ('\u{200e}', "\\342\\200\\216"),
    ('\u{200f}', "\\342\\200\\217"),
    ('\u{2028}', "\\342\\200\\250"),
    ('\u{2029}', "\\342\\200\\251"),
    ('\u{202a}', "\\342\\200\\252"),
    ('\u{202b}', "\\342\\200\\253"),
    ('\u{202c}', "\\342\\200\\254"),
    ('\u{202d}', "\\342\\200\\255"),
    ('\u{202e}', "\\342\\200\\256"),
    ('\u{2066}', "\\342\\201\\246"),
    ('\u{2067}', "\\342\\201\\247"),
    ('\u{2068}', "\\342\\201\\250"),
    ('\u{2069}', "\\342\\201\\251"),
    ('\u{feff}', "\\357\\273\\277"),
];

#[test]
fn every_constructor_preserves_normalization_families() {
    for (code, escaped) in C0.iter().enumerate() {
        constructors(
            &format!("a{}z", char::from_u32(code as u32).unwrap()),
            &format!("a{escaped}z"),
        );
    }
    // DEL and C1 use code-point octal, not octal of their UTF-8 bytes.
    for code in 0x7f..=0x9f {
        constructors(
            &char::from_u32(code).unwrap().to_string(),
            &format!("\\{code:03o}"),
        );
    }
    for &(character, escaped) in FORMATS {
        constructors(&character.to_string(), escaped);
    }
    constructors("alpha:beta.gamma", "alpha_beta_gamma");
    constructors(" ", " ");
    constructors("MiXeD-Case_01 é日本😀", "MiXeD-Case_01 é日本😀");
    // Adjacent non-Cc characters, including unlisted format characters, stay literal.
    let neighbors = "\u{00ac}\u{00ae}\u{061b}\u{061d}\u{200a}\u{2010}\u{2027}\u{202f}\u{2065}\u{206a}\u{fefe}\u{ff00}\u{2060}";
    constructors(neighbors, neighbors);
    constructors("a\\nz", "a\\\\nz");
    constructors("a\nz", "a\\nz");
    constructors("a\\000z", "a\\\\000z");
}

#[test]
fn all_empty_constructors_have_the_same_leaf_diagnostic() {
    let errors = [
        SessionName::new("").unwrap_err(),
        "".parse::<SessionName>().unwrap_err(),
        SessionName::try_from("").unwrap_err(),
        SessionName::try_from(String::new()).unwrap_err(),
    ];
    for error in errors {
        assert_eq!(
            error.to_string(),
            "invalid session name: session names must be non-empty"
        );
        assert!(error.source().is_none());
    }
}

#[test]
fn numeric_ids_preserve_bounds_order_hash_and_copy() {
    fn bounds<T: Copy + Clone + Ord + Hash + Send + Sync + 'static>() {}
    bounds::<SessionId>();
    bounds::<WindowId>();
    bounds::<PaneId>();
    for raw in [0, 1, 257, u32::MAX] {
        let session = SessionId::new(raw);
        let window = WindowId::new(raw);
        let pane = PaneId::new(raw);
        assert_eq!(session, SessionId::from(raw));
        assert_eq!(window, WindowId::from(raw));
        assert_eq!(pane, PaneId::from(raw));
        assert_eq!([u32::from(session), window.into(), pane.into()], [raw; 3]);
        assert_eq!([session.as_u32(), window.as_u32(), pane.as_u32()], [raw; 3]);
        assert_eq!(session.to_string(), format!("${raw}"));
        assert_eq!(window.to_string(), format!("@{raw}"));
        assert_eq!(pane.to_string(), format!("%{raw}"));
        assert_eq!(
            [hash(&session), hash(&window), hash(&pane)],
            [hash(&raw); 3]
        );
    }
    assert!(SessionId::new(0) < SessionId::new(u32::MAX));
    assert!(WindowId::new(0) < WindowId::new(u32::MAX));
    assert!(PaneId::new(0) < PaneId::new(u32::MAX));
}

#[cfg(feature = "serde")]
#[test]
fn serde_preserves_raw_and_canonical_policies_for_every_family() {
    let mut pairs: Vec<(String, String)> = C0
        .iter()
        .enumerate()
        .map(|(code, escaped)| {
            (
                char::from_u32(code as u32).unwrap().to_string(),
                (*escaped).to_owned(),
            )
        })
        .collect();
    pairs.extend((0x7f..=0x9f).map(|code| {
        (
            char::from_u32(code).unwrap().to_string(),
            format!("\\{code:03o}"),
        )
    }));
    pairs.extend(
        FORMATS
            .iter()
            .map(|(ch, escaped)| (ch.to_string(), (*escaped).to_owned())),
    );
    pairs.extend(
        [
            ("alpha:beta.gamma", "alpha_beta_gamma"),
            ("\\", "\\"),
            ("\\n", "\\n"),
            ("\\000", "\\000"),
            ("é日本😀", "é日本😀"),
            ("x\\n:\n.\u{202e}", "x\\n_\\n_\\342\\200\\256"),
        ]
        .map(|(raw, expected)| (raw.to_owned(), expected.to_owned())),
    );
    for (raw, canonical) in pairs {
        let json = serde_json::to_string(&raw).unwrap();
        let name: SessionName = serde_json::from_str(&json).unwrap();
        assert_eq!(name.as_str(), canonical);
        let wire = bincode::serialize(&raw).unwrap();
        let decoded: SessionName = bincode::deserialize(&wire).unwrap();
        assert_eq!(decoded, name);
        // Constructor output must survive repeated decoding without escaping twice.
        let constructed = SessionName::new(raw).unwrap();
        for original in [name, constructed] {
            let bytes = bincode::serialize(&original).unwrap();
            assert_eq!(bytes, bincode::serialize(original.as_str()).unwrap());
            assert_eq!(
                bincode::deserialize::<SessionName>(&bytes).unwrap(),
                original
            );
            let json = serde_json::to_string(&original).unwrap();
            assert_eq!(json, serde_json::to_string(original.as_str()).unwrap());
            assert_eq!(
                serde_json::from_str::<SessionName>(&json).unwrap(),
                original
            );
        }
    }
}

#[cfg(feature = "serde")]
#[test]
fn json_and_binary_layouts_and_validation_diagnostics_are_fixed() {
    let name = SessionName::new("é\\n").unwrap();
    assert_eq!(serde_json::to_string(&name).unwrap(), r#""é\\\\n""#);
    assert_eq!(
        bincode::serialize(&name).unwrap(),
        [5, 0, 0, 0, 0, 0, 0, 0, 0xc3, 0xa9, 0x5c, 0x5c, 0x6e]
    );
    for raw in [0u32, 257, u32::MAX] {
        assert_eq!(
            bincode::serialize(&SessionId::new(raw)).unwrap(),
            raw.to_le_bytes()
        );
        assert_eq!(
            bincode::serialize(&WindowId::new(raw)).unwrap(),
            raw.to_le_bytes()
        );
        assert_eq!(
            bincode::serialize(&PaneId::new(raw)).unwrap(),
            raw.to_le_bytes()
        );
        let json = raw.to_string();
        assert_eq!(serde_json::to_string(&SessionId::new(raw)).unwrap(), json);
        assert_eq!(serde_json::to_string(&WindowId::new(raw)).unwrap(), json);
        assert_eq!(serde_json::to_string(&PaneId::new(raw)).unwrap(), json);
        assert_eq!(
            serde_json::from_str::<SessionId>(&json).unwrap().as_u32(),
            raw
        );
        assert_eq!(
            serde_json::from_str::<WindowId>(&json).unwrap().as_u32(),
            raw
        );
        assert_eq!(serde_json::from_str::<PaneId>(&json).unwrap().as_u32(), raw);
    }
    assert_eq!(
        bincode::deserialize::<SessionName>(&[0; 8])
            .unwrap_err()
            .to_string(),
        "invalid session name: session names must be non-empty"
    );
    assert_eq!(
        serde_json::from_str::<SessionName>(r#""""#)
            .unwrap_err()
            .to_string(),
        "invalid session name: session names must be non-empty"
    );
    for json in ["-1", "4294967296", "1.5", "\"7\""] {
        assert!(serde_json::from_str::<SessionId>(json).is_err());
        assert!(serde_json::from_str::<WindowId>(json).is_err());
        assert!(serde_json::from_str::<PaneId>(json).is_err());
    }
}
