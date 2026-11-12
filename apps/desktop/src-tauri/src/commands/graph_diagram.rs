use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::State;

use crate::ai_provider::{
    AiProvider, ProviderError, ProviderUsage, RetryPolicy, ReviewRequest, ReviewSelection,
    validate_request,
};
use crate::anthropic_provider::AnthropicProvider;
use crate::claude_code_provider::ClaudeCodeProvider;
use crate::command_error::{CommandError, CommandErrorCode};
use crate::credentials::CredentialProvider;
use crate::knowledge_graph::GraphSnapshot;
use crate::openai_provider::OpenAiProvider;
use crate::state::AppState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateGraphDiagramRequest {
    pub project_path: String,
    pub snapshot: GraphSnapshot,
    pub provider: CredentialProvider,
    pub model: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedGraphDiagram {
    pub diagram: Value,
    pub provider_id: String,
    pub model: String,
    pub usage: ProviderUsage,
}

#[tauri::command]
pub async fn generate_graph_diagram(
    request: GenerateGraphDiagramRequest,
    app_state: State<'_, AppState>,
) -> Result<GeneratedGraphDiagram, CommandError> {
    let root = std::fs::canonicalize(&request.project_path)
        .map_err(|error| invalid(format!("프로젝트 경로를 확인할 수 없습니다: {error}")))?;
    if root.to_string_lossy() != request.snapshot.project_path {
        return Err(invalid("프로젝트와 그래프 snapshot이 일치하지 않습니다."));
    }
    if request.snapshot.nodes.is_empty() {
        return Err(invalid("구조도를 생성할 graph node가 없습니다."));
    }
    let model = request.model.trim();
    if model.is_empty() || model.len() > 128 {
        return Err(invalid("모델 ID를 입력해 주세요."));
    }

    let graph_context = json!({
        "nodes": request.snapshot.nodes.iter().map(|node| json!({
            "id": node.node_id,
            "type": node.node_type,
            "name": node.name,
            "qualifiedName": node.qualified_name,
            "confidence": node.confidence,
        })).collect::<Vec<_>>(),
        "edges": request.snapshot.edges.iter().map(|edge| json!({
            "type": edge.edge_type,
            "source": edge.source_node_id,
            "target": edge.target_node_id,
            "targetName": edge.target_name,
            "confidence": edge.confidence,
        })).collect::<Vec<_>>(),
    });
    let context = format!(
        "Create a hierarchical architecture diagram from analyzer facts only.\nReturn JSON exactly as {{\"title\":string,\"nodes\":[{{\"id\":string,\"label\":string,\"type\":string,\"layer\":number}}],\"edges\":[{{\"source\":string,\"target\":string,\"label\":string}}]}}.\nUse 0 for roots and increasing integers downstream. Do not invent nodes or edges.\nAnalyzer graph:\n{}",
        serde_json::to_string(&graph_context)
            .map_err(|_| internal("graph context를 만들 수 없습니다."))?
    );
    let first_node = request.snapshot.nodes.first().unwrap();
    let review_request = ReviewRequest {
        selection: ReviewSelection::GraphNode {
            node_id: first_node.node_id.clone(),
            label: "전체 analyzer graph 구조도".to_owned(),
            range: None,
        },
        context,
        model: model.to_owned(),
        max_context_tokens: 32_000,
        max_output_tokens: Some(8_192),
    };
    let provider: Box<dyn AiProvider> = match request.provider {
        CredentialProvider::OpenAi => {
            if crate::credentials::openai_subscription_status()
                .map_err(|_| internal("OpenAI 인증 상태를 확인할 수 없습니다."))?
                .inference_enabled
            {
                Box::new(OpenAiProvider::from_subscription().map_err(provider_error)?)
            } else {
                Box::new(OpenAiProvider::from_keyring().map_err(provider_error)?)
            }
        }
        CredentialProvider::Anthropic => {
            Box::new(AnthropicProvider::from_keyring().map_err(provider_error)?)
        }
        CredentialProvider::ClaudeCode => Box::new(ClaudeCodeProvider::new(root)),
        CredentialProvider::GitHub => {
            return Err(invalid("GitHub는 AI provider로 사용할 수 없습니다."));
        }
    };
    validate_request(&review_request, &provider.descriptor()).map_err(provider_error)?;
    let task = app_state.register_task()?;
    let response = provider
        .review(&review_request, &task, RetryPolicy::default())
        .await
        .map_err(provider_error)?;
    let diagram = response
        .structured_output
        .get("diagram")
        .cloned()
        .unwrap_or(response.structured_output);
    validate_diagram(&diagram)?;
    Ok(GeneratedGraphDiagram {
        diagram,
        provider_id: response.provider_id,
        model: response.model,
        usage: response.usage,
    })
}

fn validate_diagram(value: &Value) -> Result<(), CommandError> {
    let nodes = value
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("AI 구조도 응답에 nodes가 없습니다."))?;
    let edges = value
        .get("edges")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("AI 구조도 응답에 edges가 없습니다."))?;
    let ids = nodes
        .iter()
        .filter_map(|node| node.get("id").and_then(Value::as_str))
        .collect::<std::collections::BTreeSet<_>>();
    if ids.len() != nodes.len()
        || nodes
            .iter()
            .any(|node| node.get("layer").and_then(Value::as_i64).is_none())
    {
        return Err(invalid("AI 구조도 node 형식이 올바르지 않습니다."));
    }
    if edges.iter().any(|edge| {
        edge.get("source")
            .and_then(Value::as_str)
            .map(|id| !ids.contains(id))
            .unwrap_or(true)
            || edge
                .get("target")
                .and_then(Value::as_str)
                .map(|id| !ids.contains(id))
                .unwrap_or(true)
    }) {
        return Err(invalid("AI 구조도 edge가 존재하지 않는 node를 가리킵니다."));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::InvalidInput, message)
}
fn internal(message: impl Into<String>) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, message)
}
fn provider_error(error: ProviderError) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, error.message)
}
