#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let socket = args.next().expect("socket");
    let operation = args.next().expect("operation");
    let rmux = rmux_sdk::Rmux::builder().unix_socket(socket).connect().await.expect("connect");
    match operation.as_str() {
        "list" => assert!(rmux.list_web_shares().await.expect("list").is_empty()),
        "lookup" => match rmux.web_share_by_id("missing").await {
            Err(error) => assert!(error.to_string().contains("web share not found"), "{error}"),
            Ok(_) => panic!("missing share resolved"),
        },
        "stop" => assert!(!rmux.stop_web_share("missing").await.expect("stop")),
        "stop-all" => assert_eq!(rmux.stop_all_web_shares().await.expect("stop all"), 0),
        _ => panic!("unknown operation"),
    }
    println!("SDK {operation} OK");
}
