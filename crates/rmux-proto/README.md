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

The crate currently emits detached RPC wire version 9. It also ships the
`V1_FRAME_LEDGER`, the first stable ledger of frame-kind IDs and bincode
tags. Breaking wire changes bump the envelope varint; compatible DTO
additions append ledger entries rather than mutating existing frame IDs.

The RMUX 0.10 release line uses an exact envelope hard-cut: a decoder accepts
only `RMUX_WIRE_VERSION` and rejects older or newer detached frames before any
DTO-level handshake is decoded. `HandshakeRequest` min/max wire fields are
therefore advisory after the current envelope has decoded, while
`required_capabilities` are mandatory feature gates.

The [capability and error reference](REGISTRY.md) is generated from the typed
registries with `cargo run -p rmux-proto --example protocol_registry`. The same
capability entries produce the baseline/feature inventories and negotiation
policy; error entries produce both `RmuxError` and its payload-free `ErrorCode`.
Neither registry adds fields or changes the published wire format.

## File-backed source input (C2S140)

RMUX 0.11.0, wire version 9, adds the client → server message
`SourceFileInputV1Request` (C2S140, bincode tag 140). Each request requires
`commands.source_file.input.v1`, `commands.cli_runtime_command_execution.v1`
and `commands.new_pane.floating_explicit.v1` in its embedded handshake.

The request runs a file-backed `source-file` invocation with original arguments,
caller working directory and a separate finished binary input channel. Nested
sources share that channel; `new-pane -I` consumes it once, after successful
creation. A failed creation leaves it available. An empty payload is EOF;
an absent channel remains distinct. Payload bytes are never parsed as commands.
The response is the existing `SourceFileResponse` or a terminal error.

Without `commands.source_file.input.v1`, CLI discovery does not send C2S140:
the ordinary source route remains, without a binary input channel, and
`new-pane -I` refuses missing input. A sent offer requires every listed
capability; an unsupported requirement returns `UnsupportedCapability` before
execution. Refusal, completion and transport failure never trigger replay.
`source-file -` still reads source code from stdin, and `source-file -n`
remains parse-only. Existing DTOs, V1 payloads and ledger entries keep their
meaning and tags. Wire-8 peers are rejected at the envelope boundary; this
additive wire-9 message requires no additional daemon restart.

## Surface

- `RMUX_FRAME_MAGIC = 0x52`, `RMUX_WIRE_VERSION = 9`, `V1_FRAME_LEDGER`.
- `encode_frame`, `decode_frame`, `FrameDecoder`.
- Request, response, attach, control, capability DTOs.
- `PaneId`, `SessionId`, `SessionName`, `WindowId` identity types.
- `RmuxError` — wire-safe error type.

`rmux-proto` is the source of truth for the RMUX wire format. Anything
that needs to encode or decode RMUX frames depends on it directly.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
