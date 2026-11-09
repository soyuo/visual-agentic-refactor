use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::analyzer::SourceRange;
use crate::state::TaskControl;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ReviewSelection {
    File {
        relative_path: String,
        range: Option<SourceRange>,
    },
    GraphNode {
        node_id: String,
        label: String,
        range: Option<SourceRange>,
    },
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRequest {
    pub selection: ReviewSelection,
    pub context: String,
    pub model: String,
    /// Maximum context budget selected by the caller. This is a token budget,
    /// not an estimate of the current context's token count.
    pub max_context_tokens: u32,
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    pub context_window_tokens: Option<u32>,
    pub supports_structured_output: bool,
    /// Whether an adapter can abort a request that is already in flight.
    pub supports_cancellation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDescriptor {
    /// Stable, lowercase identifier such as `openai` or `anthropic`.
    pub id: String,
    pub display_name: String,
    pub capabilities: ProviderCapabilities,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderResponse {
    pub provider_id: String,
    pub model: String,
    /// Provider-specific structured JSON. The finding validator owns its schema.
    pub structured_output: Value,
    pub usage: ProviderUsage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderErrorCode {
    Authentication,
    RateLimited,
    Network,
    ProviderUnavailable,
    InvalidRequest,
    ContextLimitExceeded,
    InvalidResponse,
    Unsupported,
    Cancelled,
    Internal,
}

impl ProviderErrorCode {
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::RateLimited | Self::Network | Self::ProviderUnavailable
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderError {
    pub code: ProviderErrorCode,
    /// Safe for display and logs; adapters must not copy raw secrets or headers here.
    pub message: String,
    pub retry_after_ms: Option<u64>,
}

impl ProviderError {
    pub fn new(code: ProviderErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retry_after_ms: None,
        }
    }

    pub fn cancelled() -> Self {
        Self::new(ProviderErrorCode::Cancelled, "리뷰 요청이 취소되었습니다.")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    max_attempts: u8,
    initial_delay_ms: u64,
    max_delay_ms: u64,
}

impl RetryPolicy {
    pub fn new(max_attempts: u8, initial_delay_ms: u64, max_delay_ms: u64) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            initial_delay_ms,
            max_delay_ms: max_delay_ms.max(initial_delay_ms),
        }
    }

    pub fn max_attempts(self) -> u8 {
        self.max_attempts
    }

    /// Returns the delay before another attempt, or `None` when retrying is
    /// unsafe or the configured attempt bound has been reached.
    pub fn delay_before_retry(
        self,
        error: &ProviderError,
        attempts_completed: u8,
    ) -> Option<Duration> {
        if !error.code.is_retryable()
            || attempts_completed == 0
            || attempts_completed >= self.max_attempts
        {
            return None;
        }

        let exponent = u32::from(attempts_completed - 1).min(63);
        let backoff_ms = self
            .initial_delay_ms
            .saturating_mul(1_u64 << exponent)
            .min(self.max_delay_ms);
        let delay_ms = if error.code == ProviderErrorCode::RateLimited {
            error
                .retry_after_ms
                .map(|retry_after_ms| retry_after_ms.min(self.max_delay_ms))
                .unwrap_or(backoff_ms)
        } else {
            backoff_ms
        };

        Some(Duration::from_millis(delay_ms))
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::new(3, 500, 8_000)
    }
}

/// Rejects invalid model/budget requests before an adapter contacts a provider.
pub fn validate_request(
    request: &ReviewRequest,
    descriptor: &ProviderDescriptor,
) -> Result<(), ProviderError> {
    if descriptor.id.trim().is_empty() || descriptor.display_name.trim().is_empty() {
        return Err(ProviderError::new(
            ProviderErrorCode::Internal,
            "제공자 식별 정보가 올바르지 않습니다.",
        ));
    }
    if request.model.trim().is_empty() || request.context.trim().is_empty() {
        return Err(ProviderError::new(
            ProviderErrorCode::InvalidRequest,
            "모델과 리뷰 컨텍스트가 필요합니다.",
        ));
    }
    if request.max_output_tokens == Some(0) {
        return Err(ProviderError::new(
            ProviderErrorCode::InvalidRequest,
            "출력 토큰 한도는 0보다 커야 합니다.",
        ));
    }
    if request.max_context_tokens == 0 {
        return Err(ProviderError::new(
            ProviderErrorCode::InvalidRequest,
            "컨텍스트 토큰 한도는 0보다 커야 합니다.",
        ));
    }
    if let Some(context_window_tokens) = descriptor.capabilities.context_window_tokens {
        let requested_tokens = request
            .max_context_tokens
            .saturating_add(request.max_output_tokens.unwrap_or_default());
        if requested_tokens > context_window_tokens {
            return Err(ProviderError::new(
                ProviderErrorCode::ContextLimitExceeded,
                "요청한 입력·출력 토큰 한도가 제공자의 모델 한도를 초과합니다.",
            ));
        }
    }
    if !descriptor.capabilities.supports_structured_output {
        return Err(ProviderError::new(
            ProviderErrorCode::Unsupported,
            "제공자가 구조화 출력을 지원하지 않습니다.",
        ));
    }

    Ok(())
}

/// Adapters call this before each external request and before each retry.
pub fn ensure_not_cancelled(task: &TaskControl) -> Result<(), ProviderError> {
    if task.is_cancel_requested() {
        Err(ProviderError::cancelled())
    } else {
        Ok(())
    }
}

/// Provider implementations own transport, but share request, response,
/// cancellation, and retry contracts.
#[async_trait::async_trait]
pub trait AiProvider: Send + Sync {
    fn descriptor(&self) -> ProviderDescriptor;

    /// The caller must obtain user approval before invoking an external adapter.
    /// Implementations must validate the request and observe cancellation before
    /// sending and before every retry. Error messages must be sanitized.
    async fn review(
        &self,
        request: &ReviewRequest,
        task: &TaskControl,
        retry_policy: RetryPolicy,
    ) -> Result<ProviderResponse, ProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use serde_json::json;

    struct FakeProvider {
        response: Result<ProviderResponse, ProviderError>,
        descriptor: ProviderDescriptor,
    }

    #[async_trait::async_trait]
    impl AiProvider for FakeProvider {
        fn descriptor(&self) -> ProviderDescriptor {
            self.descriptor.clone()
        }

        async fn review(
            &self,
            request: &ReviewRequest,
            task: &TaskControl,
            _retry_policy: RetryPolicy,
        ) -> Result<ProviderResponse, ProviderError> {
            ensure_not_cancelled(task)?;
            validate_request(request, &self.descriptor)?;
            self.response.clone()
        }
    }

    fn fixture_provider(response: Result<ProviderResponse, ProviderError>) -> FakeProvider {
        FakeProvider {
            response,
            descriptor: ProviderDescriptor {
                id: "fixture".into(),
                display_name: "Fixture provider".into(),
                capabilities: ProviderCapabilities {
                    context_window_tokens: Some(8_192),
                    supports_structured_output: true,
                    supports_cancellation: true,
                },
            },
        }
    }

    fn fixture_request() -> ReviewRequest {
        ReviewRequest {
            selection: ReviewSelection::GraphNode {
                node_id: "method:Cart.add".into(),
                label: "Cart.add".into(),
                range: None,
            },
            context: "Review only the selected method and supplied evidence.".into(),
            model: "fixture-model".into(),
            max_context_tokens: 4_096,
            max_output_tokens: Some(1_024),
        }
    }

    #[test]
    fn adapter_contract_returns_structured_output_usage_and_identity() {
        let response = ProviderResponse {
            provider_id: "fixture".into(),
            model: "fixture-model".into(),
            structured_output: json!({"findings": []}),
            usage: ProviderUsage {
                input_tokens: Some(128),
                output_tokens: Some(16),
            },
        };
        let provider = fixture_provider(Ok(response.clone()));
        let task = AppState::default().register_task().expect("register task");

        let actual = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime")
            .block_on(provider.review(&fixture_request(), &task, RetryPolicy::default()))
            .expect("fixture review");

        assert!(
            actual == response,
            "adapter should return the structured response"
        );
        assert_eq!(provider.descriptor().id, "fixture");

        let request_json = serde_json::to_value(fixture_request()).expect("serialize request");
        let response_json = serde_json::to_value(actual).expect("serialize response");
        assert_eq!(request_json["maxContextTokens"], 4_096);
        assert_eq!(
            request_json["selection"]["graphNode"]["nodeId"],
            "method:Cart.add"
        );
        assert_eq!(response_json["providerId"], "fixture");
        assert_eq!(response_json["usage"]["inputTokens"], 128);
    }

    #[test]
    fn adapter_contract_preserves_classified_provider_errors() {
        let expected = ProviderError {
            code: ProviderErrorCode::RateLimited,
            message: "요청 한도를 초과했습니다.".into(),
            retry_after_ms: Some(2_000),
        };
        let provider = fixture_provider(Err(expected.clone()));
        let task = AppState::default().register_task().expect("register task");

        let actual = match tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime")
            .block_on(provider.review(&fixture_request(), &task, RetryPolicy::default()))
        {
            Err(error) => error,
            Ok(_) => panic!("fixture provider should return its error"),
        };

        assert_eq!(actual, expected);
        assert!(actual.code.is_retryable());
    }

    #[test]
    fn request_validation_rejects_total_token_budget_above_provider_limit() {
        let provider = fixture_provider(Ok(ProviderResponse {
            provider_id: "fixture".into(),
            model: "fixture-model".into(),
            structured_output: json!({"findings": []}),
            usage: ProviderUsage::default(),
        }));
        let mut request = fixture_request();
        request.max_context_tokens = 8_192;

        let error = validate_request(&request, &provider.descriptor)
            .expect_err("request should exceed fixture context window");

        assert_eq!(error.code, ProviderErrorCode::ContextLimitExceeded);
        assert!(!error.code.is_retryable());
    }

    #[test]
    fn retry_policy_is_bounded_classified_and_honors_retry_after() {
        let policy = RetryPolicy::default();
        let transient = ProviderError::new(ProviderErrorCode::Network, "network unavailable");
        let rate_limited = ProviderError {
            code: ProviderErrorCode::RateLimited,
            message: "rate limited".into(),
            retry_after_ms: Some(20_000),
        };
        let permanent = ProviderError::new(ProviderErrorCode::Authentication, "not authorized");

        assert_eq!(policy.max_attempts(), 3);
        assert_eq!(
            policy.delay_before_retry(&transient, 1),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            policy.delay_before_retry(&transient, 2),
            Some(Duration::from_millis(1_000))
        );
        assert_eq!(
            policy.delay_before_retry(&rate_limited, 1),
            Some(Duration::from_millis(8_000))
        );
        let network_with_retry_after = ProviderError {
            code: ProviderErrorCode::Network,
            message: "network unavailable".into(),
            retry_after_ms: Some(7_000),
        };
        assert_eq!(
            policy.delay_before_retry(&network_with_retry_after, 1),
            Some(Duration::from_millis(500))
        );
        assert_eq!(policy.delay_before_retry(&transient, 3), None);
        assert_eq!(policy.delay_before_retry(&permanent, 1), None);
    }

    #[test]
    fn adapter_contract_stops_before_sending_after_cancellation() {
        let state = AppState::default();
        let task = state.register_task().expect("register task");
        state.request_cancel(task.task_id()).expect("cancel task");
        let provider = fixture_provider(Ok(ProviderResponse {
            provider_id: "fixture".into(),
            model: "fixture-model".into(),
            structured_output: json!({"findings": []}),
            usage: ProviderUsage::default(),
        }));

        let error = match tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime")
            .block_on(provider.review(&fixture_request(), &task, RetryPolicy::default()))
        {
            Err(error) => error,
            Ok(_) => panic!("cancelled request must not reach provider"),
        };

        assert_eq!(error.code, ProviderErrorCode::Cancelled);
    }
}
