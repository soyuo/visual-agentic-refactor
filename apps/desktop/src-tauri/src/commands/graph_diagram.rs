use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::State;

use crate::ai_provider::{
    AiProvider, ProviderError, ProviderUsage, RetryPolicy, ReviewRequest, ReviewSelection,
    validate_request,
};
use crate::anthropic_provider::AnthropicProvider;
use crate::claude_code_provider::ClaudeCodeProvider;
use crate::codex_provider::CodexProvider;
use crate::command_error::{CommandError, CommandErrorCode};
use crate::credentials::CredentialProvider;
use crate::knowledge_graph::GraphSnapshot;
use crate::openai_provider::OpenAiProvider;
use crate::persistence::PersistenceState;
use crate::state::AppState;

const MAX_GRAPH_CONTEXT_CHARS: usize = 900_000;
const MAX_GRAPH_NODE_CONTEXT_CHARS: usize = 600_000;

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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetGraphDiagramRequest {
    pub project_path: String,
    pub snapshot_id: String,
    pub provider: CredentialProvider,
    pub model: String,
}

#[tauri::command]
pub fn get_graph_diagram(
    request: GetGraphDiagramRequest,
    persistence: State<'_, PersistenceState>,
) -> Result<Option<GeneratedGraphDiagram>, CommandError> {
    let stored = persistence
        .database()?
        .load_graph_diagram(
            &request.project_path,
            &request.snapshot_id,
            provider_id(request.provider),
            request.model.trim(),
        )
        .map_err(|error| internal(error))?;
    Ok(stored.map(|item| GeneratedGraphDiagram {
        diagram: item.diagram,
        provider_id: item.provider_id,
        model: item.model,
        usage: ProviderUsage::default(),
    }))
}

#[tauri::command]
pub async fn generate_graph_diagram(
    request: GenerateGraphDiagramRequest,
    app_state: State<'_, AppState>,
    persistence: State<'_, PersistenceState>,
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

    let (graph_context, truncated) = compact_graph_context(&request.snapshot);
    let _ = truncated;
    let context = format!(
        "이 프로젝트의 시스템 아키텍처를 좌측에서 우측으로 이어지는 계층형 다이어그램으로 그려줘. analyzer가 제공한 실제 노드와 관계만 사용하고, 루트는 layer 0, 의존 대상은 더 큰 layer에 배치해. 연결 관계를 빠뜨리지 말고, 아래 JSON 형식으로만 반환해: {{\"title\":string,\"nodes\":[{{\"id\":string,\"label\":string,\"type\":string,\"layer\":number}}],\"edges\":[{{\"source\":string,\"target\":string,\"label\":string}}]}}.\nAnalyzer facts:\n{}",
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
        output_schema: Some(diagram_schema()),
    };
    let provider_key = provider_id(request.provider);
    let provider = build_provider(request.provider, root)?;
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
    let allowed_node_ids = request
        .snapshot
        .nodes
        .iter()
        .map(|node| node.node_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    validate_diagram(&diagram, &allowed_node_ids)?;
    persistence
        .database()?
        .save_graph_diagram(
            &request.project_path,
            &request.snapshot.summary.snapshot_id,
            provider_key,
            model,
            &diagram,
        )
        .map_err(internal)?;
    Ok(GeneratedGraphDiagram {
        diagram,
        provider_id: response.provider_id,
        model: response.model,
        usage: response.usage,
    })
}

pub(crate) fn build_provider(
    provider: CredentialProvider,
    root: std::path::PathBuf,
) -> Result<Box<dyn AiProvider>, CommandError> {
    Ok(match provider {
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
        CredentialProvider::Codex => Box::new(CodexProvider::new(root)),
        CredentialProvider::GitHub => {
            return Err(invalid("GitHub는 AI provider로 사용할 수 없습니다."));
        }
    })
}

pub(crate) fn provider_id(provider: CredentialProvider) -> &'static str {
    match provider {
        CredentialProvider::OpenAi => "openai",
        CredentialProvider::Anthropic => "anthropic",
        CredentialProvider::ClaudeCode => "claude-code",
        CredentialProvider::Codex => "codex",
        CredentialProvider::GitHub => "github",
    }
}

fn compact_graph_context(snapshot: &GraphSnapshot) -> (Value, bool) {
    let mut nodes = Vec::new();
    let mut selected_ids = std::collections::BTreeSet::new();
    let mut node_chars = 2;
    let mut truncated = false;

    for node in &snapshot.nodes {
        let value = json!({
            "id": node.node_id,
            "type": node.node_type,
            "name": node.name,
            "qualifiedName": node.qualified_name,
            "confidence": node.confidence,
        });
        let item_chars = serde_json::to_string(&value).map_or(0, |text| text.chars().count());
        if node_chars + item_chars + 1 > MAX_GRAPH_NODE_CONTEXT_CHARS {
            truncated = true;
            continue;
        }
        node_chars += item_chars + 1;
        selected_ids.insert(node.node_id.clone());
        nodes.push(value);
    }

    let mut edges = Vec::new();
    let mut edge_chars = 2;
    for edge in &snapshot.edges {
        let Some(target) = edge.target_node_id.as_ref() else {
            truncated = true;
            continue;
        };
        if !selected_ids.contains(&edge.source_node_id) || !selected_ids.contains(target) {
            truncated = true;
            continue;
        }
        let value = json!({
            "type": edge.edge_type,
            "source": edge.source_node_id,
            "target": target,
            "targetName": edge.target_name,
            "confidence": edge.confidence,
        });
        let item_chars = serde_json::to_string(&value).map_or(0, |text| text.chars().count());
        if node_chars + edge_chars + item_chars + 1 > MAX_GRAPH_CONTEXT_CHARS {
            truncated = true;
            continue;
        }
        edge_chars += item_chars + 1;
        edges.push(value);
    }

    (
        json!({
            "nodes": nodes,
            "edges": edges,
            "sourceCounts": {
                "nodes": snapshot.nodes.len(),
                "edges": snapshot.edges.len(),
            },
        }),
        truncated,
    )
}

fn validate_diagram(
    value: &Value,
    allowed_node_ids: &std::collections::BTreeSet<&str>,
) -> Result<(), CommandError> {
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
    if nodes.iter().any(|node| {
        node.get("id")
            .and_then(Value::as_str)
            .is_none_or(|id| !allowed_node_ids.contains(id))
    }) {
        return Err(invalid("AI가 분석 결과에 없는 노드를 생성했습니다."));
    }
    if ids.len() != nodes.len()
        || nodes
            .iter()
            .any(|node| node.get("layer").and_then(Value::as_i64).is_none())
    {
        return Err(invalid("AI 구조도 node 형식이 올바르지 않습니다."));
    }
    let layers = nodes
        .iter()
        .filter_map(|node| node.get("layer").and_then(Value::as_i64))
        .collect::<std::collections::BTreeSet<_>>();
    if !edges.is_empty() && layers.len() < 2 {
        return Err(invalid("AI가 계층을 만들지 못했습니다."));
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
            || edge.get("source").and_then(Value::as_str).is_some_and(|source| {
                let source_layer = nodes.iter().find(|node| {
                    node.get("id").and_then(Value::as_str) == Some(source)
                }).and_then(|node| node.get("layer").and_then(Value::as_i64));
                let target_layer = edge.get("target").and_then(Value::as_str).and_then(|target| {
                    nodes.iter().find(|node| {
                        node.get("id").and_then(Value::as_str) == Some(target)
                    }).and_then(|node| node.get("layer").and_then(Value::as_i64))
                });
                matches!((source_layer, target_layer), (Some(source), Some(target)) if source >= target)
            })
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
pub(crate) fn provider_error(error: ProviderError) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, error.message)
}

fn diagram_schema() -> serde_json::Value {
    let string = serde_json::json!({ "type": "string" });
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["title", "nodes", "edges"],
        "properties": {
            "title": string,
            "nodes": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "label", "type", "layer"],
                    "properties": {
                        "id": string,
                        "label": string,
                        "type": string,
                        "layer": { "type": "integer" }
                    }
                }
            },
            "edges": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["source", "target", "label"],
                    "properties": {
                        "source": string,
                        "target": string,
                        "label": string
                    }
                }
            }
        }
    })
}
