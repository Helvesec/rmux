//! Capability identities and negotiation policy. These types are not wire DTOs.

use crate::RmuxError;

/// Peer that announces support (distinct from the direction of feature traffic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityAdvertiser {
    /// Daemon handshake or the binary's local capability inventory.
    Daemon,
    /// Daemon handshake, then client opt-in on the attach request.
    DaemonAndAttachClient,
}

/// Direction of the traffic enabled by a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityDirection {
    /// Requests with responses, or streams carrying traffic in both directions.
    Bidirectional,
    /// Client-originated attach-stream messages.
    ClientToServer,
    /// Server-originated errors, render frames or event streams.
    ServerToClient,
}

/// Operation scope, not the lifetime of a pane, session or lease resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityScope {
    /// Detached protocol framing and handshake.
    Protocol,
    /// Daemon inspection and lifecycle.
    Daemon,
    /// An attached terminal stream.
    Attach,
    /// A control-mode stream.
    Control,
    /// Session operations.
    Session,
    /// Window operations.
    Window,
    /// Pane operations and subscriptions.
    Pane,
    /// Command execution and canonicalization.
    Command,
    /// Browser sharing.
    Web,
}

/// Lifetime of negotiated permission, not the advertised feature's resource TTL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityLifetime {
    /// Inventory describes this daemon build; clients discard it on reconnect.
    Advertisement,
    /// Explicit handshake opt-in is remembered only on that connection.
    Connection,
    /// Opt-in is carried by the individual request.
    Request,
    /// Opt-in lasts for the upgraded attach stream.
    AttachStream,
}

/// How a caller enables the feature after discovering the daemon's inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityNegotiation {
    /// Advertisement is sufficient; the caller can require or optionally probe it.
    Advertised,
    /// Must appear in a handshake's required list on this connection.
    RequiredOnConnection,
    /// Must appear in the handshake carried inside the execution request.
    RequiredPerRequest,
    /// Must be echoed by the client in its attach request.
    AttachOptIn,
}

/// Existing optional behavior when a capability is absent. No fallback is sent automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityFallback {
    /// No generic fallback; refuse the feature or let its caller handle absence.
    Unavailable,
    /// Resize using terminal cells without pixel dimensions.
    CellResize,
    /// Consume the legacy byte-output attach stream.
    RawAttachOutput,
    /// Encode Windows input through the ordinary key path.
    EncodedKeys,
    /// Use the existing command/source-file path without the canonicalization RPC.
    LegacyCommandPath,
    /// Inspect the session list when daemon status metadata is unavailable.
    LegacySessionCount,
    /// Slot-based snapshot handles can use the legacy endpoint; stable handles cannot.
    SlotSnapshot,
    /// Automatic SDK broadcast can fan out individual pane requests.
    ClientBroadcast,
    /// CLI all-session window listing can query each session separately.
    PerSessionWindowList,
}

/// Whether this particular caller requires the feature or only probes it.
/// Required absence always fails, including for a known optional feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityRequirement {
    /// Refuse before issuing a request needing the missing feature.
    Required,
    /// Let the caller decide whether to use the recorded fallback.
    Optional,
}

/// Result of exact-id capability discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityAvailability {
    /// The peer explicitly advertised the requested id, even if locally unknown.
    Available,
    /// The id is absent and has no registered fallback (including unknown ids).
    Unavailable,
    /// The id is absent; this existing fallback remains a caller decision.
    Fallback(CapabilityFallback),
}

/// Build-dependent advertisement. Retired identities remain known, never advertised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityAdvertisement {
    /// Present in every build's baseline inventory.
    Baseline,
    /// Present only when browser sharing is compiled in.
    Web,
    /// Historical identity superseded by a different, explicitly negotiated id.
    Retired,
}

/// One published capability and its local negotiation semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityDescriptor {
    /// Published id; never normalized or matched by prefix.
    pub id: &'static str,
    /// Human-readable description used by generated documentation.
    pub description: &'static str,
    /// Peer announcing support.
    pub advertiser: CapabilityAdvertiser,
    /// Direction of enabled traffic.
    pub direction: CapabilityDirection,
    /// Scope of enabled operations.
    pub scope: CapabilityScope,
    /// Lifetime of negotiated permission.
    pub lifetime: CapabilityLifetime,
    /// Required opt-in beyond inventory discovery, if any.
    pub negotiation: CapabilityNegotiation,
    /// Optional absence behavior; required absence always returns an error.
    pub fallback: CapabilityFallback,
    /// Build-dependent announcement policy.
    pub advertisement: CapabilityAdvertisement,
}

impl CapabilityDescriptor {
    /// Whether this descriptor belongs in the supplied build's inventory.
    #[must_use]
    pub const fn advertised(&self, web_share: bool) -> bool {
        match self.advertisement {
            CapabilityAdvertisement::Baseline => true,
            CapabilityAdvertisement::Web => web_share,
            CapabilityAdvertisement::Retired => false,
        }
    }
}

/// Looks up a locally known id. Unknown ids are preserved by negotiation.
#[must_use]
pub fn capability_descriptor(id: &str) -> Option<&'static CapabilityDescriptor> {
    CAPABILITY_REGISTRY.iter().find(|entry| entry.id == id)
}

/// Resolves an id by exact peer advertisement, with the registry's optional fallback.
/// An unknown advertised id is accepted for forward compatibility. An unknown
/// missing id is unavailable when optional, and a typed error when required.
pub fn resolve_capability<S: AsRef<str>>(
    id: &str,
    requirement: CapabilityRequirement,
    supported: &[S],
) -> Result<CapabilityAvailability, RmuxError> {
    let availability = capability_availability(id, supported);
    if requirement == CapabilityRequirement::Required
        && availability != CapabilityAvailability::Available
    {
        return Err(RmuxError::UnsupportedCapability {
            feature: id.to_owned(),
            supported: supported
                .iter()
                .map(|entry| entry.as_ref().to_owned())
                .collect(),
        });
    }
    Ok(availability)
}

/// Non-erroring optional probe shared by clients and the SDK.
#[must_use]
pub fn capability_availability<S: AsRef<str>>(id: &str, supported: &[S]) -> CapabilityAvailability {
    if supported.iter().any(|entry| entry.as_ref() == id) {
        return CapabilityAvailability::Available;
    }
    match capability_descriptor(id).map(|entry| entry.fallback) {
        None | Some(CapabilityFallback::Unavailable) => CapabilityAvailability::Unavailable,
        Some(fallback) => CapabilityAvailability::Fallback(fallback),
    }
}

macro_rules! capabilities {
    ($(
        $advertisement:ident { $(
            $(#[doc = $doc:literal])+ $constant:ident = $id:literal;
            ($advertiser:ident, $direction:ident, $scope:ident, $lifetime:ident,
             $negotiation:ident, $fallback:ident);
        )* }
    )*) => {
        $($( $(#[doc = $doc])+ pub const $constant: &str = $id; )*)*
        /// Complete registry in published advertisement order, followed by retired ids.
        pub const CAPABILITY_REGISTRY: &[CapabilityDescriptor] = &[
            $($(CapabilityDescriptor {
                id: $constant, description: concat!($($doc, " "),+),
                advertiser: CapabilityAdvertiser::$advertiser,
                direction: CapabilityDirection::$direction,
                scope: CapabilityScope::$scope, lifetime: CapabilityLifetime::$lifetime,
                negotiation: CapabilityNegotiation::$negotiation,
                fallback: CapabilityFallback::$fallback,
                advertisement: CapabilityAdvertisement::$advertisement,
            },)*)*
        ];
    };
}

capabilities! {
    Baseline {
        /// Versioned authoritative window scene snapshots.
        CAPABILITY_SCENE_V1 = "scene.window.v1";
        (Daemon, Bidirectional, Window, Advertisement, Advertised, Unavailable);
        /// Stable feature id for the detached bincode RPC transport.
        CAPABILITY_DETACHED_RPC = "rpc.detached";
        (Daemon, Bidirectional, Protocol, Advertisement, Advertised, Unavailable);
        /// Stable feature id for the capabilities handshake request.
        CAPABILITY_HANDSHAKE = "protocol.capabilities";
        (Daemon, Bidirectional, Protocol, Advertisement, Advertised, Unavailable);
        /// Stable feature id for framed protocol errors returned as `Response::Error`.
        CAPABILITY_FRAMED_ERRORS = "protocol.framed_errors";
        (Daemon, ServerToClient, Protocol, Advertisement, Advertised, Unavailable);
        /// Stable feature id for `attach-session` framed-to-raw stream upgrades.
        CAPABILITY_ATTACH_STREAM = "stream.attach";
        (Daemon, Bidirectional, Attach, Advertisement, Advertised, Unavailable);
        /// Stable feature id for attach-stream resize messages that carry pixel geometry.
        CAPABILITY_ATTACH_RESIZE_GEOMETRY = "stream.attach.resize_geometry";
        (Daemon, ClientToServer, Attach, Advertisement, Advertised, CellResize);
        /// Stable feature id for coalescible attach-stream render messages.
        CAPABILITY_ATTACH_RENDER = "stream.attach.render";
        (DaemonAndAttachClient, ServerToClient, Attach, AttachStream, AttachOptIn, RawAttachOutput);
        /// Stable feature id for attach-stream Windows console key messages.
        CAPABILITY_ATTACH_WINDOWS_CONSOLE_KEY = "stream.attach.windows_console_key";
        (DaemonAndAttachClient, ClientToServer, Attach, AttachStream, AttachOptIn, EncodedKeys);
        /// Stable feature id for control-mode framed-to-raw stream upgrades.
        CAPABILITY_CONTROL_STREAM = "stream.control";
        (Daemon, Bidirectional, Control, Advertisement, Advertised, Unavailable);
        /// Stable feature id for daemon shutdown over detached RPC.
        CAPABILITY_DAEMON_SHUTDOWN = "daemon.shutdown";
        (Daemon, Bidirectional, Daemon, Advertisement, Advertised, Unavailable);
        /// Stable feature id for daemon version and activity metadata.
        CAPABILITY_DAEMON_STATUS = "daemon.status";
        (Daemon, Bidirectional, Daemon, Advertisement, Advertised, LegacySessionCount);
        /// Stable feature id for safe idle-daemon shutdown during client upgrades.
        CAPABILITY_DAEMON_SHUTDOWN_IF_IDLE = "daemon.shutdown_if_idle";
        (Daemon, Bidirectional, Daemon, Advertisement, Advertised, Unavailable);
        /// Stable feature id for daemon-backed SDK waits and cancellation.
        CAPABILITY_SDK_WAITS = "sdk.waits";
        (Daemon, Bidirectional, Pane, Advertisement, Advertised, Unavailable);
        /// Stable feature id for two-phase SDK waits that acknowledge the armed state.
        CAPABILITY_SDK_WAITS_ARMED = "sdk.waits.armed";
        (Daemon, Bidirectional, Pane, Connection, RequiredOnConnection, Unavailable);
        /// Stable feature id for SDK pane operations that target stable pane ids.
        CAPABILITY_SDK_PANE_BY_ID = "sdk.pane.by_id";
        (Daemon, Bidirectional, Pane, Advertisement, Advertised, SlotSnapshot);
        /// Stable feature id for daemon-side SDK pane input broadcast.
        CAPABILITY_SDK_PANE_BROADCAST = "sdk.pane.broadcast";
        (Daemon, Bidirectional, Pane, Advertisement, Advertised, ClientBroadcast);
        /// Stable feature id for SDK pane-local option access.
        CAPABILITY_SDK_PANE_OPTIONS = "sdk.pane.options";
        (Daemon, Bidirectional, Pane, Advertisement, Advertised, Unavailable);
        /// Stable feature id for SDK pane title/option/close state events.
        CAPABILITY_SDK_PANE_STATE_EVENTS = "sdk.pane.state_events";
        (Daemon, ServerToClient, Pane, Advertisement, Advertised, Unavailable);
        /// Stable feature id for SDK best-effort pane foreground process state.
        CAPABILITY_SDK_PANE_FOREGROUND = "sdk.pane.foreground";
        (Daemon, Bidirectional, Pane, Advertisement, Advertised, Unavailable);
        /// Stable feature id for daemon-side app-owned session leases.
        CAPABILITY_SDK_SESSION_LEASE = "sdk.session.lease";
        (Daemon, Bidirectional, Session, Advertisement, Advertised, Unavailable);
        /// Stable feature id for connection-negotiated app-owned lease requests addressed by session id.
        CAPABILITY_SDK_SESSION_LEASE_BY_ID_V2 = "sdk.session.lease.by_id.v2";
        (Daemon, Bidirectional, Session, Connection, RequiredOnConnection, Unavailable);
        /// Stable feature id for owned-session creation that returns a stable session identity.
        CAPABILITY_SDK_OWNED_SESSION_STABLE_IDENTITY = "sdk.owned_session.stable_identity";
        (Daemon, Bidirectional, Session, Advertisement, Advertised, Unavailable);
        /// Stable feature id for explicit SDK process launch modes.
        CAPABILITY_SDK_PROCESS_COMMAND = "sdk.process.command";
        (Daemon, Bidirectional, Pane, Advertisement, Advertised, Unavailable);
        /// Stable feature id for target-client aware command request variants.
        CAPABILITY_TARGET_CLIENT_COMMANDS = "commands.target_client";
        (Daemon, Bidirectional, Command, Advertisement, Advertised, Unavailable);
        /// Stable feature id for CLI pane commands with daemon-side target resolution.
        CAPABILITY_CLI_TARGET_ACTIONS = "commands.cli_target_actions";
        (Daemon, Bidirectional, Command, Advertisement, Advertised, Unavailable);
        /// Stable feature id for `capture-pane` with daemon-side target resolution.
        CAPABILITY_CLI_CAPTURE_TARGET_ACTION = "commands.cli_capture_target_action";
        (Daemon, Bidirectional, Command, Advertisement, Advertised, Unavailable);
        /// Stable feature id for non-executing server-side CLI alias canonicalization.
        CAPABILITY_CLI_RUNTIME_COMMAND_EXPANSION = "commands.cli_runtime_command_expansion";
        (Daemon, Bidirectional, Command, Advertisement, Advertised, LegacyCommandPath);
        /// Stable feature id for negotiated server-side CLI argv execution.
        CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION = "commands.cli_runtime_command_execution.v1";
        (Daemon, Bidirectional, Command, Request, RequiredPerRequest, Unavailable);
        /// Explicit floating pane creation in the existing runtime execution envelope.
        CAPABILITY_NEW_PANE_FLOATING_EXPLICIT_V1 = "commands.new_pane.floating_explicit.v1";
        (Daemon, Bidirectional, Command, Request, RequiredPerRequest, Unavailable);
        /// Absolute floating pane resize through the existing resize-pane request.
        CAPABILITY_RESIZE_PANE_FLOATING_ABSOLUTE_V1 = "commands.resize_pane.floating_absolute.v1";
        (Daemon, Bidirectional, Command, Advertisement, Advertised, Unavailable);
        /// Absolute floating pane translation in the runtime execution envelope.
        CAPABILITY_RESIZE_PANE_POSITION_V1 = "commands.resize_pane.position.v1";
        (Daemon, Bidirectional, Command, Request, RequiredPerRequest, Unavailable);
        /// Existing-pane tiled/floating conversion in the runtime execution envelope.
        CAPABILITY_SET_PANE_PLACEMENT_V1 = "commands.set_pane_placement.v1";
        (Daemon, Bidirectional, Command, Request, RequiredPerRequest, Unavailable);
        /// Atomic command split/key with an independently framed binary stdin payload.
        CAPABILITY_SPLIT_WINDOW_KEY_INPUT_V1 = "commands.split_window.key_input.v1";
        (Daemon, Bidirectional, Command, Request, RequiredPerRequest, Unavailable);
        /// `new-pane -I`: a finished stdin payload framed beside its own command.
        CAPABILITY_NEW_PANE_INPUT_V1 = "commands.new_pane.input.v1";
        (Daemon, Bidirectional, Command, Request, RequiredPerRequest, Unavailable);
        /// File-backed source execution carrying a separate finished binary input.
        CAPABILITY_SOURCE_FILE_INPUT_V1 = "commands.source_file.input.v1";
        (Daemon, Bidirectional, Command, Request, RequiredPerRequest, Unavailable);
        /// Explicit source input result distinguishing outer read failure from sourced errors.
        CAPABILITY_SOURCE_FILE_QUEUE_STATUS_V1 = "commands.source_file.queue_status.v1";
        (Daemon, Bidirectional, Command, Request, RequiredPerRequest, Unavailable);
        /// Stable feature id for daemon-owned `list-windows -a` queue execution.
        CAPABILITY_CLI_LIST_WINDOWS_ALL_QUEUE = "commands.cli_list_windows_all_queue";
        (Daemon, Bidirectional, Command, Advertisement, Advertised, PerSessionWindowList);
        /// Stable feature id for explicit RMUX window snapshot export and restore.
        CAPABILITY_RMUX_WINDOW_SNAPSHOT_V1 = "commands.window_snapshot.v1";
        (Daemon, Bidirectional, Window, Advertisement, Advertised, Unavailable);
        /// Stable feature id for atomic SDK split visible/stable identity responses.
        CAPABILITY_SDK_PANE_SPLIT_IDENTITY = "sdk.pane.split_identity";
        (Daemon, Bidirectional, Pane, Advertisement, Advertised, Unavailable);
        /// Stable feature id for recoverable raw pane-output streams.
        CAPABILITY_SDK_PANE_RAW_RECOVERY = "sdk.pane.raw_recovery";
        (Daemon, ServerToClient, Pane, Advertisement, Advertised, Unavailable);
        /// Stable feature id for authoritative pane-surface streams.
        CAPABILITY_SDK_PANE_SURFACE_STREAM = "sdk.pane.surface_stream";
        (Daemon, ServerToClient, Pane, Advertisement, Advertised, Unavailable);
        /// Stable feature id for the typed attach-stream server-exit notice.
        CAPABILITY_ATTACH_SERVER_EXITED = "stream.attach.server_exited";
        (DaemonAndAttachClient, ServerToClient, Attach, AttachStream, AttachOptIn, RawAttachOutput);
    }
    Web {
        /// Stable feature id for browser-visible pane sharing.
        ///
        /// This optional capability is advertised by daemons compiled with their web
        /// listener enabled rather than by the protocol baseline capability list.
        CAPABILITY_WEB_SHARE = "web.share";
        (Daemon, Bidirectional, Web, Advertisement, Advertised, Unavailable);
    }
    Retired {
        /// Stable feature id for app-owned session lease requests addressed by session id.
        CAPABILITY_SDK_SESSION_LEASE_BY_ID = "sdk.session.lease.by_id";
        (Daemon, Bidirectional, Session, Advertisement, Advertised, Unavailable);
    }
}

// A const projection keeps the historical public slice type and its exact order.
const BASELINE_COUNT: usize = {
    let mut count = 0;
    let mut index = 0;
    while index < CAPABILITY_REGISTRY.len() {
        if CAPABILITY_REGISTRY[index].advertised(false) {
            count += 1;
        }
        index += 1;
    }
    count
};

const BASELINE_IDS: [&str; BASELINE_COUNT] = {
    let mut ids = [""; BASELINE_COUNT];
    let mut input = 0;
    let mut output = 0;
    while input < CAPABILITY_REGISTRY.len() {
        if CAPABILITY_REGISTRY[input].advertised(false) {
            ids[output] = CAPABILITY_REGISTRY[input].id;
            output += 1;
        }
        input += 1;
    }
    ids
};

/// Capabilities advertised by this protocol build, derived in registry order.
pub const SUPPORTED_CAPABILITIES: &[&str] = &BASELINE_IDS;
