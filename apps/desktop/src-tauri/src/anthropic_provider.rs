use std::time::Duration;

use serde_json::{Value, json};

use crate::ai_provider::{
    AiProvider, ProviderCapabilities, ProviderDescriptor, ProviderError, ProviderErrorCode,
    ProviderResponse, ProviderUsage, RetryPolicy, ReviewRequest, ensure_not_cancelled,
    validate_request,
};
use crate::credentials::{self, CredentialProvider};
use crate::state::TaskControl;

const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2026-10-21";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CANCELLATION_POLL: Duration = Duration::from_millis(25);
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 4_096;

#[derive(Debug)]
pub struct AnthropicHttpResponse {
    pub status: u16,
    pub retry_after_ms: Option<u64>,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicTransportError {
    Network,
}

#[async_trait::async_trait]
pub trait AnthropicTransport: Send + Sync {
    async fn post(
        &self,
        api_key: &str,
        body: &Value,
    ) -> Result<AnthropicHttpResponse, AnthropicTransportError>;
}

pub struct ReqwestAnthropicTransport {
    client: reqwest::Client,
}

impl ReqwestAnthropicTransport {
    pub fn new() -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| {
                ProviderError::new(
                    ProviderErrorCode::Internal,
                    "Anthropic 요청을 초기화하지 못했습니다.",
                )
            })?;
        Ok(Self { client })
    }
}

#[async_trait::async_trait]
impl AnthropicTransport for ReqwestAnthropicTransport {
    async fn post(
        &self,
        api_key: &str,
        body: &Value,
    ) -> Result<AnthropicHttpResponse, AnthropicTransportError> {
        let response = self
            .client
            .post(MESSAGES_URL)
            .bearer_auth(api_key)
            .header("anthropic-version", API_VERSION)
            .json(body)
            .send()
            .await
            .map_err(|_| AnthropicTransportError::Network)?;
        let status = response.status().as_u16();
        let retry_after_ms = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|seconds| seconds.parse::<u64>().ok())
            .map(|seconds| seconds.saturating_mul(1_000));
        let body = response
            .text()
            .await
            .map_err(|_| AnthropicTransportError::Network)?;

        Ok(AnthropicHttpResponse {
            status,
            retry_after_ms,
            body,
        })
    }
}

pub struct AnthropicProvider<T = ReqwestAnthropicTransport> {
    api_key: String,
    transport: T,
}

impl AnthropicProvider<ReqwestAnthropicTransport> {
    pub fn from_keyring() -> Result<Self, ProviderError> {
        let api_key = credentials::read_for_provider(CredentialProvider::Anthropic)
            .map_err(|_| {
                ProviderError::new(
                    ProviderErrorCode::Internal,
                    "OS 자격 증명 저장소에서 Anthropic 자격 증명을 읽지 못했습니다.",
                )
            })?
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorCode::Authentication,
                    "Anthropic API 키를 먼저 설정해 주세요.",
                )
            })?;

        Ok(Self {
            api_key,
            transport: ReqwestAnthropicTransport::new()?,
        })
    }
}

impl<T> AnthropicProvider<T> {
    pub fn with_transport(api_key: String, transport: T) -> Self {
        Self { api_key, transport }
    }
}

#[async_trait::async_trait]
impl<T: AnthropicTransport> AiProvider for AnthropicProvider<T> {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: "anthropic".into(),
            display_name: "Anthropic".into(),
            capabilities: ProviderCapabilities {
                context_window_tokens: None,
                supports_structured_output: true,
                supports_cancellation: true,
            },
        }
    }

    async fn review(
        &self,
        request: &ReviewRequest,
        task: &TaskControl,
        retry_policy: RetryPolicy,
    ) -> Result<ProviderResponse, ProviderError> {
        let descriptor = self.descriptor();
        validate_request(request, &descriptor)?;
        if self.api_key.trim().is_empty() {
            return Err(ProviderError::new(
                ProviderErrorCode::Authentication,
                "Anthropic API 키를 먼저 설정해 주세요.",
            ));
        }

        let body = build_request(request);
        let mut attempts = 0;
        loop {
            ensure_not_cancelled(task)?;
            attempts += 1;

            let response = tokio::select! {
                biased;
                _ = wait_for_cancellation(task) => return Err(ProviderError::cancelled()),
                result = self.transport.post(&self.api_key, &body) => result,
            };
            let result = match response {
                Ok(response) => parse_http_response(response, &request.model),
                Err(AnthropicTransportError::Network) => Err(ProviderError::new(
                    ProviderErrorCode::Network,
                    "Anthropic 요청 중 네트워크 오류가 발생했습니다.",
                )),
            };

            match result {
                Ok(response) => return Ok(response),
                Err(error) => {
                    let Some(delay) = retry_policy.delay_before_retry(&error, attempts) else {
                        return Err(error);
                    };
                    tokio::select! {
                        biased;
                        _ = wait_for_cancellation(task) => return Err(ProviderError::cancelled()),
                        _ = tokio::time::sleep(delay) => {}
                    }
                }
            }
        }
    }
}

async fn wait_for_cancellation(task: &TaskControl) {
    loop {
        if task.is_cancel_requested() {
            return;
        }
        tokio::time::sleep(CANCELLATION_POLL).await;
    }
}

fn build_request(request: &ReviewRequest) -> Value {
    json!({
        "model": request.model,
        "max_tokens": request.max_output_tokens.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS),
        "system": "Review only the supplied code and evidence. Do not invent findings or source locations. Return only the requested JSON structure.",
        "messages": [{
            "role": "user",
            "content": request.context,
        }],
        "output_config": {
            "format": {
                "type": "json_schema",
                "schema": request.output_schema.clone().unwrap_or_else(finding_schema),
            }
        }
    })
}

fn finding_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "title": { "type": "string" },
                        "description": { "type": "string" },
                        "category": { "type": "string", "enum": ["correctness", "security", "performance", "reliability", "maintainability", "testability", "style", "other"] },
                        "severity": { "type": "string", "enum": ["critical", "high", "medium", "low", "informational"] },
                        "confidence": { "type": "string", "enum": ["high", "medium", "low"] },
                        "evidence": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "relativePath": { "type": "string" },
                                    "startLine": { "type": "integer" },
                                    "startColumn": { "type": "integer" },
                                    "endLine": { "type": "integer" },
                                    "endColumn": { "type": "integer" },
                                    "excerpt": { "type": "string" }
                                },
                                "required": ["relativePath", "startLine", "startColumn", "endLine", "endColumn", "excerpt"]
                            }
                        },
                        "affectedSymbols": { "type": "array", "items": { "type": "string" } },
                        "suggestion": { "type": "string" },
                        "testSuggestion": { "type": "string" }
                    },
                    "required": ["title", "description", "category", "severity", "confidence", "evidence", "affectedSymbols", "suggestion", "testSuggestion"]
                }
            }
        },
        "required": ["findings"]
    })
}

fn parse_http_response(
    response: AnthropicHttpResponse,
    requested_model: &str,
) -> Result<ProviderResponse, ProviderError> {
    if !(200..300).contains(&response.status) {
        let body = serde_json::from_str(&response.body).unwrap_or(Value::Null);
        return Err(map_http_error(
            response.status,
            response.retry_after_ms,
            &body,
        ));
    }

    let body: Value = serde_json::from_str(&response.body).map_err(|_| {
        ProviderError::new(
            ProviderErrorCode::InvalidResponse,
            "Anthropic이 올바른 JSON 응답을 반환하지 않았습니다.",
        )
    })?;
    if body.get("type").and_then(Value::as_str) != Some("message") {
        return Err(ProviderError::new(
            ProviderErrorCode::InvalidResponse,
            "Anthropic 리뷰 응답 형식이 올바르지 않습니다.",
        ));
    }

    let output_text = body
        .get("content")
        .and_then(Value::as_array)
        .and_then(|content| {
            content.iter().find_map(|part| {
                (part.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| part.get("text").and_then(Value::as_str))
                    .flatten()
            })
        });
    let structured_output = output_text
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .filter(Value::is_object)
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorCode::InvalidResponse,
                "Anthropic 리뷰 결과가 올바른 구조화 형식이 아닙니다.",
            )
        })?;

    let usage = body.get("usage");
    Ok(ProviderResponse {
        provider_id: "anthropic".into(),
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(requested_model)
            .to_owned(),
        structured_output,
        usage: ProviderUsage {
            input_tokens: usage
                .and_then(|value| value.get("input_tokens"))
                .and_then(Value::as_u64),
            output_tokens: usage
                .and_then(|value| value.get("output_tokens"))
                .and_then(Value::as_u64),
        },
    })
}

fn map_http_error(status: u16, retry_after_ms: Option<u64>, body: &Value) -> ProviderError {
    let (code, message) = match status {
        401 | 403 => (
            ProviderErrorCode::Authentication,
            "Anthropic 인증 또는 계정 권한을 확인해 주세요.",
        ),
        429 => (
            ProviderErrorCode::RateLimited,
            "Anthropic 요청 한도를 초과했습니다.",
        ),
        408 => (
            ProviderErrorCode::Network,
            "Anthropic 요청 시간이 초과되었습니다.",
        ),
        413 => (
            ProviderErrorCode::ContextLimitExceeded,
            "요청이 Anthropic의 입력 크기 한도를 초과했습니다.",
        ),
        400 if is_context_limit_error(body) => (
            ProviderErrorCode::ContextLimitExceeded,
            "요청이 Anthropic 모델의 컨텍스트 한도를 초과했습니다.",
        ),
        500..=599 => (
            ProviderErrorCode::ProviderUnavailable,
            "Anthropic 서비스를 일시적으로 사용할 수 없습니다.",
        ),
        400 | 402 | 404 | 409 | 422 => (
            ProviderErrorCode::InvalidRequest,
            "Anthropic이 요청을 처리하지 못했습니다. 모델과 계정 상태를 확인해 주세요.",
        ),
        _ => (
            ProviderErrorCode::InvalidResponse,
            "Anthropic 요청에 예상하지 못한 응답이 반환되었습니다.",
        ),
    };
    ProviderError {
        code,
        message: message.into(),
        retry_after_ms: (code == ProviderErrorCode::RateLimited)
            .then_some(retry_after_ms)
            .flatten(),
    }
}

fn is_context_limit_error(body: &Value) -> bool {
    let Some(error) = body.get("error") else {
        return false;
    };
    if error.get("type").and_then(Value::as_str) != Some("invalid_request_error") {
        return false;
    }

    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    ["prompt is too long", "context window", "context length"]
        .iter()
        .any(|marker| message.contains(marker))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::state::AppState;

    #[derive(Clone)]
    struct MockTransport {
        responses: Arc<Mutex<VecDeque<Result<AnthropicHttpResponse, AnthropicTransportError>>>>,
        calls: Arc<AtomicUsize>,
        seen_keys: Arc<Mutex<Vec<String>>>,
        seen_bodies: Arc<Mutex<Vec<Value>>>,
    }

    impl MockTransport {
        fn new(responses: Vec<Result<AnthropicHttpResponse, AnthropicTransportError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                calls: Arc::new(AtomicUsize::new(0)),
                seen_keys: Arc::new(Mutex::new(Vec::new())),
                seen_bodies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait::async_trait]
    impl AnthropicTransport for MockTransport {
        async fn post(
            &self,
            api_key: &str,
            body: &Value,
        ) -> Result<AnthropicHttpResponse, AnthropicTransportError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen_keys
                .lock()
                .expect("key lock")
                .push(api_key.to_owned());
            self.seen_bodies
                .lock()
                .expect("body lock")
                .push(body.clone());
            self.responses
                .lock()
                .expect("response lock")
                .pop_front()
                .unwrap_or(Err(AnthropicTransportError::Network))
        }
    }

    struct PendingTransport {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl AnthropicTransport for PendingTransport {
        async fn post(
            &self,
            _api_key: &str,
            _body: &Value,
        ) -> Result<AnthropicHttpResponse, AnthropicTransportError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::future::pending().await
        }
    }

    fn completed_response() -> AnthropicHttpResponse {
        AnthropicHttpResponse {
            status: 200,
            retry_after_ms: None,
            body: json!({
                "type": "message",
                "model": "claude-test",
                "content": [{"type": "text", "text": "{\"findings\":[]}"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 42, "output_tokens": 8}
            })
            .to_string(),
        }
    }

    fn request() -> ReviewRequest {
        ReviewRequest {
            selection: crate::ai_provider::ReviewSelection::File {
                relative_path: "src/Cart.kt".into(),
                range: None,
            },
            context: "Review this selected source file.".into(),
            model: "claude-test".into(),
            max_context_tokens: 4_096,
            max_output_tokens: Some(512),
            output_schema: None,
        }
    }

    fn run<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    #[test]
    fn sends_structured_request_and_returns_usage_without_exposing_key() {
        let transport = MockTransport::new(vec![Ok(completed_response())]);
        let provider = AnthropicProvider::with_transport("test-secret".into(), transport.clone());
        let task = AppState::default().register_task().expect("task");

        let result = run(provider.review(&request(), &task, RetryPolicy::default()))
            .expect("review response");
        let bodies = transport.seen_bodies.lock().expect("body lock");
        let serialized = serde_json::to_string(&result).expect("serialize result");

        assert_eq!(result.provider_id, "anthropic");
        assert_eq!(result.model, "claude-test");
        assert_eq!(result.structured_output, json!({"findings": []}));
        assert_eq!(result.usage.input_tokens, Some(42));
        assert_eq!(result.usage.output_tokens, Some(8));
        assert_eq!(bodies[0]["model"], "claude-test");
        assert_eq!(bodies[0]["max_tokens"], 512);
        assert_eq!(
            bodies[0]["messages"][0]["content"],
            "Review this selected source file."
        );
        assert_eq!(bodies[0]["output_config"]["format"]["type"], "json_schema");
        assert!(!serialized.contains("test-secret"));
    }

    #[test]
    fn uses_bounded_retry_for_non_json_rate_limit_response() {
        let transport = MockTransport::new(vec![
            Ok(AnthropicHttpResponse {
                status: 429,
                retry_after_ms: Some(1),
                body: "<html>temporarily unavailable</html>".into(),
            }),
            Ok(completed_response()),
        ]);
        let provider = AnthropicProvider::with_transport("test-secret".into(), transport.clone());
        let task = AppState::default().register_task().expect("task");

        let result = run(provider.review(&request(), &task, RetryPolicy::new(2, 1, 1)))
            .expect("retry succeeds");

        assert_eq!(result.provider_id, "anthropic");
        assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
        assert!(
            transport
                .seen_keys
                .lock()
                .expect("key lock")
                .iter()
                .all(|key| key == "test-secret")
        );
    }

    #[test]
    fn maps_authentication_errors_without_returning_provider_body() {
        let error = map_http_error(401, None, &json!({"error": {"message": "test-secret"}}));

        assert_eq!(error.code, ProviderErrorCode::Authentication);
        assert!(!error.message.contains("test-secret"));
    }

    #[test]
    fn maps_context_size_errors_and_rate_limit_retry_after() {
        let size_error = map_http_error(413, None, &Value::Null);
        let rate_error = map_http_error(429, Some(2_000), &Value::Null);

        assert_eq!(size_error.code, ProviderErrorCode::ContextLimitExceeded);
        assert_eq!(rate_error.code, ProviderErrorCode::RateLimited);
        assert_eq!(rate_error.retry_after_ms, Some(2_000));
    }

    #[test]
    fn maps_context_limit_400_without_exposing_provider_message() {
        let provider_message = "prompt is too long: secret source details";
        let context_error = map_http_error(
            400,
            None,
            &json!({
                "type": "error",
                "error": {
                    "type": "invalid_request_error",
                    "message": provider_message
                }
            }),
        );
        let invalid_request = map_http_error(
            400,
            None,
            &json!({
                "type": "error",
                "error": {
                    "type": "invalid_request_error",
                    "message": "unsupported parameter"
                }
            }),
        );

        assert_eq!(context_error.code, ProviderErrorCode::ContextLimitExceeded);
        assert!(!context_error.message.contains("secret source details"));
        assert_eq!(invalid_request.code, ProviderErrorCode::InvalidRequest);
    }

    #[test]
    fn maps_network_and_invalid_response_errors_without_raw_details() {
        let transport = MockTransport::new(vec![Err(AnthropicTransportError::Network)]);
        let provider = AnthropicProvider::with_transport("test-secret".into(), transport);
        let task = AppState::default().register_task().expect("task");
        let network_error = match run(provider.review(&request(), &task, RetryPolicy::new(1, 1, 1)))
        {
            Err(error) => error,
            Ok(_) => panic!("network failure must return an error"),
        };
        let response_error = match parse_http_response(
            AnthropicHttpResponse {
                status: 200,
                retry_after_ms: None,
                body: "not-json test-secret".into(),
            },
            "claude-test",
        ) {
            Err(error) => error,
            Ok(_) => panic!("malformed success response must be rejected"),
        };

        assert_eq!(network_error.code, ProviderErrorCode::Network);
        assert!(!network_error.message.contains("test-secret"));
        assert_eq!(response_error.code, ProviderErrorCode::InvalidResponse);
        assert!(!response_error.message.contains("test-secret"));
    }

    #[test]
    fn uses_bounded_default_output_limit_when_request_omits_it() {
        let mut request = request();
        request.max_output_tokens = None;

        assert_eq!(
            build_request(&request)["max_tokens"],
            DEFAULT_MAX_OUTPUT_TOKENS
        );
    }

    #[test]
    fn cancels_an_in_flight_request_by_dropping_transport_future() {
        let state = AppState::default();
        let task = state.register_task().expect("task");
        let task_id = task.task_id();
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = AnthropicProvider::with_transport(
            "test-secret".into(),
            PendingTransport {
                calls: Arc::clone(&calls),
            },
        );
        let cancellation_state = state.clone();

        let result = run(async move {
            let reviewer = tokio::spawn(async move {
                provider
                    .review(&request(), &task, RetryPolicy::default())
                    .await
            });
            tokio::time::sleep(Duration::from_millis(10)).await;
            cancellation_state
                .request_cancel(task_id)
                .expect("cancel task");
            reviewer.await.expect("review join")
        });

        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("cancelled review must return an error"),
        };
        assert_eq!(error.code, ProviderErrorCode::Cancelled);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
