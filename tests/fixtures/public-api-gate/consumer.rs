fn main() {
    assert_eq!(api_canary::retained(11_u32), 11_u32);
    #[cfg(all(feature = "web", feature = "regex", not(feature = "crossterm")))]
    assert_eq!(api_canary::combination_only(), 7_u32);
}
