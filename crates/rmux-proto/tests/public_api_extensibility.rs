use rmux_proto::{KillServerRequest, KillServerResponse, Request, Response};

fn classify_request(request: &Request) -> &'static str {
    match request {
        Request::KillServer(_) => "kill-server",
        _ => "other",
    }
}

fn classify_response(response: &Response) -> &'static str {
    match response {
        Response::KillServer(_) => "kill-server",
        _ => "other",
    }
}

#[test]
fn downstream_request_matches_remain_forward_compatible() {
    assert_eq!(
        classify_request(&Request::KillServer(KillServerRequest)),
        "kill-server"
    );
}

#[test]
fn downstream_response_matches_remain_forward_compatible() {
    assert_eq!(
        classify_response(&Response::KillServer(KillServerResponse)),
        "kill-server"
    );
}
