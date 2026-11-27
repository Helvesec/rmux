// Compile separately with rustc and --extern rmux_sdk, rmux_proto, rmux_types.
// The base accepts all eight statements; the cutoff must produce only E0277.
fn main() {
    let sdk = rmux_sdk::TerminalSizeSpec::new(0, 65535);
    let proto = rmux_proto::TerminalSize::new(0, 65535);
    let domain = rmux_types::TerminalSize::new(0, 65535);
    let _ = <rmux_proto::TerminalSize as From<rmux_sdk::TerminalSizeSpec>>::from(sdk);
    let _: rmux_proto::TerminalSize = sdk.into();
    let _ = <rmux_types::TerminalSize as From<rmux_sdk::TerminalSizeSpec>>::from(sdk);
    let _: rmux_types::TerminalSize = sdk.into();
    let _ = <rmux_sdk::TerminalSizeSpec as From<rmux_proto::TerminalSize>>::from(proto);
    let _: rmux_sdk::TerminalSizeSpec = proto.into();
    let _ = <rmux_sdk::TerminalSizeSpec as From<rmux_types::TerminalSize>>::from(domain);
    let _: rmux_sdk::TerminalSizeSpec = domain.into();
}
