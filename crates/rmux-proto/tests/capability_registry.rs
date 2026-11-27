//! Acceptance for the protocol's typed capability and error projections.
use rmux_proto::capabilities::{
    capability_descriptor, resolve_capability, CapabilityAvailability, CapabilityLifetime,
    CapabilityRequirement, CAPABILITY_REGISTRY,
};
use rmux_proto::{
    capabilities_for_features, HandshakeRequest, RmuxError, CAPABILITY_SDK_SESSION_LEASE_BY_ID,
    CAPABILITY_SDK_SESSION_LEASE_BY_ID_V2, CAPABILITY_WEB_SHARE, SUPPORTED_CAPABILITIES,
};

#[test]
fn registry_projects_advertisement_negotiation_and_stable_error_codes() {
    let baseline = capabilities_for_features(false);
    assert_eq!(baseline, SUPPORTED_CAPABILITIES);
    assert_eq!(
        CAPABILITY_REGISTRY
            .iter()
            .filter(|entry| entry.advertised(false))
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        baseline,
    );
    assert_eq!(
        CAPABILITY_REGISTRY
            .iter()
            .filter(|entry| entry.advertised(true))
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        capabilities_for_features(true),
    );
    let lease = capability_descriptor(CAPABILITY_SDK_SESSION_LEASE_BY_ID_V2).unwrap();
    assert_eq!(lease.lifetime, CapabilityLifetime::Connection);
    assert!(!capability_descriptor(CAPABILITY_SDK_SESSION_LEASE_BY_ID)
        .unwrap()
        .advertised(true));
    assert!(!baseline.contains(&CAPABILITY_WEB_SHARE));
    assert!(capability_descriptor("future.unknown").is_none());
    for id in [CAPABILITY_WEB_SHARE, "future.unknown"] {
        let error = resolve_capability(id, CapabilityRequirement::Required, &baseline).unwrap_err();
        assert_eq!(
            error,
            HandshakeRequest::requiring([id])
                .validate_against(&baseline)
                .unwrap_err()
        );
        assert_eq!(error.code().as_str(), "unsupported_capability");
        assert_eq!(error.code().wire_discriminant(), 13);
        let encoded = bincode::serialize(&error).unwrap();
        assert_eq!(&encoded[..4], &13_u32.to_le_bytes());
        assert_eq!(
            resolve_capability(id, CapabilityRequirement::Optional, &baseline).unwrap(),
            CapabilityAvailability::Unavailable,
        );
    }
    // Forward capabilities are accepted by exact id when the peer advertises them.
    assert_eq!(
        resolve_capability(
            "future.unknown",
            CapabilityRequirement::Required,
            &["future.unknown"]
        )
        .unwrap(),
        CapabilityAvailability::Available,
    );
    assert_eq!(
        RmuxError::ProcessStillRunning.code().wire_discriminant(),
        18
    );
}
