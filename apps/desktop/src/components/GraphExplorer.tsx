import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import type { GeneratedGraphDiagram, GraphDiagramNode, GraphNode, GraphSnapshot } from "../lib/commands";
import {
  buildGraphViewModel,
  nextGraphNodeId,
  type GraphViewKind,
} from "../lib/graphViewModel";

interface GraphExplorerProps {
  snapshot: GraphSnapshot | null;
  loading: boolean;
  error: string | null;
  projectAvailable: boolean;
  onAnalyze: () => void;
  onOpenSource: (relativePath: string, line: number | null) => void;
  onSelectionChanged: (nodeId: string | null) => void;
  onGenerateAiDiagram: (provider: "openai" | "anthropic" | "claudeCode", model: string) => Promise<GeneratedGraphDiagram>;
}

const views: { id: GraphViewKind; label: string }[] = [
  { id: "architecture", label: "아키텍처" },
  { id: "api", label: "API" },
  { id: "dependencies", label: "의존성" },
  { id: "flow", label: "흐름" },
];

function confidenceLabel(confidence: GraphNode["confidence"]): string {
  if (confidence === "confirmed") return "확정";
  if (confidence === "inferred") return "추론";
  return "미확인";
}

function sourceLabel(node: GraphNode): string {
  return node.filePath ?? node.sourceEvidence?.relativePath ?? "소스 위치 없음";
}

function sourceRangeLabel(node: GraphNode): string {
  const evidence = node.sourceEvidence;
  return evidence
    ? `${sourceLabel(node)}:${evidence.startLine}:${evidence.startColumn}–${evidence.endLine}:${evidence.endColumn}`
    : sourceLabel(node);
}

function hierarchicalPositions(nodes: GraphNode[], edges: { sourceNodeId: string; targetNodeId: string | null }[]) {
  const nodeIds = new Set(nodes.map((node) => node.nodeId));
  const incoming = new Map<string, string[]>();
  const outgoing = new Map<string, string[]>();
  for (const node of nodes) {
    incoming.set(node.nodeId, []);
    outgoing.set(node.nodeId, []);
  }
  for (const edge of edges) {
    if (!edge.targetNodeId || !nodeIds.has(edge.sourceNodeId) || !nodeIds.has(edge.targetNodeId)) continue;
    outgoing.get(edge.sourceNodeId)!.push(edge.targetNodeId);
    incoming.get(edge.targetNodeId)!.push(edge.sourceNodeId);
  }

  const depth = new Map<string, number>();
  const queue = nodes.filter((node) => incoming.get(node.nodeId)!.length === 0).map((node) => node.nodeId);
  queue.forEach((nodeId) => depth.set(nodeId, 0));
  for (let cursor = 0; cursor < queue.length; cursor += 1) {
    const sourceId = queue[cursor];
    for (const targetId of outgoing.get(sourceId) ?? []) {
      const nextDepth = (depth.get(sourceId) ?? 0) + 1;
      if (!depth.has(targetId)) queue.push(targetId);
      depth.set(targetId, Math.max(depth.get(targetId) ?? 0, nextDepth));
    }
  }

  // Cycles have no root. Keep them visible by assigning each remaining component
  // to the first layer after the deepest reachable node.
  const deepest = Math.max(0, ...depth.values());
  for (const node of nodes) {
    if (!depth.has(node.nodeId)) depth.set(node.nodeId, deepest + 1);
  }

  const layers = new Map<number, GraphNode[]>();
  for (const node of nodes) {
    const layer = layers.get(depth.get(node.nodeId)!) ?? [];
    layer.push(node);
    layers.set(depth.get(node.nodeId)!, layer);
  }

  const positions = new Map<string, { node: GraphNode; x: number; y: number }>();
  for (const [layerIndex, layer] of layers) {
    const layerHeight = Math.max(86, 540 / Math.max(1, layer.length));
    layer.forEach((node, index) => positions.set(node.nodeId, {
      node,
      x: 100 + layerIndex * 180,
      y: 30 + layerHeight * (index + 0.5),
    }));
  }
  return nodes.map((node) => positions.get(node.nodeId)!);
}

function edgeEndpoints(source: { x: number; y: number }, target: { x: number; y: number }) {
  const dx = target.x - source.x;
  const dy = target.y - source.y;
  if (dx === 0 && dy === 0) {
    return {
      x1: source.x + 42,
      y1: source.y - 22,
      x2: target.x + 42,
      y2: target.y + 22,
    };
  }
  const scale = 1 / Math.max(Math.abs(dx) / 42, Math.abs(dy) / 22);
  return {
    x1: source.x + dx * scale,
    y1: source.y + dy * scale,
    x2: target.x - dx * scale,
    y2: target.y - dy * scale,
  };
}

function GraphExplorer(props: GraphExplorerProps) {
  const [view, setView] = createSignal<GraphViewKind>("architecture");
  const [selectedId, setSelectedId] = createSignal("");
  const [zoom, setZoom] = createSignal(1);
  const [offset, setOffset] = createSignal({ x: 0, y: 0 });
  const [dragOrigin, setDragOrigin] = createSignal<{ x: number; y: number } | null>(null);
  const [aiProvider, setAiProvider] = createSignal<"openai" | "anthropic" | "claudeCode">("openai");
  const [aiModel, setAiModel] = createSignal("");
  const [aiDiagram, setAiDiagram] = createSignal<GeneratedGraphDiagram | null>(null);
  const [aiBusy, setAiBusy] = createSignal(false);
  const [aiError, setAiError] = createSignal<string | null>(null);
  const nodeElements = new Map<string, SVGGElement>();
  const model = createMemo(() => props.snapshot ? buildGraphViewModel(props.snapshot, view()) : null);
  const selectedNode = createMemo(() => model()?.nodes.find((node) => node.nodeId === selectedId()) ?? null);
  const indexedNodes = createMemo(() => {
    const graph = model();
    return graph ? hierarchicalPositions(graph.nodes, graph.edges) : [];
  });
  const positions = createMemo(() => new Map(indexedNodes().map((item) => [item.node.nodeId, item])));
  const selectedEdges = createMemo(() => model()?.edges.filter((edge) =>
    edge.sourceNodeId === selectedId() || edge.targetNodeId === selectedId()) ?? []);

  createEffect(() => props.onSelectionChanged(selectedNode()?.nodeId ?? null));

  createEffect(() => {
    const nodes = model()?.nodes ?? [];
    const visibleIds = new Set(nodes.map((node) => node.nodeId));
    for (const nodeId of nodeElements.keys()) {
      if (!visibleIds.has(nodeId)) nodeElements.delete(nodeId);
    }
    if (!nodes.some((node) => node.nodeId === selectedId())) {
      setSelectedId(nodes[0]?.nodeId ?? "");
    }
  });

  function selectRelative(node: GraphNode, direction: -1 | 1) {
    const ids = model()?.nodes.map((item) => item.nodeId) ?? [];
    const nextId = nextGraphNodeId(ids, node.nodeId, direction) ?? node.nodeId;
    setSelectedId(nextId);
    requestAnimationFrame(() => nodeElements.get(nextId)?.focus());
  }

  function handleNodeKeyDown(event: KeyboardEvent, node: GraphNode) {
    if (event.key === "ArrowRight" || event.key === "ArrowDown") {
      event.preventDefault();
      selectRelative(node, 1);
    } else if (event.key === "ArrowLeft" || event.key === "ArrowUp") {
      event.preventDefault();
      selectRelative(node, -1);
    } else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      setSelectedId(node.nodeId);
    }
  }

  function onPointerDown(event: PointerEvent) {
    if ((event.target as Element).closest("[data-graph-node]")) return;
    setDragOrigin({ x: event.clientX - offset().x, y: event.clientY - offset().y });
    (event.currentTarget as SVGSVGElement).setPointerCapture(event.pointerId);
  }

  function onPointerMove(event: PointerEvent) {
    const origin = dragOrigin();
    if (origin) setOffset({ x: event.clientX - origin.x, y: event.clientY - origin.y });
  }

  async function generateAiDiagram() {
    if (!props.snapshot || !aiModel().trim() || aiBusy()) return;
    if (!window.confirm("analyzer 구조 메타데이터만 선택한 AI 제공자에게 전송해 계층형 구조도를 생성합니다. 소스 원문과 API 키는 전송하지 않습니다. 계속할까요?")) return;
    setAiBusy(true); setAiError(null);
    try { setAiDiagram(await props.onGenerateAiDiagram(aiProvider(), aiModel().trim())); }
    catch (error) { setAiError(error instanceof Error ? error.message : String(error)); }
    finally { setAiBusy(false); }
  }

  const aiPositions = createMemo(() => {
    const diagram = aiDiagram()?.diagram;
    const result = new Map<string, { node: GraphDiagramNode; x: number; y: number }>();
    if (!diagram) return result;
    const layers = new Map<number, GraphDiagramNode[]>();
    for (const node of diagram.nodes) {
      const layer = layers.get(node.layer) ?? [];
      layer.push(node); layers.set(node.layer, layer);
    }
    for (const [layer, nodes] of layers) nodes.forEach((node, index) => result.set(node.id, { node, x: 110 + layer * 190, y: 55 + index * 86 }));
    return result;
  });

  return (
    <section class="graph-explorer" aria-labelledby="graph-title" aria-busy={props.loading}>
      <header class="graph-heading">
        <div>
          <p class="eyebrow">CODE INTELLIGENCE</p>
          <h1 id="graph-title">지식 그래프</h1>
          <p class="content-description">프로젝트 구조와 코드 관계를 탐색하고 근거 위치로 이동합니다.</p>
        </div>
        <button class="primary-button" type="button" disabled={!props.projectAvailable || props.loading} onClick={props.onAnalyze}>
          {props.loading ? "분석 중…" : "그래프 분석"}
        </button>
      </header>

      <Show when={props.error}><p class="project-error" role="alert">{props.error}</p></Show>
      <Show when={props.loading && props.snapshot}><p role="status">그래프를 불러오거나 분석하고 있습니다.</p></Show>
      <Show when={props.snapshot} fallback={
        <div class="graph-state" aria-live="polite">
          <h2>{props.loading ? "그래프를 불러오는 중입니다" : "저장된 그래프가 없습니다"}</h2>
          <p>{props.loading ? "잠시 기다려 주세요." : "프로젝트를 열고 분석하면 관계 그래프가 여기에 표시됩니다."}</p>
        </div>
      }>
        {(snapshot) => <>
          <div class="graph-toolbar" role="group" aria-label="그래프 보기와 화면 조작">
            <div class="graph-view-tabs" role="group" aria-label="그래프 종류">
              <For each={views}>{(item) => (
                <button type="button" aria-pressed={view() === item.id} class="graph-view-tab"
                  classList={{ selected: view() === item.id }} onClick={() => { setView(item.id); setSelectedId(""); }}>
                  {item.label}
                </button>
              )}</For>
            </div>
            <div class="graph-zoom-controls" aria-label="확대/축소">
              <button type="button" aria-label="축소" onClick={() => setZoom((value) => Math.max(0.5, value - 0.15))}>−</button>
              <span aria-live="polite">{Math.round(zoom() * 100)}%</span>
              <button type="button" aria-label="확대" onClick={() => setZoom((value) => Math.min(2.5, value + 0.15))}>＋</button>
              <button type="button" onClick={() => { setZoom(1); setOffset({ x: 0, y: 0 }); }}>초기화</button>
            </div>
          </div>

          <Show when={model()?.nodes.length} fallback={
            <div class="graph-state" aria-live="polite">
              <h2>이 보기에서 표시할 관계가 없습니다</h2>
              <p>다른 그래프 종류를 선택하거나 프로젝트를 다시 분석해 보세요.</p>
            </div>
          }>
            <div class="graph-ai-controls">
              <select value={aiProvider()} onChange={(event) => setAiProvider(event.currentTarget.value as "openai" | "anthropic" | "claudeCode")} aria-label="AI 제공자">
                <option value="openai">OpenAI</option>
                <option value="anthropic">Anthropic</option>
                <option value="claudeCode">Claude Code</option>
              </select>
              <input value={aiModel()} onInput={(event) => setAiModel(event.currentTarget.value)} placeholder="모델 ID" aria-label="AI 모델 ID" />
              <button class="secondary-button" type="button" disabled={aiBusy() || !aiModel().trim()} onClick={() => void generateAiDiagram()}>{aiBusy() ? "AI 생성 중..." : "AI로 계층 구조도 생성"}</button>
            </div>
            <Show when={aiError()}><p class="project-error" role="alert">{aiError()}</p></Show>
            <Show when={aiDiagram()}>{(generated) => <section class="graph-ai-diagram" aria-label="AI 계층 구조도">
              <h2>{generated().diagram.title ?? "AI 계층 구조도"}</h2>
              <svg class="graph-canvas" viewBox="0 0 1000 600" role="img" aria-label="AI가 생성한 계층형 구조도">
                <For each={generated().diagram.edges}>{(edge) => { const source = aiPositions().get(edge.source); const target = aiPositions().get(edge.target); return source && target ? <g><line {...edgeEndpoints(source, target)} marker-end="url(#graph-arrow)" /><title>{edge.label}</title></g> : null; }}</For>
                <For each={generated().diagram.nodes}>{(node) => { const position = aiPositions().get(node.id); return position ? <g class="graph-node" transform={`translate(${position.x} ${position.y})`}><rect x="-70" y="-24" width="140" height="48" rx="10" /><text text-anchor="middle" y="4">{node.label.slice(0, 22)}</text><title>{node.label} · {node.type}</title></g> : null; }}</For>
              </svg>
            </section>}</Show>
            <div class="graph-layout">
              <div class="graph-canvas-wrap">
                <svg class="graph-canvas" viewBox="0 0 1000 600" role="group" aria-label={`${views.find((item) => item.id === view())?.label} 그래프`}
                  onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={() => setDragOrigin(null)} onPointerCancel={() => setDragOrigin(null)}>
                  <g transform={`translate(${offset().x} ${offset().y}) translate(500 300) scale(${zoom()}) translate(-500 -300)`}>
                    <defs>
                      <marker id="graph-arrow" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto" markerUnits="strokeWidth">
                        <path d="M0,0 L8,4 L0,8 z" fill="#9ca9bc" />
                      </marker>
                    </defs>
                    <For each={model()?.edges}>{(edge) => {
                      const source = positions().get(edge.sourceNodeId);
                      const target = edge.targetNodeId ? positions().get(edge.targetNodeId) : undefined;
                      return source && target ? <g class={`graph-edge ${edge.confidence}`}>
                        <line {...edgeEndpoints(source, target)} marker-end="url(#graph-arrow)" />
                        <title>{edge.edgeType}: {source.node.name} → {target.node.name}</title>
                      </g> : null;
                    }}</For>
                    <For each={indexedNodes()}>{({ node, x, y }) => (
                      <g data-graph-node="true" class={`graph-node ${node.confidence}`} classList={{ selected: selectedId() === node.nodeId }}
                        transform={`translate(${x} ${y})`} role="button" tabindex={selectedId() === node.nodeId || (!selectedId() && node === model()?.nodes[0]) ? 0 : -1}
                        aria-pressed={selectedId() === node.nodeId} aria-label={`${node.name}, ${node.nodeType}, ${confidenceLabel(node.confidence)}, ${sourceLabel(node)}`}
                        ref={(element) => nodeElements.set(node.nodeId, element)}
                        onFocus={() => setSelectedId(node.nodeId)} onClick={() => setSelectedId(node.nodeId)} onKeyDown={(event) => handleNodeKeyDown(event, node)}>
                        <rect x="-42" y="-22" width="84" height="44" rx="10" />
                        <text text-anchor="middle" y="4">{node.name.length > 13 ? `${node.name.slice(0, 12)}…` : node.name}</text>
                        <title>{node.name} · {node.nodeType} · {confidenceLabel(node.confidence)}</title>
                      </g>
                    )}</For>
                  </g>
                </svg>
                <p class="graph-canvas-hint">노드를 선택하세요. 배경을 끌어 이동하고 화살표 키로 노드를 탐색할 수 있습니다.</p>
                <div class="graph-legend" aria-label="관계 신뢰도">
                  <span class="confirmed">확정</span><span class="inferred">추론</span><span class="unknown">미확인</span>
                </div>
                <Show when={(model()?.omittedNodeCount ?? 0) > 0 || (model()?.omittedEdgeCount ?? 0) > 0}>
                  <p class="graph-limit-note" role="status">화면 제한으로 노드 {model()?.omittedNodeCount}개, 관계 {model()?.omittedEdgeCount}개를 생략했습니다.</p>
                </Show>
                <p class="graph-completeness" role="status">분석 상태: {snapshot().completeness === "complete" ? "완료" : "일부 결과"}</p>
              </div>

              <aside class="graph-inspector" aria-label="선택한 그래프 항목">
                <Show when={selectedNode()} fallback={<p class="graph-inspector-empty">그래프 노드를 선택하면 상세 정보가 표시됩니다.</p>}>
                  {(node) => <>
                    <p class="eyebrow">선택 항목</p>
                    <h2>{node().name}</h2>
                    <dl>
                      <dt>종류</dt><dd>{node().nodeType}</dd>
                      <dt>신뢰도</dt><dd><span class={`confidence-badge ${node().confidence}`}>{confidenceLabel(node().confidence)}</span></dd>
                      <dt>정규 이름</dt><dd>{node().qualifiedName ?? "정보 없음"}</dd>
                    </dl>
                    <Show when={node().filePath || node().sourceEvidence}>
                      <p class="graph-source-path">{sourceRangeLabel(node())}</p>
                      <button class="secondary-button" type="button" onClick={() => props.onOpenSource(
                        node().filePath ?? node().sourceEvidence!.relativePath,
                        node().sourceEvidence?.startLine ?? null,
                      )}>파일 위치 열기</button>
                    </Show>
                    <h3>연결 관계 ({selectedEdges().length})</h3>
                    <ul class="graph-relation-list">
                      <For each={selectedEdges()}>{(edge) => {
                        const target = edge.sourceNodeId === node().nodeId
                          ? positions().get(edge.targetNodeId ?? "")?.node.name ?? edge.targetName ?? "미해결 대상"
                          : positions().get(edge.sourceNodeId)?.node.name ?? "연결 항목";
                        return <li>
                          <strong>{edge.edgeType}</strong><span>{target}</span>
                          <small>{confidenceLabel(edge.confidence)} · {edge.evidence.relativePath}:{edge.evidence.startLine}</small>
                          <button type="button" class="text-button" onClick={() => props.onOpenSource(edge.evidence.relativePath, edge.evidence.startLine)}>근거 위치 열기</button>
                        </li>;
                      }}</For>
                    </ul>
                  </>}
                </Show>
              </aside>
            </div>
          </Show>
        </>}
      </Show>
    </section>
  );
}

export default GraphExplorer;
