use std::time::Duration;

use serde_json::{Value, json};

use crate::ai_provider::{
    AiProvider, ProviderCapabilities, ProviderDescriptor, ProviderError, ProviderErrorCode,
    ProviderResponse, ProviderUsage, RetryPolicy, ReviewRequest, ensure_not_cancelled,
    validate_request,
};
use crate::credentials::{self, CredentialProvider};
use crate::state::TaskControl;

const RESPONSES_URL: &str = "https://api.openai.com/v1/responses";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CANCELLATION_POLL: Duration = Duration::from_millis(25);

#[derive(Debug)]
pub struct OpenAiHttpResponse {
    pub status: u16,
    pub retry_after_ms: Option<u64>,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiTransportError {
    Network,
}

#[async_trait::async_trait]
pub trait OpenAiTransport: Send + Sync {
    async fn post(
        &self,
        api_key: &str,
        body: &Value,
    ) -> Result<OpenAiHttpResponse, OpenAiTransportError>;
}

pub struct ReqwestOpenAiTransport {
    client: reqwest::Client,
}

impl ReqwestOpenAiTransport {
    pub fn new() -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| {
                ProviderError::new(
                    ProviderErrorCode::Internal,
                    "OpenAI 요청을 초기화하지 못했습니다.",
                )
            })?;
        Ok(Self { client })
    }
}

#[async_trait::async_trait]
impl OpenAiTransport for ReqwestOpenAiTransport {
    async fn post(
        &self,
        api_key: &str,
        body: &Value,
    ) -> Result<OpenAiHttpResponse, OpenAiTransportError> {
        let response = self
            .client
            .post(RESPONSES_URL)
            .bearer_auth(api_key)
            .json(body)
            .send()
            .await
            .map_err(|_| OpenAiTransportError::Network)?;
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
            .map_err(|_| OpenAiTransportError::Network)?;

        Ok(OpenAiHttpResponse {
            status,
            retry_after_ms,
            body,
        })
    }
}

pub struct OpenAiProvider<T = ReqwestOpenAiTransport> {
    api_key: String,
    transport: T,
}

impl OpenAiProvider<ReqwestOpenAiTransport> {
    pub fn from_keyring() -> Result<Self, ProviderError> {
        let api_key = credentials::read_for_provider(CredentialProvider::OpenAi)
            .map_err(|_| {
                ProviderError::new(
                    ProviderErrorCode::Internal,
                    "OS 자격 증명 저장소에서 OpenAI 자격 증명을 읽지 못했습니다.",
                )
            })?
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorCode::Authentication,
                    "OpenAI API 키를 먼저 설정해 주세요.",
                )
            })?;

        Ok(Self {
            api_key,
            transport: ReqwestOpenAiTransport::new()?,
        })
    }
}

impl<T> OpenAiProvider<T> {
    pub fn with_transport(api_key: String, transport: T) -> Self {
        Self { api_key, transport }
    }
}

#[async_trait::async_trait]
impl<T: OpenAiTransport> AiProvider for OpenAiProvider<T> {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: "openai".into(),
            display_name: "OpenAI".into(),
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
                "OpenAI API 키를 먼저 설정해 주세요.",
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
                Err(OpenAiTransportError::Network) => Err(ProviderError::new(
                    ProviderErrorCode::Network,
                    "OpenAI 요청 중 네트워크 오류가 발생했습니다.",
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
    let mut body = json!({
        "model": request.model,
        "instructions": "Review only the supplied code and evidence. Do not invent findings or source locations. Return only the requested JSON structure.",
        "input": request.context,
        "text": {
            "format": {
                "type": "json_schema",
                "name": "code_review",
                "strict": true,
                "schema": finding_schema(),
            }
        }
    });
    if let Some(max_output_tokens) = request.max_output_tokens {
        body["max_output_tokens"] = json!(max_output_tokens);
    }
    body
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
    response: OpenAiHttpResponse,
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
            "OpenAI가 올바른 JSON 응답을 반환하지 않았습니다.",
        )
    })?;

    if body.get("status").and_then(Value::as_str) != Some("completed") {
        return Err(ProviderError::new(
            ProviderErrorCode::InvalidResponse,
            "OpenAI 리뷰 응답이 완료되지 않았습니다.",
        ));
    }

    let mut output_text = None;
    if let Some(items) = body.get("output").and_then(Value::as_array) {
        for item in items {
            if let Some(content) = item.get("content").and_then(Value::as_array) {
                for part in content {
                    match part.get("type").and_then(Value::as_str) {
                        Some("output_text") => {
                            output_text = part.get("text").and_then(Value::as_str);
                        }
                        Some("refusal") => {
                            return Err(ProviderError::new(
                                ProviderErrorCode::InvalidResponse,
                                "OpenAI가 리뷰 요청에 응답하지 않았습니다.",
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    let structured_output = output_text
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .filter(Value::is_object)
        .ok_or_else(|| {
            ProviderError::new(
                ProviderErrorCode::InvalidResponse,
                "OpenAI 리뷰 결과가 올바른 구조화 형식이 아닙니다.",
            )
        })?;

    let usage = body.get("usage");
    Ok(ProviderResponse {
        provider_id: "openai".into(),
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
    let code = body
        .pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (code, message) = match (status, code) {
        (401 | 403, _) => (
            ProviderErrorCode::Authentication,
            "OpenAI 인증에 실패했습니다. API 키와 계정 권한을 확인해 주세요.",
        ),
        (429, _) => (
            ProviderErrorCode::RateLimited,
            "OpenAI 요청 한도에 도달했습니다.",
        ),
        (_, "context_length_exceeded" | "max_output_tokens") => (
            ProviderErrorCode::ContextLimitExceeded,
            "요청이 OpenAI 모델의 컨텍스트 한도를 초과했습니다.",
        ),
        (408, _) => (
            ProviderErrorCode::Network,
            "OpenAI 요청 시간이 초과되었습니다.",
        ),
        (500..=599, _) => (
            ProviderErrorCode::ProviderUnavailable,
            "OpenAI 서비스를 일시적으로 사용할 수 없습니다.",
        ),
        (400, _) => (
            ProviderErrorCode::InvalidRequest,
            "OpenAI가 요청 형식이나 모델을 거부했습니다.",
        ),
        _ => (
            ProviderErrorCode::ProviderUnavailable,
            "OpenAI 요청을 처리하지 못했습니다.",
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

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::state::AppState;

    #[derive(Clone)]
    struct MockTransport {
        responses: Arc<Mutex<VecDeque<Result<OpenAiHttpResponse, OpenAiTransportError>>>>,
        calls: Arc<AtomicUsize>,
        seen_authorization: Arc<Mutex<Vec<String>>>,
        seen_body: Arc<Mutex<Vec<Value>>>,
    }

    impl MockTransport {
        fn new(responses: Vec<Result<OpenAiHttpResponse, OpenAiTransportError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                calls: Arc::new(AtomicUsize::new(0)),
                seen_authorization: Arc::new(Mutex::new(Vec::new())),
                seen_body: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait::async_trait]
    impl OpenAiTransport for MockTransport {
        async fn post(
            &self,
            api_key: &str,
            body: &Value,
        ) -> Result<OpenAiHttpResponse, OpenAiTransportError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen_authorization
                .lock()
                .expect("authorization lock")
                .push(api_key.to_owned());
            self.seen_body.lock().expect("body lock").push(body.clone());
            self.responses
                .lock()
                .expect("response lock")
                .pop_front()
                .unwrap_or(Err(OpenAiTransportError::Network))
        }
    }

    struct PendingTransport {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl OpenAiTransport for PendingTransport {
        async fn post(
            &self,
            _api_key: &str,
            _body: &Value,
        ) -> Result<OpenAiHttpResponse, OpenAiTransportError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::future::pending().await
        }
    }

    fn completed_response() -> OpenAiHttpResponse {
        OpenAiHttpResponse {
            status: 200,
            retry_after_ms: None,
            body: json!({
                "status": "completed",
                "model": "gpt-test",
                "output": [{"type": "message", "content": [{"type": "output_text", "text": "{\"findings\":[]}"}]}],
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
            model: "gpt-test".into(),
            max_context_tokens: 4_096,
            max_output_tokens: Some(512),
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
        let provider = OpenAiProvider::with_transport("test-secret".into(), transport.clone());
        let task = AppState::default().register_task().expect("task");

        let result = run(provider.review(&request(), &task, RetryPolicy::default()))
            .expect("review response");
        let body = transport.seen_body.lock().expect("body lock");

        assert_eq!(result.provider_id, "openai");
        assert_eq!(result.model, "gpt-test");
        assert_eq!(result.structured_output, json!({"findings": []}));
        assert_eq!(result.usage.input_tokens, Some(42));
        assert_eq!(result.usage.output_tokens, Some(8));
        assert_eq!(body[0]["model"], "gpt-test");
        assert_eq!(body[0]["text"]["format"]["type"], "json_schema");
        assert!(
            !serde_json::to_string(&result)
                .expect("serialize result")
                .contains("test-secret")
        );
    }

    #[test]
    fn retries_transient_errors_with_a_bounded_policy_and_sanitized_messages() {
        let transport = MockTransport::new(vec![
            Ok(OpenAiHttpResponse {
                status: 429,
                retry_after_ms: Some(1),
                body: "<html>temporarily unavailable</html>".into(),
            }),
            Ok(completed_response()),
        ]);
        let provider = OpenAiProvider::with_transport("test-secret".into(), transport.clone());
        let task = AppState::default().register_task().expect("task");
        let policy = RetryPolicy::new(2, 1, 1);

        let result = run(provider.review(&request(), &task, policy)).expect("retry succeeds");

        assert_eq!(result.provider_id, "openai");
        assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
        assert!(
            transport
                .seen_authorization
                .lock()
                .expect("authorization lock")
                .iter()
                .all(|value| value == "test-secret")
        );
    }

    #[test]
    fn maps_context_errors_without_returning_provider_body() {
        let error = map_http_error(
            400,
            None,
            &json!({"error": {"code": "context_length_exceeded", "message": "test-secret"}}),
        );

        assert_eq!(error.code, ProviderErrorCode::ContextLimitExceeded);
        assert!(!error.message.contains("test-secret"));
    }

    #[test]
    fn cancels_an_in_flight_request_by_dropping_transport_future() {
        let state = AppState::default();
        let task = state.register_task().expect("task");
        let task_id = task.task_id();
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = OpenAiProvider::with_transport(
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
