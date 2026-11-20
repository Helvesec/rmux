use super::*;
use crate::daemon::ShutdownHandle;
use crate::server_access::current_owner_uid;
use crate::web::WebShareSettings;
use rmux_core::events::SubscriptionLimits;
use rmux_proto::{Request, Response, WebShareConfigRequest, WebShareRequest, WebShareResponse};
use tokio::io::AsyncWriteExt;
use tokio::time::timeout;

const RETRY_WINDOW: Duration = Duration::from_millis(150);

#[tokio::test]
async fn abandoned_startup_retry_preserves_web_listener_started_by_other_client() {
    for start_web in [false, true] {
        // Keep the selected port occupied so the normal automatic-port fallback
        // supplies a private listener without racing a bind probe.
        let blocker = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let settings = WebShareSettings::from_options_with_port_explicit(
            blocker.local_addr().unwrap().port(),
            None,
            false,
        )
        .unwrap();
        let handler = Arc::new(
            RequestHandler::with_owner_uid_subscription_limits_and_web_settings(
                current_owner_uid(),
                SubscriptionLimits::default(),
                settings,
            ),
        );
        let (shutdown_handle, mut shutdown_rx) = ShutdownHandle::new();
        handler.install_shutdown_handle(shutdown_handle);
        let config = handler.start_config_loading();
        let client_b = handler.begin_detached_connection(7);
        let (owner, observer) = UnixStream::pair().unwrap();
        let watcher = watch(Some(observer), Arc::clone(&handler))
            .unwrap()
            .expect("startup owner is observed");

        // Actual EOF, then completed configuration: the startup watcher must
        // reach its shutdown request while B still owns a live connection.
        drop(owner);
        drop(config);
        timeout(RETRY_WINDOW, watcher)
            .await
            .expect("startup EOF observed")
            .expect("startup watcher completed");
        assert!(
            timeout(RETRY_WINDOW, &mut shutdown_rx).await.is_err(),
            "startup cleanup must defer while B remains connected"
        );

        let listener = if start_web {
            let response = handler
                .handle(Request::WebShare(Box::new(WebShareRequest::Config(
                    WebShareConfigRequest,
                ))))
                .await;
            let Response::WebShare(response) = response else {
                panic!("B must be able to start the web listener");
            };
            let WebShareResponse::Config(config) = *response else {
                panic!("B must receive the web listener configuration");
            };
            Some(config.listener)
        } else {
            None
        };
        drop(client_b);

        if let Some(listener) = listener {
            assert!(
                timeout(RETRY_WINDOW, &mut shutdown_rx).await.is_err(),
                "the abandoned-startup retry must preserve B's persistent web listener"
            );
            let mut web = timeout(
                RETRY_WINDOW,
                tokio::net::TcpStream::connect((listener.host.as_str(), listener.port)),
            )
            .await
            .expect("web listener still accepts")
            .unwrap();
            web.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .await
                .unwrap();
            let mut response = Vec::new();
            timeout(RETRY_WINDOW, web.read_to_end(&mut response))
                .await
                .expect("web listener still responds")
                .unwrap();
            assert!(response.starts_with(b"HTTP/1.1 404"));

            // Ordinary exit-empty retains its own policy even with web active.
            assert!(handler.request_shutdown_if_server_empty().await);
        }
        timeout(RETRY_WINDOW, shutdown_rx)
            .await
            .expect("empty daemon without protected web shuts down")
            .expect("shutdown is requested naturally");
        handler.close_normal_request_admission();
        handler.close_normal_and_drain_lifecycle_producers().await;
    }
}
