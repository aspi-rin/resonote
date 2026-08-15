use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    thread::{self, JoinHandle},
};

use super::{test_support::ScriptedChatClient, *};

struct ScriptedServer {
    address: SocketAddr,
    handle: JoinHandle<String>,
}

fn serve_once(response: String) -> ScriptedServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let request = read_request(&mut stream);
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
        request
    });
    ScriptedServer { address, handle }
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 2_048];
    while let Ok(count) = stream.read(&mut buffer) {
        if count == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..count]);
        let Some(header_end) = request.windows(4).position(|item| item == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        if request.len() >= header_end + 4 + content_length {
            break;
        }
    }
    String::from_utf8_lossy(&request).into_owned()
}

fn json_response(status_line: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn complete(endpoint: &str, api_key: Option<&SecretString>) -> Result<String, ChatError> {
    ReqwestChatClient::new().unwrap().complete(ChatRequest {
        api_key,
        endpoint,
        messages: vec![ChatMessage {
            content: "你好",
            role: ChatRole::User,
        }],
        model: "Hy-MT2-1.8B",
        timeout: Duration::from_secs(2),
    })
}

#[test]
fn builds_chat_completions_url_from_base_or_full_endpoint() {
    assert_eq!(
        chat_completions_url("http://127.0.0.1:8000/v1")
            .unwrap()
            .as_str(),
        "http://127.0.0.1:8000/v1/chat/completions"
    );
    assert_eq!(
        chat_completions_url("http://localhost:8000/v1/chat/completions/")
            .unwrap()
            .as_str(),
        "http://localhost:8000/v1/chat/completions"
    );
    assert_eq!(
        chat_completions_url("https://example.com")
            .unwrap()
            .as_str(),
        "https://example.com/chat/completions"
    );
    assert_eq!(
        chat_completions_url("  https://example.com/v1?key=value#part  ")
            .unwrap()
            .as_str(),
        "https://example.com/v1/chat/completions"
    );
}

#[test]
fn rejects_endpoints_with_userinfo_or_unsupported_schemes() {
    for endpoint in [
        "http://user:password@example.com/v1",
        "http://user@example.com/v1",
        "ftp://example.com/v1",
        "file:///tmp/v1",
        "example.com/v1",
        "",
    ] {
        assert!(
            matches!(
                chat_completions_url(endpoint),
                Err(ChatError::InvalidEndpoint { status: None })
            ),
            "{endpoint} should be rejected"
        );
    }
}

#[test]
fn allows_bearer_only_over_https_or_loopback() {
    for endpoint in [
        "https://example.com/v1",
        "http://127.0.0.1:8000/v1",
        "http://localhost:8000/v1",
        "http://[::1]:8000/v1",
    ] {
        assert!(allows_bearer_auth(&chat_completions_url(endpoint).unwrap()));
    }
    for endpoint in ["http://example.com/v1", "http://192.0.2.10:8000/v1"] {
        assert!(!allows_bearer_auth(
            &chat_completions_url(endpoint).unwrap()
        ));
    }
}

#[test]
fn omits_authorization_when_the_api_key_is_blank() {
    let server = serve_once(json_response(
        "200 OK",
        r#"{"choices":[{"message":{"content":"Hello"}}]}"#,
    ));
    let endpoint = format!("http://{}/v1", server.address);

    let completion = complete(&endpoint, Some(&SecretString::new("   ".to_owned()))).unwrap();

    let request = server.handle.join().unwrap();
    let headers = request.to_ascii_lowercase();
    assert_eq!(completion, "Hello");
    assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1"));
    assert!(!headers.contains("authorization"));
    assert!(headers.contains("content-type: application/json"));
    assert!(request.contains("\"temperature\":0"));
    assert!(request.contains("\"content\":\"你好\""));
}

#[test]
fn sends_the_trimmed_bearer_token() {
    let server = serve_once(json_response(
        "200 OK",
        r#"{"choices":[{"message":{"content":"  Hello world.  "}}]}"#,
    ));
    let endpoint = format!("http://{}/v1", server.address);

    let completion =
        complete(&endpoint, Some(&SecretString::new("  secret  ".to_owned()))).unwrap();

    let request = server.handle.join().unwrap();
    assert_eq!(completion, "Hello world.");
    assert!(request.contains("authorization: Bearer secret\r\n"));
}

#[test]
fn refuses_to_send_credentials_over_remote_plain_http() {
    let error = complete(
        "http://192.0.2.10:8000/v1",
        Some(&SecretString::new("secret".to_owned())),
    )
    .unwrap_err();

    assert!(matches!(error, ChatError::InsecureEndpoint));
    assert!(!error.retryable());
}

#[test]
fn refuses_to_follow_redirects() {
    let server = serve_once(
        "HTTP/1.1 302 Found\r\nLocation: http://192.0.2.10/v1/chat/completions\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_owned(),
    );
    let endpoint = format!("http://{}/v1", server.address);

    let error = complete(&endpoint, Some(&SecretString::new("secret".to_owned()))).unwrap_err();

    let request = server.handle.join().unwrap();
    assert!(matches!(
        error,
        ChatError::InvalidEndpoint { status: Some(_) }
    ));
    assert_eq!(error.http_status(), Some(StatusCode::FOUND));
    assert!(!error.retryable());
    assert_eq!(
        request.matches("authorization: Bearer secret").count(),
        1,
        "the credential must reach only the disclosed host"
    );
}

#[test]
fn stops_reading_response_bodies_over_the_limit() {
    let body = "x".repeat(MAX_RESPONSE_BYTES + 64);
    let server = serve_once(json_response("200 OK", &body));
    let endpoint = format!("http://{}/v1", server.address);

    let error = complete(&endpoint, None).unwrap_err();

    let _ = server.handle.join();
    assert!(matches!(error, ChatError::ResponseTooLarge));
    assert!(!error.retryable());
}

#[test]
fn keeps_provider_response_bodies_out_of_errors() {
    let body = r#"{"error":"upstream echoed Authorization: Bearer top-secret and 你好"}"#;
    let server = serve_once(json_response("500 Internal Server Error", body));
    let endpoint = format!("http://{}/v1", server.address);

    let error = complete(&endpoint, None).unwrap_err();

    let _ = server.handle.join();
    assert!(matches!(error, ChatError::Unavailable(_)));
    assert!(error.retryable());
    assert_eq!(error.http_status(), Some(StatusCode::INTERNAL_SERVER_ERROR));
    for rendered in [error.to_string(), format!("{error:?}")] {
        assert!(!rendered.contains("top-secret"));
        assert!(!rendered.contains("你好"));
        assert!(!rendered.contains("upstream echoed"));
    }
}

#[test]
fn rejects_unusable_completion_payloads() {
    for body in [
        r#"{"choices":[]}"#,
        r#"{"choices":[{}]}"#,
        r#"{"choices":[{"message":{}}]}"#,
        r#"{"choices":[{"message":{"content":null}}]}"#,
        r#"{"choices":[{"message":{"content":42}}]}"#,
        r#"{"choices":[{"message":{"content":"   "}}]}"#,
        "",
        "not json",
    ] {
        assert!(
            matches!(
                parse_completion(body),
                Err(ChatError::ProviderResponseInvalid)
            ),
            "{body} should be rejected"
        );
    }
    assert!(ChatError::ProviderResponseInvalid.retryable());
}

#[test]
fn maps_http_status_codes_to_stable_errors() {
    assert!(matches!(
        classify_status(StatusCode::UNAUTHORIZED),
        ChatError::Unauthorized
    ));
    assert!(matches!(
        classify_status(StatusCode::FORBIDDEN),
        ChatError::Forbidden
    ));
    assert!(matches!(
        classify_status(StatusCode::REQUEST_TIMEOUT),
        ChatError::Timeout
    ));
    assert!(matches!(
        classify_status(StatusCode::TOO_MANY_REQUESTS),
        ChatError::RateLimited(_)
    ));
    assert!(matches!(
        classify_status(StatusCode::BAD_GATEWAY),
        ChatError::Unavailable(_)
    ));
    assert!(matches!(
        classify_status(StatusCode::BAD_REQUEST),
        ChatError::UnexpectedStatus(_)
    ));
    assert!(!classify_status(StatusCode::UNAUTHORIZED).retryable());
    assert!(classify_status(StatusCode::BAD_GATEWAY).retryable());
}

#[test]
fn scripted_client_replays_results_in_call_order() {
    let client = ScriptedChatClient::new(vec![
        Ok("first".to_owned()),
        Err(ChatError::Timeout),
        Ok("third".to_owned()),
    ]);
    let key = SecretString::new(" secret ".to_owned());

    let outcomes = ["a", "b", "c"]
        .into_iter()
        .map(|content| {
            client.complete(ChatRequest {
                api_key: Some(&key),
                endpoint: "http://127.0.0.1:8000/v1",
                messages: vec![ChatMessage {
                    content,
                    role: ChatRole::User,
                }],
                model: "notes-model",
                timeout: Duration::from_secs(30),
            })
        })
        .collect::<Vec<_>>();

    assert_eq!(outcomes[0].as_deref().unwrap(), "first");
    assert!(matches!(outcomes[1], Err(ChatError::Timeout)));
    assert_eq!(outcomes[2].as_deref().unwrap(), "third");
    let requests = client.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].api_key.as_deref(), Some("secret"));
    assert_eq!(requests[0].model, "notes-model");
    assert_eq!(requests[2].messages[0], ("user".to_owned(), "c".to_owned()));
}
