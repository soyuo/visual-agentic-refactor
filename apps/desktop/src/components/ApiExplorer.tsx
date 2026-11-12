import { createSignal, For, Show } from "solid-js";
import type { GraphNode, GraphSnapshot } from "../lib/commands";

interface ApiExplorerProps {
  snapshot: GraphSnapshot | null;
  loading: boolean;
  error: string | null;
  projectAvailable: boolean;
  onAnalyze: () => void;
  onOpenSource: (relativePath: string, line: number | null) => void;
}

function endpointMethod(node: GraphNode): string {
  const metadata = node.metadata as { httpMethod?: string };
  return (metadata.httpMethod ?? node.name.split(" ")[0] ?? "ANY").toUpperCase();
}

function apiNodes(snapshot: GraphSnapshot | null): GraphNode[] {
  return snapshot?.nodes
    .filter((node) => node.nodeType === "apiEndpoint")
    .sort((left, right) => left.name.localeCompare(right.name)) ?? [];
}

export default function ApiExplorer(props: ApiExplorerProps) {
  const [query, setQuery] = createSignal("");
  const [method, setMethod] = createSignal("ALL");
  const allNodes = () => apiNodes(props.snapshot);
  const methods = () => ["ALL", ...new Set(allNodes().map((node) => endpointMethod(node)))];
  const nodes = () => {
    const needle = query().trim().toLowerCase();
    return allNodes().filter((node) =>
      (method() === "ALL" || endpointMethod(node) === method())
      && (!needle || `${node.name} ${node.qualifiedName ?? ""} ${node.filePath ?? ""}`.toLowerCase().includes(needle)));
  };

  return (
    <section class="graph-explorer" aria-labelledby="api-title" aria-busy={props.loading}>
      <header class="graph-heading">
        <div>
          <p class="eyebrow">CODE INTELLIGENCE</p>
          <h1 id="api-title">API 목록</h1>
          <p class="content-description">정적 분석으로 확인된 Spring·Ktor API route와 handler를 근거 위치와 함께 표시합니다.</p>
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
        <Show when={allNodes().length > 0} fallback={
          <div class="graph-state" role="status">
            <h2>확인된 API가 없습니다</h2>
            <p>Spring mapping annotation과 Ktor routing DSL(route, get, post…)을 기준으로 route를 찾습니다.</p>
          </div>
        }>
          <div class="api-toolbar">
            <input type="search" placeholder="경로, handler, 파일로 검색" aria-label="API 검색" value={query()} onInput={(event) => setQuery(event.currentTarget.value)} />
            <div class="scope-kind-switch" role="group" aria-label="HTTP 메서드 필터">
              <For each={methods()}>{(item) => <button type="button" classList={{ active: method() === item }} onClick={() => setMethod(item)}>{item === "ALL" ? "전체" : item}</button>}</For>
            </div>
            <span class="api-count">{nodes().length} / {allNodes().length}개</span>
          </div>
          <Show when={nodes().length === 0}><p class="muted-message" role="status">조건에 맞는 API가 없습니다.</p></Show>
          <div class="api-list" role="list" aria-label="분석된 API 목록">
            <For each={nodes()}>{(node) => {
              const metadata = node.metadata as { httpMethod?: string; path?: string | null; responseType?: string | null; framework?: string };
              const evidence = node.sourceEvidence;
              return (
                <article class="api-card" role="listitem">
                  <div class="api-card-heading">
                    <span class={`http-method http-${endpointMethod(node).toLowerCase()}`}>{endpointMethod(node)}</span>
                    <h2>{metadata.path ?? node.name}</h2>
                    <span class="api-framework">{metadata.framework === "ktor" ? "Ktor" : "Spring"}</span>
                    <span class={`confidence-badge ${node.confidence}`}>{node.confidence === "confirmed" ? "확인" : node.confidence === "inferred" ? "추정" : "불명"}</span>
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
