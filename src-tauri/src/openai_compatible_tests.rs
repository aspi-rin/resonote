use super::{
    test_support::{ScriptedChatClient, ScriptedModelLister, json_response, serve_once},
    *,
};

fn list_models(endpoint: &str, api_key: Option<&SecretString>) -> Result<Vec<String>, ChatError> {
    ReqwestChatClient::new()
        .unwrap()
        .list_models(ModelListRequest { api_key, endpoint })
}

fn complete(endpoint: &str, api_key: Option<&SecretString>) -> Result<String, ChatError> {
    ReqwestChatClient::new().unwrap().complete(ChatRequest {
        api_key,
        endpoint,
        max_tokens: None,
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
    assert!(!request.contains("max_tokens"));
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
fn includes_max_tokens_only_when_the_caller_pins_one() {
    let messages = vec![ChatMessage {
        content: "你好",
        role: ChatRole::User,
    }];

    let default = request_body("notes-model", &messages, None);
    let pinned = request_body("notes-model", &messages, Some(8_192));

    assert!(!default.contains("max_tokens"));
    assert!(pinned.contains(r#""max_tokens":8192"#));
    for body in [&default, &pinned] {
        assert!(body.contains(r#""temperature":0"#));
        assert!(body.contains(r#""model":"notes-model""#));
        assert!(body.contains(r#""content":"你好""#));
    }
}

#[test]
fn reports_a_completion_stopped_at_the_output_limit() {
    let truncated = r#"{"choices":[{"finish_reason":"length","message":{"content":"{\"parts\":[{\"segmentId\":1"}}]}"#;
    let empty = r#"{"choices":[{"finish_reason":"length","message":{"content":null}}]}"#;

    assert!(matches!(
        parse_completion(truncated),
        Err(ChatError::OutputTruncated)
    ));
    assert!(matches!(
        parse_completion(empty),
        Err(ChatError::OutputTruncated)
    ));
    assert!(ChatError::OutputTruncated.retryable());
    assert_eq!(ChatError::OutputTruncated.http_status(), None);
    for intact in [
        r#"{"choices":[{"finish_reason":"stop","message":{"content":"Hello"}}]}"#,
        r#"{"choices":[{"finish_reason":null,"message":{"content":"Hello"}}]}"#,
        r#"{"choices":[{"message":{"content":"Hello"}}]}"#,
    ] {
        assert_eq!(parse_completion(intact).unwrap(), "Hello");
    }
}

#[test]
fn sends_the_pinned_output_ceiling_and_surfaces_the_truncation() {
    let server = serve_once(json_response(
        "200 OK",
        r#"{"choices":[{"finish_reason":"length","message":{"content":"partial"}}]}"#,
    ));
    let endpoint = format!("http://{}/v1", server.address);

    let error = ReqwestChatClient::new()
        .unwrap()
        .complete(ChatRequest {
            api_key: None,
            endpoint: &endpoint,
            max_tokens: Some(8_192),
            messages: vec![ChatMessage {
                content: "你好",
                role: ChatRole::User,
            }],
            model: "notes-model",
            timeout: Duration::from_secs(2),
        })
        .unwrap_err();

    let request = server.handle.join().unwrap();
    assert!(matches!(error, ChatError::OutputTruncated));
    assert!(request.contains(r#""max_tokens":8192"#));
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
fn builds_models_url_from_base_or_full_endpoint() {
    assert_eq!(
        models_url("http://127.0.0.1:8000/v1").unwrap().as_str(),
        "http://127.0.0.1:8000/v1/models"
    );
    assert_eq!(
        models_url("http://localhost:8000/v1/chat/completions/")
            .unwrap()
            .as_str(),
        "http://localhost:8000/v1/models"
    );
    assert_eq!(
        models_url("https://example.com").unwrap().as_str(),
        "https://example.com/models"
    );
    assert_eq!(
        models_url("  https://example.com/v1?key=value#part  ")
            .unwrap()
            .as_str(),
        "https://example.com/v1/models"
    );
    for endpoint in [
        "http://user:password@example.com/v1",
        "http://user@example.com/v1",
        "ftp://example.com/v1",
        "example.com/v1",
        "",
    ] {
        assert!(
            matches!(
                models_url(endpoint),
                Err(ChatError::InvalidEndpoint { status: None })
            ),
            "{endpoint} should be rejected"
        );
    }
}

#[test]
fn gets_the_models_path_with_the_bearer_token_and_parses_the_ids() {
    let server = serve_once(json_response(
        "200 OK",
        r#"{"object":"list","data":[{"id":"deepseek-reasoner","object":"model"},{"id":"deepseek-chat"}]}"#,
    ));
    let endpoint = format!("http://{}/v1", server.address);

    let models = list_models(&endpoint, Some(&SecretString::new("  secret  ".to_owned()))).unwrap();

    let request = server.handle.join().unwrap();
    assert_eq!(models, vec!["deepseek-chat", "deepseek-reasoner"]);
    assert!(request.starts_with("GET /v1/models HTTP/1.1"));
    assert!(request.contains("authorization: Bearer secret\r\n"));
}

#[test]
fn omits_authorization_from_the_models_request_when_no_key_is_bound() {
    let server = serve_once(json_response("200 OK", r#"{"data":[]}"#));
    let endpoint = format!("http://{}/v1", server.address);

    let models = list_models(&endpoint, Some(&SecretString::new("   ".to_owned()))).unwrap();

    let request = server.handle.join().unwrap();
    assert!(models.is_empty());
    assert!(!request.to_ascii_lowercase().contains("authorization"));
}

#[test]
fn refuses_to_list_models_over_remote_plain_http_with_a_key() {
    let error = list_models(
        "http://192.0.2.10:8000/v1",
        Some(&SecretString::new("secret".to_owned())),
    )
    .unwrap_err();

    assert!(matches!(error, ChatError::InsecureEndpoint));
}

#[test]
fn dedupes_sorts_and_skips_unusable_model_ids() {
    let body = r#"{"data":[{"id":"gamma"},{"id":"alpha"},{"id":"gamma"},{"id":""},{"id":"  beta  "},{"object":"model"},{"id":null}]}"#;

    assert_eq!(
        parse_model_ids(body).unwrap(),
        vec!["alpha", "beta", "gamma"]
    );
    assert!(parse_model_ids(r#"{"data":[]}"#).unwrap().is_empty());
}

#[test]
fn rejects_unusable_models_payloads() {
    for body in [
        "",
        "not json",
        "{}",
        r#"{"data":{}}"#,
        r#"{"models":[{"id":"alpha"}]}"#,
        r#"{"data":[{"id":42}]}"#,
    ] {
        assert!(
            matches!(
                parse_model_ids(body),
                Err(ChatError::ProviderResponseInvalid)
            ),
            "{body} should be rejected"
        );
    }
}

#[test]
fn scripted_lister_replays_results_in_call_order() {
    let lister = ScriptedModelLister::new(vec![
        Ok(vec!["alpha".to_owned()]),
        Err(ChatError::Unauthorized),
    ]);
    let key = SecretString::new(" secret ".to_owned());

    let first = lister.list_models(ModelListRequest {
        api_key: Some(&key),
        endpoint: "http://127.0.0.1:8000/v1",
    });
    let second = lister.list_models(ModelListRequest {
        api_key: None,
        endpoint: "https://example.com/v1",
    });

    assert_eq!(first.unwrap(), vec!["alpha"]);
    assert!(matches!(second, Err(ChatError::Unauthorized)));
    let requests = lister.requests();
    assert_eq!(requests[0].api_key.as_deref(), Some("secret"));
    assert_eq!(requests[1].endpoint, "https://example.com/v1");
    assert!(requests[1].api_key.is_none());
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
                max_tokens: None,
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
