use rmux_proto::{OptionName, ScopeSelector, SetOptionMode, SetOptionRequest};

#[test]
fn all_149_option_tags_remain_frozen() {
    let frozen: &[(u32, OptionName, &str)] = include!("fixtures/option_names_4961.rs");
    assert_eq!(frozen.len(), 149);
    for &(tag, option, name) in frozen {
        let bytes = tag.to_le_bytes();
        assert_eq!(bincode::serialize(&option).unwrap(), bytes, "{name}");
        assert_eq!(
            bincode::deserialize::<OptionName>(&bytes).unwrap(),
            option,
            "{name}"
        );
    }
}

#[test]
fn unknown_nested_option_tags_still_fail_decoding() {
    let request = SetOptionRequest {
        scope: ScopeSelector::Global,
        option: OptionName::Status,
        value: "on".into(),
        mode: SetOptionMode::Replace,
    };
    let mut bytes = bincode::serialize(&request).unwrap();
    for tag in [149_u32, u32::MAX] {
        bytes[4..8].copy_from_slice(&tag.to_le_bytes());
        assert!(bincode::deserialize::<SetOptionRequest>(&bytes).is_err());
    }
}
