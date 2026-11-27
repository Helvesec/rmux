These negative doctests require `rustdoc --test` with the built `rmux_sdk` and
`rmux_proto` crates supplied via `--extern` and their dependency search path.
Both snippets compile before the cutoff and must fail with missing trait bounds
after it. `rejected_conversions.rs` additionally checks the `rmux_types` reexport.

```compile_fail,E0277
let size = rmux_sdk::TerminalSizeSpec::new(120, 32);
let _ = <rmux_proto::TerminalSize as From<rmux_sdk::TerminalSizeSpec>>::from(size);
let _: rmux_proto::TerminalSize = size.into();
```

```compile_fail,E0277
let size = rmux_proto::TerminalSize::new(120, 32);
let _ = <rmux_sdk::TerminalSizeSpec as From<rmux_proto::TerminalSize>>::from(size);
let _: rmux_sdk::TerminalSizeSpec = size.into();
```
