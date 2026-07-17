# rmux-proto

Detached IPC protocol DTOs, framing, and wire-safe errors for the
[RMUX](https://github.com/Helvesec/rmux) terminal multiplexer.

Defines the local wire protocol RMUX clients use to talk to the daemon.
All DTOs are platform-neutral, bincode-encoded, and framed by a single
envelope:

```
magic byte      0x52
wire version    varint (LEB128)
payload length  little-endian u32
payload         bincode v1 DTO
```

The crate currently emits detached RPC wire version 5. It also ships the
`V1_FRAME_LEDGER`, the first stable ledger of frame-kind IDs and bincode
tags. Breaking wire changes bump the envelope varint; compatible DTO
additions append ledger entries rather than mutating existing frame IDs.

RMUX 0.9.0 uses an exact envelope hard-cut: a decoder accepts only
`RMUX_WIRE_VERSION` and rejects older or newer detached frames before any
DTO-level handshake is decoded. `HandshakeRequest` min/max wire fields are
therefore advisory after the current envelope has decoded, while
`required_capabilities` are mandatory feature gates.

## Surface

- `RMUX_FRAME_MAGIC = 0x52`, `RMUX_WIRE_VERSION = 5`, `V1_FRAME_LEDGER`.
- `encode_frame`, `decode_frame`, `FrameDecoder`.
- Request, response, attach, control, capability DTOs.
- `PaneId`, `SessionId`, `SessionName`, `WindowId` identity types.
- `RmuxError` — wire-safe error type.

`rmux-proto` is the source of truth for the RMUX wire format. Anything
that needs to encode or decode RMUX frames depends on it directly.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
