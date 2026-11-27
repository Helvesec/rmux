const KITTY_GRAPHICS_APC_START: &[u8] = b"\x1b_G";
const STRING_TERMINATOR: &[u8] = b"\x1b\\";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KittyGraphicsApcDecode {
    NotKittyGraphics,
    Partial,
    Matched { size: usize },
}

#[cfg(test)]
pub(super) fn decode_kitty_graphics_apc(input: &[u8]) -> KittyGraphicsApcDecode {
    decode_kitty_graphics_apc_after_append(input, 0)
}

pub(super) fn decode_kitty_graphics_apc_after_append(
    input: &[u8],
    new_input_at: usize,
) -> KittyGraphicsApcDecode {
    if !input.is_empty()
        && input.len() < KITTY_GRAPHICS_APC_START.len()
        && KITTY_GRAPHICS_APC_START.starts_with(input)
    {
        return KittyGraphicsApcDecode::Partial;
    }

    if input.starts_with(KITTY_GRAPHICS_APC_START) {
        // Bytes before the delimiter overlap were already searched while this
        // partial APC was retained.
        let search_at = new_input_at
            .saturating_sub(STRING_TERMINATOR.len() - 1)
            .max(KITTY_GRAPHICS_APC_START.len())
            .min(input.len());
        if let Some(end_offset) = find_subslice(&input[search_at..], STRING_TERMINATOR) {
            return KittyGraphicsApcDecode::Matched {
                size: search_at + end_offset + STRING_TERMINATOR.len(),
            };
        }
        return KittyGraphicsApcDecode::Partial;
    }

    KittyGraphicsApcDecode::NotKittyGraphics
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::{
        decode_kitty_graphics_apc, decode_kitty_graphics_apc_after_append, KittyGraphicsApcDecode,
        KITTY_GRAPHICS_APC_START,
    };

    #[test]
    fn matches_through_string_terminator() {
        assert_eq!(
            decode_kitty_graphics_apc(b"\x1b_Gi=1;OK\x1b\\tail"),
            KittyGraphicsApcDecode::Matched { size: 11 }
        );
    }

    #[test]
    fn retains_complete_start_without_terminator() {
        assert_eq!(
            decode_kitty_graphics_apc(b"\x1b_Gi=1;OK"),
            KittyGraphicsApcDecode::Partial
        );
    }

    #[test]
    fn incremental_search_finds_string_terminator_split_at_append_boundary() {
        let input = b"\x1b_Gi=1;OK\x1b\\tail";
        let terminator_at = b"\x1b_Gi=1;OK".len();
        assert_eq!(
            decode_kitty_graphics_apc_after_append(input, terminator_at + 1),
            KittyGraphicsApcDecode::Matched {
                size: terminator_at + b"\x1b\\".len()
            }
        );
    }

    #[test]
    fn incremental_search_starts_at_the_append_boundary_overlap() {
        // An earlier terminator cannot exist in retained state; use it as a
        // sentinel that fails if the decoder rescans before the overlap.
        let input = b"\x1b_Gold\x1b\\padding-padding\x1b\\tail";
        let new_input_at = b"\x1b_Gold\x1b\\padding-padding".len();
        assert_eq!(
            decode_kitty_graphics_apc_after_append(input, new_input_at),
            KittyGraphicsApcDecode::Matched {
                size: new_input_at + b"\x1b\\".len()
            }
        );
    }

    #[test]
    fn retains_ambiguous_meta_underscore_prefix() {
        assert_eq!(
            decode_kitty_graphics_apc(b"\x1b_"),
            KittyGraphicsApcDecode::Partial
        );
    }

    #[test]
    fn retains_every_fragmented_start_prefix() {
        for split in 1..KITTY_GRAPHICS_APC_START.len() {
            assert_eq!(
                decode_kitty_graphics_apc(&KITTY_GRAPHICS_APC_START[..split]),
                KittyGraphicsApcDecode::Partial,
                "Kitty APC start split at byte {split}",
            );
        }
    }

    #[test]
    fn ignores_non_kitty_apc_payloads() {
        assert_eq!(
            decode_kitty_graphics_apc(b"\x1b_title\x1b\\"),
            KittyGraphicsApcDecode::NotKittyGraphics
        );
    }
}
