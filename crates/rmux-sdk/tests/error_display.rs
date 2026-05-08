use std::error::Error as StdError;
use std::fmt::Debug;
use std::io;

use rmux_sdk::{CollectError, RmuxError};

fn assert_error_bounds_without_clone<T>()
where
    T: Debug + StdError + Send + Sync + 'static,
{
}

macro_rules! assert_not_clone {
    ($type:ty) => {
        const _: fn() = || {
            trait AmbiguousIfClone<Marker> {
                fn probe() {}
            }

            impl<T: ?Sized> AmbiguousIfClone<()> for T {}
            impl<T: ?Sized + Clone> AmbiguousIfClone<u8> for T {}

            <$type as AmbiguousIfClone<_>>::probe();
        };
    };
}

assert_not_clone!(RmuxError);
assert_not_clone!(CollectError);

#[test]
fn result_alias_uses_sdk_facade_error() {
    fn returns_alias() -> rmux_sdk::Result<u8> {
        Err(RmuxError::unsupported(
            "operation.test",
            "use a daemon that supports operation.test",
        ))
    }

    let result: core::result::Result<u8, RmuxError> = returns_alias();
    let error = result.expect_err("alias must use RmuxError as its error type");

    assert_eq!(error.feature(), Some("operation.test"));
}

#[test]
fn sdk_errors_have_required_bounds_without_clone() {
    assert_error_bounds_without_clone::<RmuxError>();
    assert_error_bounds_without_clone::<CollectError>();
}

#[test]
fn unsupported_display_is_two_visible_lines() {
    let error = RmuxError::unsupported(
        "protocol.wire_version",
        "upgrade the rmux daemon or use an SDK that supports this wire version",
    );

    assert_eq!(
        error.to_string(),
        "unsupported feature `protocol.wire_version`\n\
         hint: upgrade the rmux daemon or use an SDK that supports this wire version"
    );
    assert_eq!(error.feature(), Some("protocol.wire_version"));
    assert_eq!(
        error.hint(),
        Some("upgrade the rmux daemon or use an SDK that supports this wire version")
    );
    assert!(StdError::source(&error).is_none());
}

#[test]
fn protocol_wire_version_maps_to_sdk_guidance() {
    let error = RmuxError::from(rmux_proto::RmuxError::UnsupportedWireVersion {
        got: 7,
        minimum: 1,
        maximum: 1,
    });

    assert_eq!(
        error.to_string(),
        "unsupported feature `protocol.wire_version`\n\
         hint: upgrade the rmux daemon or use an SDK that supports wire version 7 \
         (supported range 1..=1)"
    );
    assert_eq!(error.feature(), Some("protocol.wire_version"));
    assert_eq!(
        error.hint(),
        Some(
            "upgrade the rmux daemon or use an SDK that supports wire version 7 \
             (supported range 1..=1)"
        )
    );
    assert!(
        !error.to_string().contains("unsupported RMUX wire version"),
        "SDK display should not expose raw lower-crate wire-version text"
    );
    assert!(StdError::source(&error).is_none());
}

#[test]
fn unknown_command_maps_to_unsupported_capability_guidance() {
    let error = RmuxError::from(rmux_proto::RmuxError::UnknownCommand(
        "capture-pane".to_owned(),
    ));

    assert_eq!(
        error.to_string(),
        "unsupported feature `command.capture-pane`\n\
         hint: upgrade the rmux daemon or use a command advertised by the negotiated \
         command inventory before sending `capture-pane`"
    );
    assert_eq!(error.feature(), Some("command.capture-pane"));
    assert_eq!(
        error.hint(),
        Some(
            "upgrade the rmux daemon or use a command advertised by the negotiated \
             command inventory before sending `capture-pane`"
        )
    );
    assert!(StdError::source(&error).is_none());
}

#[test]
fn protocol_error_display_wraps_source() {
    let error = RmuxError::protocol(rmux_proto::RmuxError::SessionNotFound("demo".to_owned()));

    assert_eq!(
        error.to_string(),
        "rmux protocol error: session not found: demo\n\
         hint: check the request and daemon state, then retry after correcting the request"
    );
    assert_eq!(error.feature(), None);
    assert_eq!(
        error.hint(),
        Some("check the request and daemon state, then retry after correcting the request")
    );
    assert_eq!(
        StdError::source(&error)
            .expect("protocol wrapper must expose its source")
            .to_string(),
        "session not found: demo"
    );
}

#[test]
fn transport_error_display_wraps_source() {
    let error = RmuxError::transport(
        "connect to unix socket",
        io::Error::new(io::ErrorKind::ConnectionRefused, "socket refused"),
    );

    assert_eq!(
        error.to_string(),
        "rmux transport error while connect to unix socket: socket refused\n\
         hint: verify the rmux daemon is running and the endpoint is reachable"
    );
    assert_eq!(error.feature(), None);
    assert_eq!(
        error.hint(),
        Some("verify the rmux daemon is running and the endpoint is reachable")
    );
    assert_eq!(
        StdError::source(&error)
            .expect("transport wrapper must expose its source")
            .to_string(),
        "socket refused"
    );
}

#[test]
fn collect_error_empty_display_is_stable() {
    let error = CollectError::default();

    assert_eq!(error.to_string(), "no SDK diagnostics were collected");
    assert_eq!(error.len(), 0);
    assert!(error.is_empty());
    assert!(StdError::source(&error).is_none());
}

#[test]
fn collect_error_single_display_preserves_hint() {
    let error = CollectError::new(vec![RmuxError::unsupported(
        "operation.attach",
        "start a daemon that supports attach-session",
    )]);

    assert_eq!(
        error.to_string(),
        "1 SDK diagnostic collected:\n1. unsupported feature `operation.attach`\n   hint: start a \
         daemon that supports attach-session"
    );
    assert_eq!(error.len(), 1);
    assert_eq!(
        error.errors()[0].hint(),
        Some("start a daemon that supports attach-session")
    );
    assert!(StdError::source(&error).is_none());
}

#[test]
fn collect_error_multiple_display_preserves_all_hints() {
    let error = CollectError::new(vec![
        RmuxError::unsupported(
            "operation.attach",
            "start a daemon that supports attach-session",
        ),
        RmuxError::unsupported(
            "operation.snapshot",
            "upgrade the daemon before requesting pane snapshots",
        ),
    ]);
    let rendered = error.to_string();

    assert_eq!(
        rendered,
        "2 SDK diagnostics collected:\n1. unsupported feature `operation.attach`\n   hint: start a \
         daemon that supports attach-session\n2. unsupported feature `operation.snapshot`\n   hint: \
         upgrade the daemon before requesting pane snapshots"
    );
    assert!(rendered.contains("hint: start a daemon that supports attach-session"));
    assert!(rendered.contains("hint: upgrade the daemon before requesting pane snapshots"));
}

#[test]
fn collect_error_public_constructors_preserve_individual_hints() {
    let mut error = CollectError::from_iter([
        RmuxError::unsupported(
            "operation.attach",
            "start a daemon that supports attach-session",
        ),
        RmuxError::unsupported(
            "operation.snapshot",
            "upgrade the daemon before requesting pane snapshots",
        ),
    ]);
    error.push(RmuxError::protocol(rmux_proto::RmuxError::SessionNotFound(
        "demo".to_owned(),
    )));

    assert_eq!(
        error
            .errors()
            .iter()
            .filter_map(RmuxError::hint)
            .collect::<Vec<_>>(),
        [
            "start a daemon that supports attach-session",
            "upgrade the daemon before requesting pane snapshots",
            "check the request and daemon state, then retry after correcting the request",
        ]
    );

    let rendered = error.to_string();
    assert!(rendered.contains("hint: start a daemon that supports attach-session"));
    assert!(rendered.contains("hint: upgrade the daemon before requesting pane snapshots"));
    assert!(rendered.contains(
        "hint: check the request and daemon state, then retry after correcting the request"
    ));
}

#[test]
fn rmux_error_collect_display_wraps_aggregate_source() {
    let collect = CollectError::new(vec![RmuxError::unsupported(
        "operation.snapshot",
        "upgrade the daemon before requesting pane snapshots",
    )]);
    let aggregate_display = collect.to_string();
    let error = RmuxError::from(collect);

    assert_eq!(error.to_string(), aggregate_display);
    assert_eq!(error.feature(), None);
    assert_eq!(error.hint(), None);
    assert_eq!(
        StdError::source(&error)
            .expect("aggregate wrapper must expose CollectError as its source")
            .to_string(),
        aggregate_display
    );
}
