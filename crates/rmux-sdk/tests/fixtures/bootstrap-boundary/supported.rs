use rmux_sdk::{Rmux, RmuxBuilder, RmuxEndpoint, RmuxError};
use std::future::Future;
use std::time::Duration;

fn handle_future(_: impl Future<Output = Result<Rmux, RmuxError>>) {}

fn main() {
    let endpoint = RmuxEndpoint::UnixSocket("explicit.sock".into());
    let builder = RmuxBuilder::new()
        .endpoint(endpoint.clone())
        .default_timeout(Duration::MAX);
    assert_eq!(builder.resolved_endpoint().unwrap(), endpoint);
    assert_eq!(builder.resolved_timeout(None), None);
    assert_eq!(
        builder.resolved_timeout(Some(Duration::ZERO)),
        Some(Duration::ZERO)
    );
    handle_future(builder.connect_or_start());
    handle_future(Rmux::builder().connect_or_start());
    handle_future(Rmux::connect_or_start());
    handle_future(Rmux::connect_or_start_at(endpoint));
}
