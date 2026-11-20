//! Identity vocabulary surface used by `rmux-core` consumers.
//!
//! The canonical identity newtypes (`SessionName`, `SessionId`,
//! `WindowId`, `PaneId`) are defined exactly once in `rmux-types`. This
//! module re-exports those types so `rmux-core` consumers can address
//! them through a single core-side surface regardless of which crate
//! originally introduced the value. Allocation, lookup, and resolution
//! remain in `rmux-core::session`; nothing in this module mutates
//! identity state.

pub use rmux_types::{PaneId, SessionId, SessionName, WindowId};

#[cfg(test)]
mod tests {
    use super::{PaneId, SessionId, SessionName, WindowId};

    #[test]
    fn core_identity_values_are_owned_by_rmux_types() {
        let name: rmux_types::SessionName = SessionName::new("a\\n:b.c").unwrap();
        let core: crate::SessionName = name;
        let proto: rmux_proto::SessionName = core;
        assert_eq!(proto.as_str(), "a\\\\n_b_c");
        let session: rmux_types::SessionId = crate::SessionId::new(u32::MAX);
        let window: rmux_types::WindowId = crate::WindowId::new(0);
        let pane: rmux_types::PaneId = crate::pane::PaneId::new(u32::MAX);
        let _: (
            rmux_proto::SessionId,
            rmux_proto::WindowId,
            rmux_proto::PaneId,
        ) = (session, window, pane);
    }

    #[test]
    fn core_identity_re_export_matches_proto_definition() {
        let proto_pane: rmux_proto::PaneId = PaneId::new(7);
        assert_eq!(proto_pane.as_u32(), 7);
        assert_eq!(proto_pane.to_string(), "%7");

        let proto_window: rmux_proto::WindowId = WindowId::new(3);
        assert_eq!(proto_window.to_string(), "@3");

        let proto_session: rmux_proto::SessionId = SessionId::new(2);
        assert_eq!(proto_session.to_string(), "$2");

        let name: rmux_proto::SessionName = SessionName::new("alpha").expect("valid");
        assert_eq!(name.as_str(), "alpha");
    }

    #[test]
    fn core_identity_re_exports_match_pane_module_re_export() {
        assert_eq!(
            std::any::TypeId::of::<crate::PaneId>(),
            std::any::TypeId::of::<PaneId>(),
            "core::PaneId from pane.rs and core::identity::PaneId must converge to one type",
        );
        assert_eq!(
            std::any::TypeId::of::<crate::WindowId>(),
            std::any::TypeId::of::<WindowId>(),
            "core::WindowId and core::identity::WindowId must converge to one type",
        );
        assert_eq!(
            std::any::TypeId::of::<crate::SessionId>(),
            std::any::TypeId::of::<SessionId>(),
            "core::SessionId and core::identity::SessionId must converge to one type",
        );
        assert_eq!(
            std::any::TypeId::of::<rmux_proto::PaneId>(),
            std::any::TypeId::of::<crate::PaneId>(),
            "core re-exports must resolve to rmux_proto::PaneId",
        );
        assert_eq!(
            std::any::TypeId::of::<rmux_proto::WindowId>(),
            std::any::TypeId::of::<crate::WindowId>(),
            "core re-exports must resolve to rmux_proto::WindowId",
        );
        assert_eq!(
            std::any::TypeId::of::<rmux_proto::SessionId>(),
            std::any::TypeId::of::<crate::SessionId>(),
            "core re-exports must resolve to rmux_proto::SessionId",
        );
        assert_eq!(
            std::any::TypeId::of::<rmux_proto::SessionName>(),
            std::any::TypeId::of::<crate::SessionName>(),
            "core re-exports must resolve to rmux_proto::SessionName",
        );
    }
}
