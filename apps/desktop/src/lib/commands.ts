import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type CommandErrorCode = "invalidInput" | "cancelled" | "internal";

export interface CommandErrorPayload {
  code: CommandErrorCode;
  message: string;
}

export class AppCommandError extends Error {
  constructor(
    readonly code: CommandErrorCode,
    message: string,
  ) {
    super(message);
    this.name = "AppCommandError";
  }
}

export interface AppInfoResponse {
  appName: string;
  version: string;
}

export interface OpenProjectResponse {
  path: string;
  name: string;
  persistenceError: string | null;
}

export type SourceLanguage = "kotlin" | "java";
export type Confidence = "confirmed" | "inferred" | "unknown";
export type GraphCompleteness = "complete" | "partial";
export type GraphDiagnosticSeverity = "info" | "warning" | "error";

export type GraphNodeType =
  | "project"
  | "module"
  | "directory"
  | "file"
  | "package"
  | "symbol"
  | "class"
  | "interface"
  | "function"
  | "method"
  | "apiEndpoint"
  | "dto"
  | "dependency"
  | "externalService";

export type GraphEdgeType =
  | "contains"
  | "declares"
  | "imports"
  | "dependsOn"
  | "calls"
  | "implements"
  | "extends"
  | "inherits"
  | "injects"
  | "exposes"
  | "usesDto"
  | "accesses"
  | "references"
  | "controls"
  | "throws"
  | "annotatedBy";

export interface SourceRange {
  relativePath: string;
  startByte: number;
  endByte: number;
  startLine: number;
  startColumn: number;
  endLine: number;
  endColumn: number;
}

export interface GraphNode {
  nodeId: string;
  nodeType: GraphNodeType;
  name: string;
  qualifiedName: string | null;
  filePath: string | null;
  sourceEvidence: SourceRange | null;
  language: SourceLanguage | null;
  metadata: unknown;
  confidence: Confidence;
}

export interface GraphEdge {
  edgeId: string;
  edgeType: GraphEdgeType;
  sourceNodeId: string;
  targetNodeId: string | null;
  targetName: string | null;
  evidence: SourceRange;
  confidence: Confidence;
}

export interface GraphDiagnostic {
  code: string;
  severity: GraphDiagnosticSeverity;
  message: string;
  sourceEvidence: SourceRange | null;
}

export interface GraphSnapshotInput {
  snapshotId: string;
  headSha: string | null;
  analyzerVersion: string;
  sourceHashes: Record<string, string>;
  settings: unknown;
  completeness: GraphCompleteness;
  analyzedAtUnixMs: number | null;
  nodes: GraphNode[];
  edges: GraphEdge[];
  diagnostics: GraphDiagnostic[];
}

export interface GraphSnapshotSummary {
  snapshotId: string;
  headSha: string | null;
  analyzerVersion: string;
  sourceHashes: Record<string, string>;
  settings: unknown;
  completeness: GraphCompleteness;
  analyzedAtUnixMs: number;
}

export interface GraphSnapshot extends GraphSnapshotSummary {
  projectPath: string;
  nodes: GraphNode[];
  edges: GraphEdge[];
  diagnostics: GraphDiagnostic[];
}

export interface AnalyzeSpringProjectResponse {
  projectPath: string;
  snapshotId: string;
  completeness: GraphCompleteness;
  sourceFileCount: number;
  nodeCount: number;
  edgeCount: number;
  diagnosticCount: number;
}

export interface ScanOptions {
  /** Gitignore-style rules; a leading `!` can re-include a default exclusion. */
  ignorePatterns?: string[];
  maxDepth?: number;
  maxFileSizeBytes?: number;
}

export interface ScanProjectRequest {
  path: string;
  options?: ScanOptions;
}

export type ScanEntryKind = "directory" | "file";

export type ScanFileStatus =
  | "eligible"
  | "unsupportedType"
  | "tooLarge"
  | "binary";

export interface ScanEntry {
  relativePath: string;
  name: string;
  kind: ScanEntryKind;
  sizeBytes: number | null;
  modifiedUnixMs: number | null;
  extension: string | null;
  fileStatus: ScanFileStatus | null;
}

export interface ScanWarning {
  relativePath: string | null;
  message: string;
}

export interface ScanSummary {
  entries: number;
  files: number;
  directories: number;
  eligibleFiles: number;
  skippedFiles: number;
  depthLimit: number;
  fileSizeLimitBytes: number;
  depthLimitReached: boolean;
  entryLimitReached: boolean;
}

export type DetectionStatus = "detected" | "notDetected" | "uncertain";

export interface DetectionEvidence {
  relativePath: string;
  description: string;
}

export interface DetectionResult {
  status: DetectionStatus;
  message: string;
  evidence: DetectionEvidence[];
  evidenceOmittedCount: number;
}

export interface ProjectDetection {
  kotlin: DetectionResult;
  java: DetectionResult;
  springBoot: DetectionResult;
  gradle: DetectionResult;
}

export interface ScanProjectResponse {
  rootPath: string;
  entries: ScanEntry[];
  warnings: ScanWarning[];
  summary: ScanSummary;
  projectDetection: ProjectDetection;
  persistenceError: string | null;
}

export type GitStatusState = "available" | "gitUnavailable" | "notRepository" | "error";
export type GitFileChangeKind = "tracked" | "untracked";

export interface GitFileChange {
  path: string;
  kind: GitFileChangeKind;
  statusCode: string;
}

export interface GitStatusResponse {
  state: GitStatusState;
  branch: string | null;
  head: string | null;
  trackedCount: number;
  modifiedCount: number;
  untrackedCount: number;
  changes: GitFileChange[];
  omittedChangeCount: number;
  message: string;
}

export type TaskStatus = "running" | "completed" | "cancelled" | "failed";

export interface TaskProgressEvent {
  taskId: number;
  status: TaskStatus;
  completed: number;
  total: number | null;
  message: string | null;
}

export interface CancelTaskResponse {
  taskId: number;
  cancellationRequested: boolean;
}

const commandErrorCodes: readonly string[] = [
  "invalidInput",
  "cancelled",
  "internal",
];

function isCommandErrorPayload(value: unknown): value is CommandErrorPayload {
  if (typeof value !== "object" || value === null) return false;

  const payload = value as Record<string, unknown>;
  return (
    typeof payload.code === "string" &&
    commandErrorCodes.includes(payload.code) &&
    typeof payload.message === "string"
  );
}

export function toAppCommandError(error: unknown): AppCommandError {
  if (error instanceof AppCommandError) return error;
  if (isCommandErrorPayload(error)) {
    return new AppCommandError(error.code, error.message);
  }

  return new AppCommandError(
    "internal",
    "데스크톱 코어 명령을 처리하지 못했습니다.",
  );
}

export async function getAppInfo(): Promise<AppInfoResponse> {
  try {
    return await invoke<AppInfoResponse>("get_app_info");
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function saveGraphSnapshot(
  projectPath: string,
  snapshot: GraphSnapshotInput,
): Promise<void> {
  try {
    await invoke<void>("save_graph_snapshot", {
      request: { projectPath, snapshot },
    });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function listGraphSnapshots(
  projectPath: string,
): Promise<GraphSnapshotSummary[]> {
  try {
    return await invoke<GraphSnapshotSummary[]>("list_graph_snapshots", {
      request: { projectPath },
    });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function getGraphSnapshot(
  projectPath: string,
  snapshotId: string,
): Promise<GraphSnapshot | null> {
  try {
    return await invoke<GraphSnapshot | null>("get_graph_snapshot", {
      request: { projectPath, snapshotId },
    });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function analyzeSpringProject(
  request: ScanProjectRequest,
): Promise<AnalyzeSpringProjectResponse> {
  try {
    return await invoke<AnalyzeSpringProjectResponse>("analyze_spring_project", {
      request,
    });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function openProject(path: string): Promise<OpenProjectResponse> {
  try {
    return await invoke<OpenProjectResponse>("open_project", {
      request: { path },
    });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function getLastProject(): Promise<OpenProjectResponse | null> {
  try {
    return await invoke<OpenProjectResponse | null>("get_last_project");
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function scanProject(
  request: ScanProjectRequest,
): Promise<ScanProjectResponse> {
  try {
    return await invoke<ScanProjectResponse>("scan_project", { request });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function getGitStatus(path: string): Promise<GitStatusResponse> {
  try {
    return await invoke<GitStatusResponse>("get_git_status", { path });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function cancelTask(taskId: number): Promise<CancelTaskResponse> {
  try {
    return await invoke<CancelTaskResponse>("cancel_task", {
      request: { taskId },
    });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function listenToTaskProgress(
  onProgress: (event: TaskProgressEvent) => void,
): Promise<UnlistenFn> {
  try {
    return await listen<TaskProgressEvent>("task-progress", (event) => {
      onProgress(event.payload);
    });
  } catch (error) {
    throw toAppCommandError(error);
  }
}
