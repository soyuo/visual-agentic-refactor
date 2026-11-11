import assert from "node:assert/strict";
import test from "node:test";
import {
  buildGraphViewModel,
  MAX_VISIBLE_GRAPH_EDGES,
  MAX_VISIBLE_GRAPH_NODES,
  nextGraphNodeId,
} from "../src/lib/graphViewModel.ts";

const evidence = {
  relativePath: "src/main/java/sample/Controller.java",
  startByte: 10,
  endByte: 20,
  startLine: 2,
  startColumn: 1,
  endLine: 2,
  endColumn: 11,
};

function node(nodeId, nodeType, name) {
  return {
    nodeId,
    nodeType,
    name,
    qualifiedName: null,
    filePath: evidence.relativePath,
    sourceEvidence: evidence,
    language: "java",
    metadata: {},
    confidence: "confirmed",
  };
}

function edge(edgeId, edgeType, sourceNodeId, targetNodeId, confidence = "confirmed", targetName = null) {
  return { edgeId, edgeType, sourceNodeId, targetNodeId, targetName, evidence, confidence };
}

function snapshot(nodes, edges) {
  return {
    projectPath: "C:/fixture",
    snapshotId: "fixture-1",
    headSha: null,
    analyzerVersion: "fixture",
    sourceHashes: {},
    settings: {},
    completeness: "complete",
    analyzedAtUnixMs: 1,
    nodes,
    edges,
    diagnostics: [],
  };
}

test("API view selects endpoint relations and their connected nodes", () => {
  const fixture = snapshot(
    [
      node("controller", "class", "ItemController"),
      node("endpoint", "apiEndpoint", "GET /items"),
      node("dto", "dto", "ItemDto"),
      node("unrelated", "method", "unrelated"),
    ],
    [
      edge("route", "exposes", "controller", "endpoint"),
      edge("response", "usesDto", "endpoint", "dto"),
      edge("call", "calls", "controller", "unrelated"),
    ],
  );

  const result = buildGraphViewModel(fixture, "api");
  assert.deepEqual(result.edges.map(({ edgeId }) => edgeId), ["route", "response"]);
  assert.deepEqual(new Set(result.nodes.map(({ nodeId }) => nodeId)), new Set(["controller", "endpoint", "dto"]));
});

test("dependency view represents unresolved targets and retains their confidence", () => {
  const fixture = snapshot(
    [node("controller", "class", "ItemController")],
    [edge("inject", "injects", "controller", null, "unknown", "LegacyService")],
  );

  const result = buildGraphViewModel(fixture, "dependencies");
  assert.equal(result.nodes.length, 2);
  assert.equal(result.nodes[1].name, "LegacyService");
  assert.equal(result.nodes[1].confidence, "unknown");
  assert.equal(result.edges[0].targetNodeId, "unknown:inject");
});

test("graph view caps rendered nodes and reports omitted results", () => {
  const nodes = Array.from({ length: MAX_VISIBLE_GRAPH_NODES + 3 }, (_, index) =>
    node(`class-${index}`, "class", `Class${index}`),
  );
  const result = buildGraphViewModel(snapshot(nodes, []), "architecture");

  assert.equal(result.nodes.length, MAX_VISIBLE_GRAPH_NODES);
  assert.equal(result.omittedNodeCount, 3);
});

test("graph view caps rendered edges and reports omitted relationships", () => {
  const edges = Array.from({ length: MAX_VISIBLE_GRAPH_EDGES + 5 }, (_, index) =>
    edge(`relation-${index}`, "calls", "source", "target"),
  );
  const fixture = snapshot(
    [node("source", "method", "loadItems"), node("target", "method", "findItems")],
    edges,
  );
  const result = buildGraphViewModel(fixture, "flow");

  assert.equal(result.edges.length, MAX_VISIBLE_GRAPH_EDGES);
  assert.equal(result.omittedEdgeCount, 5);
});

test("graph view removes duplicate edge IDs deterministically", () => {
  const fixture = snapshot(
    [node("source", "method", "loadItems"), node("target", "method", "findItems")],
    [edge("same", "calls", "source", "target"), edge("same", "calls", "source", "target")],
  );
  const result = buildGraphViewModel(fixture, "flow");
  assert.deepEqual(result.edges.map(({ edgeId }) => edgeId), ["same"]);
  assert.equal(result.omittedEdgeCount, 0);
});

test("keyboard node navigation wraps in both directions", () => {
  const ids = ["first", "second", "third"];
  assert.equal(nextGraphNodeId(ids, "second", 1), "third");
  assert.equal(nextGraphNodeId(ids, "first", -1), "third");
  assert.equal(nextGraphNodeId([], "first", 1), null);
});
