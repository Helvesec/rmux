use rmux_sdk::{ForegroundSource, PaneStateClosedReason};
use serde::{de::DeserializeOwned, Serialize};
use std::fmt::Debug;
use std::hash::Hash;

fn assert_enum_contract<T>(cases: &[(T, &str, [u8; 4])])
where
    T: Debug + Clone + Copy + PartialEq + Eq + Hash + Serialize + DeserializeOwned,
{
    for &(value, name, bytes) in cases {
        let json = format!("\"{name}\"");
        assert_eq!(format!("{value:?}"), name);
        assert_eq!(serde_json::to_string(&value).unwrap(), json);
        assert_eq!(serde_json::from_str::<T>(&json).unwrap(), value);
        assert_eq!(bincode::serialize(&value).unwrap(), bytes);
        assert_eq!(bincode::deserialize::<T>(&bytes).unwrap(), value);
    }
    assert!(serde_json::from_str::<T>("\"Unknown\"").is_err());
    assert!(bincode::deserialize::<T>(&[255, 255, 255, 255]).is_err());
}

#[test]
fn foreground_source_retains_traits_names_and_binary_serde_order() {
    assert_enum_contract(&[
        (ForegroundSource::Process, "Process", [0, 0, 0, 0]),
        (ForegroundSource::RootProcess, "RootProcess", [1, 0, 0, 0]),
        (ForegroundSource::Osc7, "Osc7", [2, 0, 0, 0]),
        (ForegroundSource::Profile, "Profile", [3, 0, 0, 0]),
        (ForegroundSource::Environment, "Environment", [4, 0, 0, 0]),
        (ForegroundSource::RuntimeName, "RuntimeName", [5, 0, 0, 0]),
    ]);
}

#[test]
fn pane_state_close_reason_retains_traits_names_and_binary_serde_order() {
    assert_enum_contract(&[
        (PaneStateClosedReason::Exited, "Exited", [0, 0, 0, 0]),
        (PaneStateClosedReason::DiedKept, "DiedKept", [1, 0, 0, 0]),
        (PaneStateClosedReason::Killed, "Killed", [2, 0, 0, 0]),
    ]);
}
