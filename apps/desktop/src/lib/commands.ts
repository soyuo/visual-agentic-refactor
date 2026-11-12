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

export type CredentialProvider = "openai" | "anthropic" | "claudeCode" | "codex" | "github";
export interface AiModelOption { id: string; displayName: string; }
export async function listAiModels(provider: CredentialProvider): Promise<AiModelOption[]> {
  try { return await invoke<AiModelOption[]>("list_ai_models", { request: { provider } }); }
  catch (error) { throw toAppCommandError(error); }
}
export type LocalRuntimeKind = "llamaCpp" | "ollama";
export interface LocalLlmRequest { runtime: LocalRuntimeKind; executablePath: string; modelPath: string; prompt: string; timeoutMs?: number; args?: string[]; }
export interface LocalLlmResponse { text: string; structured: unknown; }

export interface ProviderCredentialStatus {
  provider: CredentialProvider;
  displayName: string;
  configured: boolean;
}

export interface GitHubProjectRequest { projectPath: string; }
export interface GitHubNumberRequest { projectPath: string; number: number; }
export interface GitHubRepository {
  owner: string;
  name: string;
  htmlUrl: string;
  defaultBranch: string;
  private: boolean;
}
export interface GitHubIssueOrPullRequest {
  number: number;
  title: string;
  state: string;
  htmlUrl: string;
  isPullRequest: boolean;
}

export interface OpenAiSubscriptionStatus {
  connected: boolean;
  inferenceEnabled: boolean;
  accountEmail: string | null;
}

export interface FixVerificationPolicy { timeoutMs: number; outputLimitBytes: number; networkDisabled: boolean }
export interface VerificationCandidate { id: string; command: string; args: string[]; reason: string }
export interface VerificationResult { id: string; command: string; args: string[]; status: string; exitCode: number | null; outputSummary: string; error: string | null; startedAtUnixMs: number; endedAtUnixMs: number }
export interface VerificationCandidates { taskId: string; candidates: VerificationCandidate[] }

export interface OpenProjectResponse {
  path: string;
  name: string;
  persistenceError: string | null;
}

export type SourceLanguage = "kotlin" | "java" | "javascript" | "typescript" | "python";
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

export interface GraphDiagramNode { id: string; label: string; type: string; layer: number; }
export interface GraphDiagramEdge { source: string; target: string; label: string; }
export interface GeneratedGraphDiagram { diagram: { title?: string; nodes: GraphDiagramNode[]; edges: GraphDiagramEdge[] }; providerId: string; model: string; usage: ProviderUsage; }
export interface GenerateGraphDiagramRequest { projectPath: string; snapshot: GraphSnapshot; provider: CredentialProvider; model: string; }
export interface GetGraphDiagramRequest { projectPath: string; snapshotId: string; provider: CredentialProvider; model: string; }
export async function generateGraphDiagram(request: GenerateGraphDiagramRequest): Promise<GeneratedGraphDiagram> {
  try { return await invoke<GeneratedGraphDiagram>("generate_graph_diagram", { request }); }
  catch (error) { throw toAppCommandError(error); }
}
export async function getGraphDiagram(request: GetGraphDiagramRequest): Promise<GeneratedGraphDiagram | null> {
  try { return await invoke<GeneratedGraphDiagram | null>("get_graph_diagram", { request }); }
  catch (error) { throw toAppCommandError(error); }
}

export interface IncrementalAnalysisPlan {
  reusableSnapshotId: string | null;
  reusable: boolean;
  changedPaths: string[];
  unchangedPaths: string[];
  addedPaths: string[];
  removedPaths: string[];
  reason: string | null;
}

export interface CompareGraphSnapshotRequest {
  projectPath: string;
  analyzerVersion: string;
  sourceHashes: Record<string, string>;
  settings: unknown;
}

export async function compareGraphSnapshot(request: CompareGraphSnapshotRequest): Promise<IncrementalAnalysisPlan> {
  return invoke<IncrementalAnalysisPlan>("compare_graph_snapshot", { request });
}

export type ReviewContextScope =
  | { file: { relativePath: string; range: SourceRange | null } }
  | { graphNode: { nodeId: string } };

export interface PreviewReviewContextRequest {
  projectPath: string;
  scope: ReviewContextScope;
  maxContextTokens: number;
}

export interface ReviewContextPreview {
  previewId: string;
  context: string;
  estimatedTokens: number;
  tokenBudget: number;
  includedFiles: string[];
  includedSourceSpans: RenderedSourceSpan[];
  omittedItems: string[];
  graphSnapshotId: string | null;
  graphCompleteness: string | null;
  tokenEstimateNote: string;
}

export interface ApproveReviewTransferRequest {
  projectPath: string;
  previewId: string;
  snapshotId: string;
  selection: ReviewFindingSelection;
  provider: CredentialProvider;
  model: string;
}

export interface RunAiReviewRequest extends ApproveReviewTransferRequest {
  requestId: string;
}

export interface AiReviewTaskStartedEvent {
  requestId: string;
  taskId: number;
}

export interface ProviderUsage {
  inputTokens: number | null;
  outputTokens: number | null;
}

export interface RenderedSourceSpan {
  relativePath: string;
  startLine: number;
  endLine: number;
  visibleText: string;
}

export type ReviewFindingCategory =
  | "correctness"
  | "security"
  | "performance"
  | "reliability"
  | "maintainability"
  | "testability"
  | "style"
  | "other";
export type ReviewFindingSeverity = "critical" | "high" | "medium" | "low" | "informational";
export type ReviewFindingConfidence = "high" | "medium" | "low";

export interface ReviewFindingEvidence {
  relativePath: string;
  startLine: number;
  startColumn: number;
  endLine: number;
  endColumn: number;
  excerpt: string;
}

export interface ReviewFinding {
  title: string;
  description: string;
  category: ReviewFindingCategory;
  severity: ReviewFindingSeverity;
  confidence: ReviewFindingConfidence;
  evidence: ReviewFindingEvidence[];
  affectedSymbols: string[];
  suggestion: string;
  testSuggestion: string;
}

export type ReviewFindingsValidationState = "complete" | "partial" | "invalid";
export type ReviewFindingValidationCode =
  | "invalidStructuredOutput"
  | "tooManyFindings"
  | "invalidFindingSchema"
  | "missingEvidence"
  | "invalidPath"
  | "outsideSelectedContext"
  | "missingSnapshotEvidence"
  | "sourceUnavailable"
  | "sourceChanged"
  | "sourceLimitExceeded"
  | "invalidRange"
  | "excerptMismatch"
  | "symbolNotFound"
  | "invalidSelection"
  | "snapshotMismatch"
  | "projectMismatch";

export interface ReviewFindingValidationDiagnostic {
  findingIndex: number | null;
  code: ReviewFindingValidationCode;
  message: string;
}

export interface ReviewFindingsValidationResult {
  state: ReviewFindingsValidationState;
  snapshotId: string | null;
  findings: ReviewFinding[];
  diagnostics: ReviewFindingValidationDiagnostic[];
}

export type FindingDecisionStatus = "pending" | "approved" | "rejected";

export interface StoredFindingDecision {
  status: FindingDecisionStatus;
  decidedAtUnixMs: number | null;
}

export interface StoredReviewSessionSummary {
  sessionId: string;
  snapshotId: string;
  validationState: ReviewFindingsValidationState;
  createdAtUnixMs: number;
  findingCount: number;
}

export interface StoredReviewFinding {
  findingId: string;
  finding: ReviewFinding;
  decision: StoredFindingDecision;
}

export interface StoredReviewSession extends StoredReviewSessionSummary {
  projectPath: string;
  diagnostics: ReviewFindingValidationDiagnostic[];
  findings: StoredReviewFinding[];
}

export type FixTaskStatus = "queued" | "running" | "completed" | "failed" | "cancelled";
export type FixTaskOrchestrationState = "idle" | "applying" | "verifying" | "retry_scheduled" | "failed" | "cancelled" | "completed";
export type FixTaskLifecycle = { action: string; state: string; approvedAtUnixMs: number | null; completedAtUnixMs: number | null; error: string | null };

export interface StoredFixTask {
  taskId: string;
  projectPath: string;
  sessionId: string;
  findingId: string;
  status: FixTaskStatus;
  targetPaths: string[];
  gitBranch: string | null;
  baseCommit: string | null;
  errorMessage: string | null;
  createdAtUnixMs: number;
  updatedAtUnixMs: number;
  workspacePath: string | null;
  workspaceState: "not_created" | "ready" | "removed" | "error";
  workspaceError: string | null;
  patchPlan: { intent: string; files: string[] } | null;
  beforePaths: string[];
  afterPaths: string[];
  orchestration: { state: FixTaskOrchestrationState; retryCount: number; maxRetries: number; retryReason: string | null; failureLog: string | null; recovery: { state: string; beforeSha: string; lastGoodSha: string; finalSha: string | null; changedFiles: string[]; diffSummary: string; verification: StoredFixTask["verification"]; errorKind: string | null; error: string | null; updatedAtUnixMs: number } | null; remote: { action: "push" | "pull_request"; state: string; expectedBaseCommit: string; branch: string; title: string | null; body: string | null; approvedAtUnixMs: number | null; completedAtUnixMs: number | null; resultUrl: string | null; error: string | null } | null; lifecycle: FixTaskLifecycle | null };
  verification: { policy: FixVerificationPolicy; results: VerificationResult[] } | null;
  readiness: { state: "blocked" | "ready" | "approved"; findingId: string; changedFiles: string[]; verification: StoredFixTask["verification"]; baseCommit: string | null; approverAction: string | null; approvedAtUnixMs: number | null; evaluatedAtUnixMs: number; reason: string | null } | null;
}

export interface FixReadinessRequest {
  projectPath: string;
  taskId: string;
  expectedBaseCommit?: string | null;
  changedFiles?: string[];
  approve: boolean;
}

export async function evaluateFixCommitReadiness(request: FixReadinessRequest): Promise<StoredFixTask> {
  return invoke<StoredFixTask>("evaluate_fix_commit_readiness", { request });
}

export interface AiReviewResponse {
  session: StoredReviewSession;
  providerId: string;
  model: string;
  usage: ProviderUsage;
}

export type ReviewFindingSelection =
  | { file: { relativePath: string; range: SourceRange | null } }
  | { graphNode: { nodeId: string; label: string; range: SourceRange | null } };

export interface ValidateReviewFindingsRequest {
  projectPath: string;
  previewId: string;
  snapshotId: string;
  selection: ReviewFindingSelection;
  structuredOutput: unknown;
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
  python: DetectionResult;
  springBoot: DetectionResult;
  gradle: DetectionResult;
  node: DetectionResult;
  react: DetectionResult;
  express: DetectionResult;
  nestJs: DetectionResult;
  fastApi: DetectionResult;
  django: DetectionResult;
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

export async function previewReviewContext(
  request: PreviewReviewContextRequest,
): Promise<ReviewContextPreview> {
  try {
    return await invoke<ReviewContextPreview>("preview_review_context", { request });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function approveReviewTransfer(
  request: ApproveReviewTransferRequest,
): Promise<void> {
  try {
    await invoke<void>("approve_review_transfer", { request });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function runAiReview(request: RunAiReviewRequest): Promise<AiReviewResponse> {
  try {
    return await invoke<AiReviewResponse>("run_ai_review", { request });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function runLocalLlm(request: LocalLlmRequest): Promise<LocalLlmResponse> {
  try { return await invoke<LocalLlmResponse>("run_local_llm", { request }); }
  catch (error) { throw toAppCommandError(error); }
}

export async function validateReviewFindings(
  request: ValidateReviewFindingsRequest,
): Promise<ReviewFindingsValidationResult> {
  try {
    return await invoke<ReviewFindingsValidationResult>("validate_review_findings", { request });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function saveReviewSession(
  request: ValidateReviewFindingsRequest,
): Promise<StoredReviewSession> {
  try {
    return await invoke<StoredReviewSession>("save_review_session", {
      request,
    });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function listReviewSessions(
  projectPath: string,
): Promise<StoredReviewSessionSummary[]> {
  try {
    return await invoke<StoredReviewSessionSummary[]>("list_review_sessions", {
      request: { projectPath },
    });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function loadReviewSession(
  projectPath: string,
  sessionId: string,
): Promise<StoredReviewSession | null> {
  try {
    return await invoke<StoredReviewSession | null>("load_review_session", {
      request: { projectPath, sessionId },
    });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function setFindingDecision(
  projectPath: string,
  sessionId: string,
  findingId: string,
  status: FindingDecisionStatus,
): Promise<StoredFindingDecision> {
  try {
    return await invoke<StoredFindingDecision>("set_finding_decision", {
      request: { projectPath, sessionId, findingId, status },
    });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function createFixTask(projectPath: string, sessionId: string, findingId: string): Promise<StoredFixTask> {
  try {
    return await invoke<StoredFixTask>("create_fix_task", { request: { projectPath, sessionId, findingId } });
  } catch (error) { throw toAppCommandError(error); }
}

export async function listFixTasks(projectPath: string): Promise<StoredFixTask[]> {
  try {
    return await invoke<StoredFixTask[]>("list_fix_tasks", { request: { projectPath } });
  } catch (error) { throw toAppCommandError(error); }
}

export async function updateFixTask(
  projectPath: string,
  taskId: string,
  status: FixTaskStatus,
  errorMessage: string | null = null,
): Promise<StoredFixTask> {
  try {
    return await invoke<StoredFixTask>("update_fix_task", { request: { projectPath, taskId, status, errorMessage } });
  } catch (error) { throw toAppCommandError(error); }
}

export interface IntegrateFixTaskRequest {
  projectPath: string;
  taskId: string;
  commits: string[];
  expectedBaseCommit: string;
  approve: boolean;
}

export interface IntegrateFixTaskResponse {
  taskId: string;
  appliedCommits: string[];
  currentIndex: number;
  state: string;
}

export async function integrateFixTaskCommits(request: IntegrateFixTaskRequest): Promise<IntegrateFixTaskResponse> {
  return invoke<IntegrateFixTaskResponse>('integrate_fix_task_commits', { request });
}

export async function finalizeFixTask(request: { projectPath: string; taskId: string; expectedBaseCommit: string }): Promise<StoredFixTask> {
  return invoke<StoredFixTask>('finalize_fix_task', { request });
}

export async function recoverFixTask(request: { projectPath: string; taskId: string }): Promise<StoredFixTask> {
  return invoke<StoredFixTask>('recover_fix_task', { request });
}

export async function updateFixTaskOrchestration(
  projectPath: string,
  taskId: string,
  state: FixTaskOrchestrationState,
  retryCount: number,
  retryReason: string | null = null,
  failureLog: string | null = null,
): Promise<StoredFixTask> {
  try {
    return await invoke<StoredFixTask>("update_fix_task_orchestration", { request: { projectPath, taskId, state, retryCount, retryReason, failureLog } });
  } catch (error) { throw toAppCommandError(error); }
}

export async function manageFixTaskLifecycle(request: {
  projectPath: string;
  taskId: string;
  action: "archive" | "discard" | "recover";
  approve: boolean;
}): Promise<StoredFixTask> {
  return invoke<StoredFixTask>("manage_fix_task_lifecycle", { request });
}

export async function listOrphanFixWorktrees(projectPath: string): Promise<Array<{ path: string; taskId: string | null }>> {
  return invoke<Array<{ path: string; taskId: string | null }>>("list_orphan_fix_worktrees", { projectPath });
}

export interface RemoteApprovalRequest { projectPath: string; taskId: string; action: "push" | "pull_request"; expectedBaseCommit: string; branch: string; title?: string; body?: string; }
export interface RemoteExecuteRequest { projectPath: string; taskId: string; }
export async function approveFixTaskRemoteOperation(request: RemoteApprovalRequest): Promise<StoredFixTask> {
  try { return await invoke<StoredFixTask>("approve_fix_task_remote_operation", { request }); }
  catch (error) { throw toAppCommandError(error); }
}
export async function executeFixTaskRemoteOperation(request: RemoteExecuteRequest): Promise<StoredFixTask> {
  try { return await invoke<StoredFixTask>("execute_fix_task_remote_operation", { request }); }
  catch (error) { throw toAppCommandError(error); }
}

export async function prepareFixTaskWorkspace(projectPath: string, taskId: string): Promise<StoredFixTask> {
  try {
    return await invoke<StoredFixTask>("prepare_fix_task_workspace", { request: { projectPath, taskId } });
  } catch (error) { throw toAppCommandError(error); }
}

export async function removeFixTaskWorkspace(projectPath: string, taskId: string): Promise<StoredFixTask> {
  try {
    return await invoke<StoredFixTask>("remove_fix_task_workspace", { request: { projectPath, taskId } });
  } catch (error) { throw toAppCommandError(error); }
}

export async function createFixPatchPlan(projectPath: string, taskId: string, intent: string, files: string[]): Promise<StoredFixTask> {
  try { return await invoke<StoredFixTask>("create_fix_patch_plan", { request: { projectPath, taskId, intent, files } }); }
  catch (error) { throw toAppCommandError(error); }
}

export async function applyFixPatch(projectPath: string, taskId: string, patch: string): Promise<StoredFixTask> {
  try { return await invoke<StoredFixTask>("apply_fix_patch", { request: { projectPath, taskId, patch } }); }
  catch (error) { throw toAppCommandError(error); }
}

export interface GeneratedFixPatch {
  taskId: string;
  patch: string;
  providerId: string;
  model: string;
}

export async function generateFixPatch(projectPath: string, taskId: string, provider: CredentialProvider, model: string): Promise<GeneratedFixPatch> {
  try { return await invoke<GeneratedFixPatch>("generate_fix_patch", { request: { projectPath, taskId, provider, model } }); }
  catch (error) { throw toAppCommandError(error); }
}

export interface FixPatchDiffResponse {
  taskId: string;
  files: string[];
  diff: string;
}

export async function getFixPatchDiff(projectPath: string, taskId: string): Promise<FixPatchDiffResponse> {
  try { return await invoke<FixPatchDiffResponse>("get_fix_patch_diff", { request: { projectPath, taskId } }); }
  catch (error) { throw toAppCommandError(error); }
}

export async function listFixVerificationCandidates(projectPath: string, taskId: string): Promise<VerificationCandidates> {
  try { return await invoke<VerificationCandidates>("list_fix_verification_candidates", { request: { projectPath, taskId, approved: false } }); }
  catch (error) { throw toAppCommandError(error); }
}

export async function runFixVerification(projectPath: string, taskId: string, policy?: FixVerificationPolicy): Promise<StoredFixTask> {
  try { return await invoke<StoredFixTask>("run_fix_verification", { request: { projectPath, taskId, approved: true, policy } }); }
  catch (error) { throw toAppCommandError(error); }
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

export async function listProviderCredentials(): Promise<ProviderCredentialStatus[]> {
  try {
    return await invoke<ProviderCredentialStatus[]>("list_provider_credentials");
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function saveProviderCredential(
  provider: CredentialProvider,
  credential: string,
): Promise<void> {
  try {
    await invoke("save_provider_credential", { request: { provider, credential } });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function deleteProviderCredential(provider: CredentialProvider): Promise<void> {
  try {
    await invoke("delete_provider_credential", { request: { provider } });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function getGitHubRepository(projectPath: string): Promise<GitHubRepository> {
  try {
    return await invoke<GitHubRepository>("get_github_repository", { request: { projectPath } });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function getGitHubIssueOrPullRequest(
  projectPath: string,
  number: number,
): Promise<GitHubIssueOrPullRequest> {
  try {
    return await invoke<GitHubIssueOrPullRequest>("get_github_issue_or_pull_request", {
      request: { projectPath, number },
    });
  } catch (error) {
    throw toAppCommandError(error);
  }
}

export async function getOpenAiSubscriptionStatus(): Promise<OpenAiSubscriptionStatus> {
  try { return await invoke<OpenAiSubscriptionStatus>("get_openai_subscription_status"); }
  catch (error) { throw toAppCommandError(error); }
}

export async function signInOpenAiSubscription(): Promise<OpenAiSubscriptionStatus> {
  try { return await invoke<OpenAiSubscriptionStatus>("sign_in_openai_subscription"); }
  catch (error) { throw toAppCommandError(error); }
}

export async function cancelOpenAiSubscriptionSignIn(): Promise<void> {
  try { await invoke("cancel_openai_subscription_sign_in"); }
  catch (error) { throw toAppCommandError(error); }
}

export async function disconnectOpenAiSubscription(): Promise<void> {
  try { await invoke("disconnect_openai_subscription"); }
  catch (error) { throw toAppCommandError(error); }
}

export async function readProjectSource(request: { projectPath: string; relativePath: string }): Promise<string> {
  try { return await invoke<string>("read_project_source", { request }); }
  catch (error) { throw toAppCommandError(error); }
}

// ---- Pull request maintenance (AI conflict resolution / CI repair) ----
export interface PullRequestCheck { name: string; state: "success" | "failure" | "pending" | "skipped"; link: string | null; }
export interface PullRequestStatus {
  number: number; title: string; url: string; state: string;
  headRef: string; baseRef: string; headSha: string;
  mergeable: "MERGEABLE" | "CONFLICTING" | "UNKNOWN" | string;
  checks: PullRequestCheck[]; checksState: "success" | "failure" | "pending" | "none";
  workspaceReady: boolean;
}
export interface ConflictMergeResult { clean: boolean; conflictedFiles: string[]; }
export interface AiChangeResult { summary: string; files: string[]; diff: string; providerId: string; model: string; }
export interface CiFailureContext { runId: string; logExcerpt: string; candidateFiles: string[]; }
async function prCall<T>(command: string, request: Record<string, unknown>): Promise<T> {
  try { return await invoke<T>(command, { request }); }
  catch (error) { throw toAppCommandError(error); }
}
export const inspectPullRequest = (projectPath: string, number: number) => prCall<PullRequestStatus>("inspect_pull_request", { projectPath, number });
export const preparePullRequestWorkspace = (projectPath: string, number: number) => prCall<PullRequestStatus>("prepare_pull_request_workspace", { projectPath, number });
export const removePullRequestWorkspace = (projectPath: string, number: number) => prCall<void>("remove_pull_request_workspace", { projectPath, number });
export const startPullRequestMerge = (projectPath: string, number: number) => prCall<ConflictMergeResult>("start_pull_request_merge", { projectPath, number });
export const resolvePullRequestConflicts = (projectPath: string, number: number, provider: CredentialProvider, model: string) => prCall<AiChangeResult>("resolve_pull_request_conflicts", { projectPath, number, provider, model });
export const previewPullRequestCiFailure = (projectPath: string, number: number) => prCall<CiFailureContext>("preview_pull_request_ci_failure", { projectPath, number });
export const repairPullRequestCi = (projectPath: string, number: number, provider: CredentialProvider, model: string) => prCall<AiChangeResult>("repair_pull_request_ci", { projectPath, number, provider, model });
export const pushPullRequestUpdate = (projectPath: string, number: number, message: string, expectedHeadSha: string) => prCall<{ commitSha: string }>("push_pull_request_update", { projectPath, number, message, expectedHeadSha, approved: true });
export const mergePullRequest = (projectPath: string, number: number, expectedHeadSha: string, method: "merge" | "squash" | "rebase") => prCall<PullRequestStatus>("merge_pull_request", { projectPath, number, expectedHeadSha, method, approved: true });
