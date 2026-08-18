use std::{io::Read, net::IpAddr, time::Duration};

use reqwest::{
    StatusCode, Url,
    blocking::{Client, Response},
    header::{AUTHORIZATION, CONTENT_TYPE},
    redirect,
};
use serde::Deserialize;
use thiserror::Error;

use crate::settings::SecretString;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MODELS_TIMEOUT: Duration = Duration::from_secs(15);

pub trait ChatCompletionPort: Send + Sync {
    fn complete(&self, request: ChatRequest<'_>) -> Result<String, ChatError>;
}

/// Discovery, deliberately split from completion: listing needs no model, so it
/// stays usable before one has ever been picked.
pub trait ModelListPort: Send + Sync {
    fn list_models(&self, request: ModelListRequest<'_>) -> Result<Vec<String>, ChatError>;
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    System,
    User,
}

impl ChatRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
        }
    }
}

pub struct ChatMessage<'a> {
    pub content: &'a str,
    pub role: ChatRole,
}

/// Never derives Serialize or Debug: it borrows a secret and carries prompt data.
pub struct ChatRequest<'a> {
    pub api_key: Option<&'a SecretString>,
    pub endpoint: &'a str,
    /// `None` leaves the provider default in place; `Some` pins an explicit
    /// output ceiling for callers whose prompts expect a long reply.
    pub max_tokens: Option<u32>,
    pub messages: Vec<ChatMessage<'a>>,
    pub model: &'a str,
    pub timeout: Duration,
}

/// Never derives Serialize or Debug: it borrows a secret.
pub struct ModelListRequest<'a> {
    pub api_key: Option<&'a SecretString>,
    pub endpoint: &'a str,
}

pub struct ReqwestChatClient {
    client: Client,
}

impl ReqwestChatClient {
    pub fn new() -> Result<Self, ChatError> {
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(redirect::Policy::none())
            .build()
            .map_err(|_| ChatError::RequestFailed)?;
        Ok(Self { client })
    }
}

impl ChatCompletionPort for ReqwestChatClient {
    fn complete(&self, request: ChatRequest<'_>) -> Result<String, ChatError> {
        let url = chat_completions_url(request.endpoint)?;
        let api_key = request
            .api_key
            .map(SecretString::trimmed)
            .filter(|key| !key.is_empty());
        if api_key.is_some() && !allows_bearer_auth(&url) {
            return Err(ChatError::InsecureEndpoint);
        }
        let mut builder = self
            .client
            .post(url)
            .timeout(request.timeout)
            .header(CONTENT_TYPE, "application/json");
        if let Some(api_key) = api_key {
            builder = builder.header(AUTHORIZATION, format!("Bearer {api_key}"));
        }
        let response = builder
            .body(request_body(
                request.model,
                &request.messages,
                request.max_tokens,
            ))
            .send()
            .map_err(classify_transport_error)?;
        let status = response.status();
        if status.is_redirection() {
            return Err(ChatError::InvalidEndpoint {
                status: Some(status),
            });
        }
        if !status.is_success() {
            return Err(classify_status(status));
        }
        parse_completion(&read_body(response)?)
    }
}

impl ModelListPort for ReqwestChatClient {
    fn list_models(&self, request: ModelListRequest<'_>) -> Result<Vec<String>, ChatError> {
        let url = models_url(request.endpoint)?;
        let api_key = request
            .api_key
            .map(SecretString::trimmed)
            .filter(|key| !key.is_empty());
        if api_key.is_some() && !allows_bearer_auth(&url) {
            return Err(ChatError::InsecureEndpoint);
        }
        let mut builder = self.client.get(url).timeout(MODELS_TIMEOUT);
        if let Some(api_key) = api_key {
            builder = builder.header(AUTHORIZATION, format!("Bearer {api_key}"));
        }
        let response = builder.send().map_err(classify_transport_error)?;
        let status = response.status();
        if status.is_redirection() {
            return Err(ChatError::InvalidEndpoint {
                status: Some(status),
            });
        }
        if !status.is_success() {
            return Err(classify_status(status));
        }
        parse_model_ids(&read_body(response)?)
    }
}

pub fn chat_completions_url(endpoint: &str) -> Result<Url, ChatError> {
    let mut url =
        Url::parse(endpoint.trim()).map_err(|_| ChatError::InvalidEndpoint { status: None })?;
    let usable = matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none();
    if !usable {
        return Err(ChatError::InvalidEndpoint { status: None });
    }
    let base_path = url.path().trim_end_matches('/');
    let path = if base_path.ends_with("/chat/completions") {
        base_path.to_owned()
    } else if base_path.is_empty() {
        "/chat/completions".to_owned()
    } else {
        format!("{base_path}/chat/completions")
    };
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

/// Derived from the completions URL so both calls agree on what an endpoint
/// means, then swapped onto the sibling `/models` path.
pub fn models_url(endpoint: &str) -> Result<Url, ChatError> {
    let mut url = chat_completions_url(endpoint)?;
    let base = url
        .path()
        .strip_suffix("/chat/completions")
        .unwrap_or_default()
        .to_owned();
    url.set_path(&format!("{base}/models"));
    Ok(url)
}

pub fn allows_bearer_auth(url: &Url) -> bool {
    url.scheme() == "https" || url.host_str().is_some_and(is_loopback_host)
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// The exact bytes sent to the provider, so callers can meter a request against
/// their own character budget before building it.
pub fn request_body(model: &str, messages: &[ChatMessage<'_>], max_tokens: Option<u32>) -> String {
    let messages = messages
        .iter()
        .map(|message| {
            serde_json::json!({ "role": message.role.as_str(), "content": message.content })
        })
        .collect::<Vec<_>>();
    let mut body = serde_json::json!({
        "model": model,
        "temperature": 0,
        "messages": messages,
    });
    if let (Some(max_tokens), Some(object)) = (max_tokens, body.as_object_mut()) {
        object.insert("max_tokens".to_owned(), serde_json::json!(max_tokens));
    }
    body.to_string()
}

fn read_body(response: Response) -> Result<String, ChatError> {
    let mut body = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::TimedOut => ChatError::Timeout,
            _ => ChatError::RequestFailed,
        })?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(ChatError::ResponseTooLarge);
    }
    String::from_utf8(body).map_err(|_| ChatError::ProviderResponseInvalid)
}

fn parse_completion(body: &str) -> Result<String, ChatError> {
    #[derive(Deserialize)]
    struct CompletionResponse {
        choices: Vec<CompletionChoice>,
    }
    #[derive(Deserialize)]
    struct CompletionChoice {
        finish_reason: Option<String>,
        message: Option<CompletionMessage>,
    }
    #[derive(Deserialize)]
    struct CompletionMessage {
        content: Option<String>,
    }

    let parsed: CompletionResponse =
        serde_json::from_str(body).map_err(|_| ChatError::ProviderResponseInvalid)?;
    let choice = parsed
        .choices
        .into_iter()
        .next()
        .ok_or(ChatError::ProviderResponseInvalid)?;
    // A completion cut off at the output limit is a distinct failure: the payload
    // is well formed up to the cut, so reporting it as invalid output hides the
    // real cause.
    if choice.finish_reason.as_deref() == Some("length") {
        return Err(ChatError::OutputTruncated);
    }
    choice
        .message
        .and_then(|message| message.content)
        .map(|content| content.trim().to_owned())
        .filter(|content| !content.is_empty())
        .ok_or(ChatError::ProviderResponseInvalid)
}

fn parse_model_ids(body: &str) -> Result<Vec<String>, ChatError> {
    #[derive(Deserialize)]
    struct ModelsResponse {
        data: Vec<ModelEntry>,
    }
    #[derive(Deserialize)]
    struct ModelEntry {
        id: Option<String>,
    }

    let parsed: ModelsResponse =
        serde_json::from_str(body).map_err(|_| ChatError::ProviderResponseInvalid)?;
    let mut ids = parsed
        .data
        .into_iter()
        .filter_map(|entry| entry.id)
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

fn classify_transport_error(error: reqwest::Error) -> ChatError {
    if error.is_timeout() {
        ChatError::Timeout
    } else if error.is_redirect() {
        ChatError::InvalidEndpoint {
            status: error.status(),
        }
    } else {
        ChatError::RequestFailed
    }
}

fn classify_status(status: StatusCode) -> ChatError {
    match status {
        StatusCode::UNAUTHORIZED => ChatError::Unauthorized,
        StatusCode::FORBIDDEN => ChatError::Forbidden,
        StatusCode::REQUEST_TIMEOUT => ChatError::Timeout,
        StatusCode::TOO_MANY_REQUESTS => ChatError::RateLimited(status),
        _ if status.is_server_error() => ChatError::Unavailable(status),
        _ => ChatError::UnexpectedStatus(status),
    }
}

/// Messages carry only locally generated text and the status code, never the
/// request body, the provider response body or the Authorization header.
#[derive(Debug, Error)]
pub enum ChatError {
    #[error("chat endpoint denied the request")]
    Forbidden,
    #[error("chat endpoint requires https or a loopback host to send credentials")]
    InsecureEndpoint,
    #[error("chat endpoint is not a usable http or https chat completions URL")]
    InvalidEndpoint { status: Option<StatusCode> },
    #[error("chat endpoint stopped the completion at its output token limit")]
    OutputTruncated,
    #[error("chat endpoint returned an unusable completion payload")]
    ProviderResponseInvalid,
    #[error("chat endpoint rate limited the request (HTTP {0})")]
    RateLimited(StatusCode),
    #[error("chat request did not reach the endpoint")]
    RequestFailed,
    #[error("chat response exceeded the supported size limit")]
    ResponseTooLarge,
    #[error("chat request timed out")]
    Timeout,
    #[error("chat endpoint rejected the credentials")]
    Unauthorized,
    #[error("chat endpoint is unavailable (HTTP {0})")]
    Unavailable(StatusCode),
    #[error("chat endpoint returned HTTP {0}")]
    UnexpectedStatus(StatusCode),
}

impl ChatError {
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::OutputTruncated
                | Self::ProviderResponseInvalid
                | Self::RateLimited(_)
                | Self::RequestFailed
                | Self::Timeout
                | Self::Unavailable(_)
        )
    }

    pub fn http_status(&self) -> Option<StatusCode> {
        match self {
            Self::InvalidEndpoint { status } => *status,
            Self::RateLimited(status)
            | Self::Unavailable(status)
            | Self::UnexpectedStatus(status) => Some(*status),
            Self::Forbidden => Some(StatusCode::FORBIDDEN),
            Self::Unauthorized => Some(StatusCode::UNAUTHORIZED),
            _ => None,
        }
    }
}

#[cfg(test)]
pub mod test_support {
    use std::{
        collections::VecDeque,
        io::{Read, Write},
        net::{SocketAddr, TcpListener, TcpStream},
        sync::Mutex,
        thread::{self, JoinHandle},
        time::Duration,
    };

    use super::{ChatCompletionPort, ChatError, ChatRequest, ModelListPort, ModelListRequest};

    /// Answers exactly one chat request and hands the raw request text back, so
    /// a test can assert on the headers the client actually sent.
    pub struct ScriptedServer {
        pub address: SocketAddr,
        pub handle: JoinHandle<String>,
    }

    pub fn serve_once(response: String) -> ScriptedServer {
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

    pub fn json_response(status_line: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[derive(Clone)]
    pub struct RecordedChatRequest {
        pub api_key: Option<String>,
        pub endpoint: String,
        pub max_tokens: Option<u32>,
        pub messages: Vec<(String, String)>,
        pub model: String,
        pub timeout: Duration,
    }

    /// Returns queued results in call order and records every request it saw.
    pub struct ScriptedChatClient {
        requests: Mutex<Vec<RecordedChatRequest>>,
        results: Mutex<VecDeque<Result<String, ChatError>>>,
    }

    impl ScriptedChatClient {
        pub fn new(results: Vec<Result<String, ChatError>>) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                results: Mutex::new(results.into()),
            }
        }

        pub fn requests(&self) -> Vec<RecordedChatRequest> {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    impl ChatCompletionPort for ScriptedChatClient {
        fn complete(&self, request: ChatRequest<'_>) -> Result<String, ChatError> {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(RecordedChatRequest {
                    api_key: request
                        .api_key
                        .map(|key| key.trimmed().to_owned())
                        .filter(|key| !key.is_empty()),
                    endpoint: request.endpoint.to_owned(),
                    max_tokens: request.max_tokens,
                    messages: request
                        .messages
                        .iter()
                        .map(|message| {
                            (message.role.as_str().to_owned(), message.content.to_owned())
                        })
                        .collect(),
                    model: request.model.to_owned(),
                    timeout: request.timeout,
                });
            self.results
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop_front()
                .unwrap_or(Err(ChatError::ProviderResponseInvalid))
        }
    }

    #[derive(Clone)]
    pub struct RecordedModelsRequest {
        pub api_key: Option<String>,
        pub endpoint: String,
    }

    /// The listing counterpart of `ScriptedChatClient`.
    pub struct ScriptedModelLister {
        requests: Mutex<Vec<RecordedModelsRequest>>,
        results: Mutex<VecDeque<Result<Vec<String>, ChatError>>>,
    }

    impl ScriptedModelLister {
        pub fn new(results: Vec<Result<Vec<String>, ChatError>>) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                results: Mutex::new(results.into()),
            }
        }

        pub fn requests(&self) -> Vec<RecordedModelsRequest> {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    impl ModelListPort for ScriptedModelLister {
        fn list_models(&self, request: ModelListRequest<'_>) -> Result<Vec<String>, ChatError> {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(RecordedModelsRequest {
                    api_key: request
                        .api_key
                        .map(|key| key.trimmed().to_owned())
                        .filter(|key| !key.is_empty()),
                    endpoint: request.endpoint.to_owned(),
                });
            self.results
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop_front()
                .unwrap_or(Err(ChatError::ProviderResponseInvalid))
        }
    }
}

#[cfg(test)]
#[path = "openai_compatible_tests.rs"]
mod tests;
