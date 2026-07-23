use std::future::Future;
use std::io;
use std::time::Duration;

use tokio::time::timeout;

const WEB_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) async fn write_with_timeout<F>(operation: F) -> io::Result<()>
where
    F: Future<Output = io::Result<()>>,
{
    match timeout(WEB_WRITE_TIMEOUT, operation).await {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "web-share client write timed out",
        )),
    }
}
