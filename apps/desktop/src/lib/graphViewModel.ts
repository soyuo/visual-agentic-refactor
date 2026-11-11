import type { GraphEdge, GraphNode, GraphNodeType, GraphSnapshot } from "./commands";

export type GraphViewKind = "architecture" | "api" | "dependencies" | "flow";

export const MAX_VISIBLE_GRAPH_NODES = 60;
export const MAX_VISIBLE_GRAPH_EDGES = 300;
export const MAX_GRAPH_SOURCE_NODES = 100_000;
export const MAX_GRAPH_SOURCE_EDGES = 250_000;

const edgeTypes: Record<GraphViewKind, ReadonlySet<GraphEdge["edgeType"]>> = {
  architecture: new Set(["contains", "declares", "inherits", "extends", "implements"]),
  api: new Set(["exposes", "usesDto"]),
  dependencies: new Set(["imports", "dependsOn", "injects", "references"]),
  flow: new Set(["calls", "controls", "throws", "accesses"]),
};

const nodeTypes: Record<GraphViewKind, ReadonlySet<GraphNodeType>> = {
  architecture: new Set([
    "project", "module", "directory", "file", "package", "class", "interface",
    "function", "method", "symbol", "dto",
  ]),
  api: new Set(["apiEndpoint", "class", "interface", "method", "dto"]),
  dependencies: new Set([
    "class", "interface", "function", "method", "dependency", "externalService", "dto",
  ]),
  flow: new Set(["class", "function", "method", "apiEndpoint"]),
};

export interface GraphViewModel {
  nodes: GraphNode[];
  edges: GraphEdge[];
  omittedNodeCount: number;
  omittedEdgeCount: number;
}

export function buildGraphViewModel(
  snapshot: GraphSnapshot,
  view: GraphViewKind,
): GraphViewModel {
  const candidates = [...new Map(
    snapshot.edges
      .filter((edge) => edgeTypes[view].has(edge.edgeType))
      .map((edge) => [edge.edgeId, edge]),
  ).values()];
  const relevantIds = new Set<string>();
  const syntheticNodes = new Map<string, GraphNode>();

  for (const edge of candidates) {
    relevantIds.add(edge.sourceNodeId);
    if (edge.targetNodeId) {
      relevantIds.add(edge.targetNodeId);
    } else if (edge.targetName) {
      const nodeId = `unknown:${edge.edgeId}`;
      syntheticNodes.set(nodeId, {
        nodeId,
        nodeType: "externalService",
        name: edge.targetName,
        qualifiedName: edge.targetName,
        filePath: edge.evidence.relativePath,
        sourceEvidence: edge.evidence,
        language: null,
        metadata: { unresolved: true },
        confidence: edge.confidence,
      });
      relevantIds.add(nodeId);
    }
  }

  const typedNodes = snapshot.nodes.filter((node) => nodeTypes[view].has(node.nodeType));
  const preferredNodes = candidates.length > 0
    ? snapshot.nodes.filter((node) => relevantIds.has(node.nodeId))
    : typedNodes;
  const allNodes = [...preferredNodes, ...syntheticNodes.values()];
  const uniqueNodes = [...new Map(allNodes.map((node) => [node.nodeId, node])).values()];
  uniqueNodes.sort((left, right) => {
    const leftPath = left.filePath ?? "";
    const rightPath = right.filePath ?? "";
    return leftPath.localeCompare(rightPath) || left.name.localeCompare(right.name);
  });

  const nodes = uniqueNodes.slice(0, MAX_VISIBLE_GRAPH_NODES);
  const visibleIds = new Set(nodes.map((node) => node.nodeId));
  const edges = candidates
    .filter((edge) => {
      const targetId = edge.targetNodeId ?? (edge.targetName ? `unknown:${edge.edgeId}` : null);
      return visibleIds.has(edge.sourceNodeId) && targetId !== null && visibleIds.has(targetId);
    })
    .slice(0, MAX_VISIBLE_GRAPH_EDGES)
    .map((edge) => edge.targetNodeId
      ? edge
      : { ...edge, targetNodeId: `unknown:${edge.edgeId}` });

  return {
    nodes,
    edges,
    omittedNodeCount: Math.max(0, uniqueNodes.length - nodes.length),
    omittedEdgeCount: Math.max(0, candidates.length - edges.length),
  };
}

export function nextGraphNodeId(
  nodeIds: readonly string[],
  currentId: string,
  direction: -1 | 1,
): string | null {
  if (nodeIds.length === 0) return null;
  const currentIndex = nodeIds.indexOf(currentId);
  const start = currentIndex < 0 ? 0 : currentIndex;
  return nodeIds[(start + direction + nodeIds.length) % nodeIds.length] ?? null;
}
