//! Preserve typed SDK values across removal of the raw transport variant.
//!
//! These are SDK serde indices, not protocol request tags. Keep the retired
//! index in the private identifier table so old bytes cannot change meaning.

use std::fmt;

use serde::de::{self, EnumAccess, VariantAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::RmuxCommandKind;

#[derive(Deserialize)]
#[serde(variant_identifier)]
enum KindTag {
    Request,
    NewSession,
    AttachSession,
    SplitWindow,
    RefreshClient,
}

impl Serialize for RmuxCommandKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::NewSession(spec) => {
                serializer.serialize_newtype_variant("RmuxCommandKind", 1, "NewSession", spec)
            }
            Self::AttachSession(spec) => {
                serializer.serialize_newtype_variant("RmuxCommandKind", 2, "AttachSession", spec)
            }
            Self::SplitWindow(spec) => {
                serializer.serialize_newtype_variant("RmuxCommandKind", 3, "SplitWindow", spec)
            }
            Self::RefreshClient(spec) => {
                serializer.serialize_newtype_variant("RmuxCommandKind", 4, "RefreshClient", spec)
            }
        }
    }
}

impl<'de> Deserialize<'de> for RmuxCommandKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct CommandVisitor;

        impl<'de> Visitor<'de> for CommandVisitor {
            type Value = RmuxCommandKind;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a typed SDK command")
            }

            fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<Self::Value, A::Error> {
                let (tag, payload) = data.variant::<KindTag>()?;
                match tag {
                    KindTag::Request => Err(de::Error::custom(
                        "raw Request SDK commands were removed; use typed SDK values",
                    )),
                    KindTag::NewSession => {
                        payload.newtype_variant().map(RmuxCommandKind::NewSession)
                    }
                    KindTag::AttachSession => payload
                        .newtype_variant()
                        .map(RmuxCommandKind::AttachSession),
                    KindTag::SplitWindow => {
                        payload.newtype_variant().map(RmuxCommandKind::SplitWindow)
                    }
                    KindTag::RefreshClient => payload
                        .newtype_variant()
                        .map(RmuxCommandKind::RefreshClient),
                }
            }
        }

        deserializer.deserialize_enum(
            "RmuxCommandKind",
            &[
                "Request",
                "NewSession",
                "AttachSession",
                "SplitWindow",
                "RefreshClient",
            ],
            CommandVisitor,
        )
    }
}
