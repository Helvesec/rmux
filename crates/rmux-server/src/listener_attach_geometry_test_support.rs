use super::connection_test_support::{
    connected_streams, finish_connection, read_response_leaving_raw_bytes, spawn_connection,
    write_test_request, TestClientStream,
};
use super::*;

pub(crate) struct AttachedConnection {
    client: TestClientStream,
    shutdown: watch::Sender<()>,
    task: tokio::task::JoinHandle<io::Result<()>>,
}

impl AttachedConnection {
    pub(crate) async fn finish(self) -> io::Result<()> {
        drop(self.client);
        let _ = self.shutdown.send(());
        tokio::time::timeout(Duration::from_secs(5), finish_connection(self.task))
            .await
            .expect("attach connection exits")
    }
}

pub(crate) async fn attach(
    handler: &RequestHandler,
    request: Request,
) -> io::Result<(Response, AttachedConnection)> {
    let handler = Arc::new(handler.clone());
    let uid = crate::server_access::current_owner_uid().saturating_add(18_230);
    handler
        .set_test_access_mode_for_uid(uid, crate::server_access::AccessMode::ReadWrite)
        .expect("geometry peer has write access");
    let peer = PeerIdentity {
        pid: std::process::id().wrapping_add(18_230),
        uid,
        user: rmux_os::identity::UserIdentity::Uid(uid),
    };
    let (server, mut client) = connected_streams("attach-geometry").await?;
    let (shutdown, task) = spawn_connection(&handler, peer, server);
    write_test_request(&mut client, request).await?;
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        read_response_leaving_raw_bytes(&mut client),
    )
    .await
    .expect("attach response arrives")?;
    Ok((
        response,
        AttachedConnection {
            client,
            shutdown,
            task,
        },
    ))
}
