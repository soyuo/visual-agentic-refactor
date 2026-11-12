import { For, Show } from "solid-js";
import type { GraphNode, GraphSnapshot } from "../lib/commands";

interface ApiExplorerProps {
  snapshot: GraphSnapshot | null;
  loading: boolean;
  error: string | null;
  projectAvailable: boolean;
  onAnalyze: () => void;
  onOpenSource: (relativePath: string, line: number | null) => void;
}

function apiNodes(snapshot: GraphSnapshot | null): GraphNode[] {
  return snapshot?.nodes
    .filter((node) => node.nodeType === "apiEndpoint")
    .sort((left, right) => left.name.localeCompare(right.name)) ?? [];
}

export default function ApiExplorer(props: ApiExplorerProps) {
  const nodes = () => apiNodes(props.snapshot);

  return (
    <section class="graph-explorer" aria-labelledby="api-title" aria-busy={props.loading}>
      <header class="graph-heading">
        <div>
          <p class="eyebrow">CODE INTELLIGENCE</p>
          <h1 id="api-title">API 목록</h1>
          <p class="content-description">정적 분석으로 확인된 Spring API route와 handler를 source evidence와 함께 표시합니다.</p>
        </div>
        <button class="primary-button" type="button" disabled={!props.projectAvailable || props.loading} onClick={props.onAnalyze}>
          {props.loading ? "분석 중…" : "API 다시 분석"}
        </button>
      </header>
      <Show when={props.error}><p class="project-error" role="alert">{props.error}</p></Show>
      <Show when={!props.snapshot && !props.loading}>
        <div class="graph-state" role="status">
          <h2>저장된 분석 결과가 없습니다</h2>
          <p>프로젝트를 먼저 분석하면 API 목록이 표시됩니다.</p>
        </div>
      </Show>
      <Show when={props.loading}><p role="status">API 목록을 분석하고 있습니다…</p></Show>
      <Show when={props.snapshot && !props.loading}>
        <Show when={nodes().length > 0} fallback={
          <div class="graph-state" role="status">
            <h2>확인된 API가 없습니다</h2>
            <p>현재 분석기는 Spring mapping annotation을 기준으로 route를 찾습니다.</p>
          </div>
        }>
          <div class="api-list" role="list" aria-label="분석된 API 목록">
            <For each={nodes()}>{(node) => {
              const metadata = node.metadata as { httpMethod?: string; path?: string | null; responseType?: string | null };
              const evidence = node.sourceEvidence;
              return (
                <article class="finding-card" role="listitem">
                  <div class="review-finding-heading">
                    <h2>{node.name}</h2>
                    <span class={`confidence-badge ${node.confidence}`}>{node.confidence}</span>
                  </div>
                  <dl class="review-finding-meta">
                    <div><dt>handler</dt><dd>{node.qualifiedName ?? "정보 없음"}</dd></div>
                    <div><dt>경로</dt><dd>{metadata.path ?? "동적 경로"}</dd></div>
                    <div><dt>응답</dt><dd>{metadata.responseType ?? "정보 없음"}</dd></div>
                    {evidence && <div><dt>근거</dt><dd>{evidence.relativePath}:{evidence.startLine}</dd></div>}
                  </dl>
                  {evidence && <button class="text-button" type="button" onClick={() => props.onOpenSource(evidence.relativePath, evidence.startLine)}>근거 위치 열기</button>}
                </article>
              );
            }}</For>
          </div>
        </Show>
      </Show>
    </section>
  );
}
