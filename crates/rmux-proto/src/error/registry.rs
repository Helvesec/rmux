//! Generates the wire enum and its non-wire code projection from the same entries.

macro_rules! define_errors {
    ($( $(#[doc = $doc:literal])+ $(#[error($($message:tt)*)])?
        $variant:ident [$($body:tt)*] => $pattern:pat, $id:literal;
    )*) => {
        /// Shared protocol and validation errors. Entry order is the bincode ABI.
        #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, thiserror::Error)]
        pub enum RmuxError {
            $( $(#[doc = $doc])+ $(#[error($($message)*)])? $variant $($body)*, )*
        }

        /// Payload-free local error identity. This projection adds no wire fields.
        /// Its ordinal is the existing bincode enum discriminant.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(u32)]
        pub enum ErrorCode {
            $( $(#[doc = $doc])+ $variant, )*
        }

        /// Description of a published error variant, projected from its definition.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct ErrorDescriptor {
            /// Typed identity and existing bincode discriminant.
            pub code: ErrorCode,
            /// Meaning of the error (its display message may include a payload).
            pub description: &'static str,
        }

        /// Error variants in their published bincode order.
        pub const ERROR_REGISTRY: &[ErrorDescriptor] = &[
            $(ErrorDescriptor { code: ErrorCode::$variant, description: concat!($($doc, " "),+) },)*
        ];

        impl ErrorCode {
            /// Stable diagnostic name; not a replacement for a serialized error DTO.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $id,)* }
            }

            /// Existing bincode enum tag. Append new variants; never reorder entries.
            #[must_use]
            pub const fn wire_discriminant(self) -> u32 { self as u32 }
        }

        impl RmuxError {
            /// Extracts the error identity without formatting or inspecting its message.
            #[must_use]
            pub const fn code(&self) -> ErrorCode {
                match self { $($pattern => ErrorCode::$variant,)* }
            }
        }
    };
}

pub(super) use define_errors;
