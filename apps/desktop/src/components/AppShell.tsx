import { createSignal } from "solid-js";
import type {
  DetectionResult,
  DetectionStatus,
  GitStatusResponse,
  GraphSnapshot,
  OpenProjectResponse,
  ScanEntry,
  ScanFileStatus,
  ScanProjectResponse,
  TaskProgressEvent,
} from "../lib/commands";
import FileTree from "./FileTree";
import GraphExplorer from "./GraphExplorer";
import AiReviewWorkspace from "./AiReviewWorkspace";

export type CoreStatus =
  | { kind: "loading" }
  | { kind: "ready"; message: string }
  | { kind: "error"; message: string }
  | { kind: "cancelled"; message: string };

export interface TaskUiState {
  event: TaskProgressEvent;
  cancellationRequested: boolean;
  cancellationError?: string;
}

interface AppShellProps {
  coreStatus: CoreStatus;
  coreStatusMessage: string;
  taskState: TaskUiState | null;
  project: OpenProjectResponse | null;
  scanResult: ScanProjectResponse | null;
  gitStatus: GitStatusResponse | null;
  gitStatusError: string | null;
  isGitStatusLoading: boolean;
  selectedEntry: ScanEntry | null;
  scanStatus: "idle" | "scanning" | "complete" | "cancelled" | "error";
  scanMessage: string | null;
  isScanning: boolean;
  isOpeningProject: boolean;
  projectError: string | null;
  onCancelTask: () => void;
  onOpenProject: () => void;
  onRescanProject: () => void;
  onRefreshGitStatus: () => void;
  onSelectEntry: (entry: ScanEntry) => void;
  graphSnapshot: GraphSnapshot | null;
  isGraphLoading: boolean;
  graphError: string | null;
  onAnalyzeGraph: () => void;
  onGraphViewSelected: () => void;
  onOpenGraphSource: (relativePath: string, line: number | null) => void;
  revealPath: string | null;
  graphSourceLocation: { relativePath: string; line: number | null } | null;
}

const navigationItems = [
  { label: "프로젝트 탐색", number: "01", active: true },
  { label: "코드 그래프", number: "02", active: true },
  { label: "API 목록", number: "03", phase: "준비 중" },
  { label: "AI 리뷰", number: "04", active: true },
  { label: "수정 및 검증", number: "05", phase: "준비 중" },
];

function getCoreStatusLabel(status: CoreStatus): string {
  if (status.kind === "loading") return "연결 중";
  if (status.kind === "ready") return "정상";
  if (status.kind === "cancelled") return "취소됨";
  return "오류";
}

function getTaskStatusLabel(task: TaskUiState): string {
  if (task.event.status === "cancelled") return "취소됨";
  if (task.event.status === "completed") return "완료";
  if (task.event.status === "failed") return "오류";
  if (task.cancellationRequested) return "취소 요청 중";
  return "진행 중";
}

function formatFileSize(bytes: number | null): string {
  if (bytes == null) return "정보 없음";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function formatModifiedDate(timestamp: number | null): string {
  if (timestamp == null) return "정보 없음";
  const date = new Date(timestamp);
  return Number.isNaN(date.getTime()) ? "정보 없음" : date.toLocaleString();
}

function getFileStatusLabel(status: ScanFileStatus | null): string {
  if (status === "eligible") return "분석 대상 가능";
  if (status === "unsupportedType") return "지원하지 않는 형식";
  if (status === "tooLarge") return "크기 제한 초과";
  if (status === "binary") return "바이너리 파일";
  return "해당 없음";
}

function getDetectionStatusLabel(status: DetectionStatus): string {
  if (status === "detected") return "감지됨";
  if (status === "uncertain") return "판단 유보";
  return "감지되지 않음";
}

function getGitStatusLabel(status: GitStatusResponse["state"]): string {
  if (status === "available") return "저장소 상태 확인됨";
  if (status === "gitUnavailable") return "Git을 사용할 수 없음";
  if (status === "notRepository") return "Git 저장소가 아님";
  return "Git 상태 조회 오류";
}

function AppShell(props: AppShellProps) {
  const [activeView, setActiveView] = createSignal<"project" | "graph" | "review">("project");
  const detectionRows = () => {
    const detection = props.scanResult?.projectDetection;
    if (!detection) return [];

    return [
      { label: "Kotlin", result: detection.kotlin },
      { label: "Java", result: detection.java },
      { label: "Spring Boot", result: detection.springBoot },
      { label: "Gradle", result: detection.gradle },
    ] satisfies Array<{ label: string; result: DetectionResult }>;
  };

  const taskProgress = () => {
    const task = props.taskState;
    if (!task || task.event.total == null) return null;

    const total = Math.max(1, task.event.total);
    return (
      <progress max={total} value={Math.min(task.event.completed, total)} />
    );
  };

  return (
    <div class="app-shell" classList={{ "graph-mode": activeView() !== "project" }}>
      <header class="topbar">
        <div class="topbar-brand">
          <div class="brand-mark" aria-hidden="true">V</div>
          <div>
            <strong>Visual Agentic Refactor</strong>
            <span>LOCAL CODE WORKSPACE</span>
          </div>
        </div>
        <div class="topbar-context">
          <span>작업 공간</span>
          <span class="breadcrumb-separator">/</span>
          <strong>{activeView() === "review" ? "AI 리뷰" : activeView() === "graph" ? "코드 그래프" : "프로젝트 탐색"}</strong>
        </div>
        <div class="local-badge"><span /> 로컬 작업 공간</div>
      </header>

      <aside class="sidebar" aria-label="주 탐색">
        <p class="section-label">WORKSPACE</p>
        <nav class="navigation-list">
          {navigationItems.map((item) => (
            <button
              class="navigation-item"
              classList={{ active: item.active && activeView() === "project" || item.label === "코드 그래프" && activeView() === "graph" || item.label === "AI 리뷰" && activeView() === "review" }}
              type="button"
              disabled={!item.active}
              aria-current={item.label === "코드 그래프" && activeView() === "graph" || item.label === "프로젝트 탐색" && activeView() === "project" || item.label === "AI 리뷰" && activeView() === "review" ? "page" : undefined}
              onClick={() => {
                if (item.label === "프로젝트 탐색") setActiveView("project");
                if (item.label === "코드 그래프") {
                  setActiveView("graph");
                  props.onGraphViewSelected();
                }
                if (item.label === "AI 리뷰") setActiveView("review");
              }}
              title={item.active ? undefined : "아직 제공되지 않는 화면입니다."}
            >
              <span class="navigation-number">{item.number}</span>
              <span class="navigation-label">{item.label}</span>
              {item.phase && <span class="navigation-phase">{item.phase}</span>}
            </button>
          ))}
        </nav>

        <div class="sidebar-note">
          <span class="sidebar-note-icon" aria-hidden="true">L</span>
          <div>
            <strong>로컬 우선</strong>
            <p>프로젝트 데이터는 이 기기에서 관리합니다.</p>
          </div>
        </div>
        <div class="sidebar-footer">LOCAL-FIRST · DESKTOP</div>
      </aside>

      <main class="content-area">
        {activeView() === "graph" ? (
          <GraphExplorer
            snapshot={props.graphSnapshot}
            loading={props.isGraphLoading}
            error={props.graphError}
            projectAvailable={props.project !== null}
            onAnalyze={props.onAnalyzeGraph}
            onOpenSource={(relativePath, line) => {
              setActiveView("project");
              props.onOpenGraphSource(relativePath, line);
            }}
          />
        ) : activeView() === "review" ? (
          <AiReviewWorkspace />
        ) : <>
        <div class="content-heading">
          <div>
            <p class="eyebrow">PROJECT WORKSPACE</p>
            <h1>프로젝트 탐색</h1>
            <p class="content-description">
              프로젝트 파일과 기술 스택, Git 상태를 한곳에서 살펴보세요.
            </p>
          </div>
          <button
            class="primary-button"
            type="button"
            disabled={props.isOpeningProject || props.isScanning}
            onClick={props.onOpenProject}
          >
            <span aria-hidden="true">＋</span>
            {props.isOpeningProject
              ? "폴더 선택 중..."
              : props.isScanning
                ? "스캔 중..."
                : "프로젝트 열기"}
          </button>
        </div>

        {props.projectError && (
          <p class="project-error" role="alert">
            {props.projectError}
          </p>
        )}

        {props.project ? (
          <section class="opened-project-card" aria-labelledby="opened-project-title">
            <span class="project-open-status">프로젝트 루트 확인됨</span>
            <h2 id="opened-project-title">{props.project.name}</h2>
            <p class="project-open-path" title={props.project.path}>
              {props.project.path}
            </p>
            <div class="project-scan-status" aria-live="polite">
              {props.scanStatus === "scanning" && (
                <p class="project-open-note">프로젝트 파일을 읽는 중입니다...</p>
              )}
              {props.scanStatus === "complete" && props.scanResult && (
                <p class="project-open-note">
                  {props.scanResult.summary.entries.toLocaleString()}개 항목을 찾았습니다.
                </p>
              )}
              {(props.scanStatus === "error" || props.scanStatus === "cancelled") && (
                <p class="inline-error" role="alert">
                  {props.scanMessage ??
                    (props.scanStatus === "cancelled" ? "스캔이 취소되었습니다." : "스캔에 실패했습니다.")}
                </p>
              )}
              {props.scanStatus !== "scanning" && (
                <button
                  class="secondary-button"
                  type="button"
                  disabled={props.isScanning}
                  onClick={props.onRescanProject}
                >
                  다시 스캔
                </button>
              )}
            </div>
            <section class="git-status-card" aria-labelledby="git-status-title" aria-live="polite">
              <div class="git-status-heading">
                <div>
                  <h3 id="git-status-title">Git 상태</h3>
                  {props.gitStatus && (
                    <span class={`git-status-label ${props.gitStatus.state}`}>
                      {getGitStatusLabel(props.gitStatus.state)}
                    </span>
                  )}
                </div>
                <button
                  class="secondary-button"
                  type="button"
                  disabled={props.isGitStatusLoading}
                  onClick={props.onRefreshGitStatus}
                >
                  {props.isGitStatusLoading ? "조회 중..." : "새로고침"}
                </button>
              </div>
              {props.isGitStatusLoading && !props.gitStatus && (
                <p class="git-status-message">저장소 상태를 확인하고 있습니다.</p>
              )}
              {props.gitStatusError && (
                <p class="inline-error" role="alert">{props.gitStatusError}</p>
              )}
              {props.gitStatus && props.gitStatus.state !== "available" && (
                <p class={`git-status-message ${props.gitStatus.state === "error" ? "error" : ""}`}>
                  {props.gitStatus.message}
                </p>
              )}
              {props.gitStatus?.state === "available" && (
                <>
                  <dl class="git-status-summary">
                    <div>
                      <dt>브랜치</dt>
                      <dd>{props.gitStatus.branch ?? (props.gitStatus.head ? "분리된 HEAD" : "첫 커밋 전")}</dd>
                    </div>
                    <div>
                      <dt>HEAD</dt>
                      <dd title={props.gitStatus.head ?? undefined}>{props.gitStatus.head ?? "커밋 없음"}</dd>
                    </div>
                    <div>
                      <dt>추적 파일</dt>
                      <dd>{props.gitStatus.trackedCount.toLocaleString()}</dd>
                    </div>
                    <div>
                      <dt>변경됨</dt>
                      <dd>{props.gitStatus.modifiedCount.toLocaleString()}</dd>
                    </div>
                    <div>
                      <dt>추적되지 않음</dt>
                      <dd>{props.gitStatus.untrackedCount.toLocaleString()}</dd>
                    </div>
                  </dl>
                  {props.gitStatus.changes.length > 0 ? (
                    <>
                      <ul class="git-change-list" aria-label="Git 변경 파일">
                        {props.gitStatus.changes.slice(0, 20).map((change) => (
                          <li>
                            <span class={`git-change-kind ${change.kind}`}>{change.statusCode}</span>
                            <code title={change.path}>{change.path}</code>
                          </li>
                        ))}
                      </ul>
                      {(props.gitStatus.omittedChangeCount > 0 || props.gitStatus.changes.length > 20) && (
                        <p class="git-change-overflow">
                          나머지 {(
                            props.gitStatus.omittedChangeCount +
                            Math.max(0, props.gitStatus.changes.length - 20)
                          ).toLocaleString()}개 변경 경로는 생략했습니다.
                        </p>
                      )}
                    </>
                  ) : (
                    <p class="git-status-message">변경된 파일이 없습니다.</p>
                  )}
                </>
              )}
            </section>
            {props.scanResult && (
              <section class="file-explorer" aria-labelledby="file-tree-title">
                <section class="project-detection" aria-labelledby="project-detection-title">
                  <h3 id="project-detection-title">기술 스택 감지</h3>
                  <ul class="project-detection-list">
                    {detectionRows().map(({ label, result }) => (
                      <li class="project-detection-item">
                        <div class="project-detection-heading">
                          <strong>{label}</strong>
                          <span class={`detection-status ${result.status}`}>
                            {getDetectionStatusLabel(result.status)}
                          </span>
                        </div>
                        <p>{result.message}</p>
                        {result.evidence.length > 0 && (
                          <ul class="detection-evidence-list">
                            {result.evidence.map((evidence) => (
                              <li>
                                <code title={evidence.relativePath}>{evidence.relativePath}</code>
                                <span>{evidence.description}</span>
                              </li>
                            ))}
                          </ul>
                        )}
                        {result.evidenceOmittedCount > 0 && (
                          <span class="detection-evidence-overflow">
                            추가 근거 {result.evidenceOmittedCount.toLocaleString()}개를 생략했습니다.
                          </span>
                        )}
                      </li>
                    ))}
                  </ul>
                </section>
                <div class="file-explorer-heading">
                  <h3 id="file-tree-title">파일 트리</h3>
                  <span>{props.scanResult.summary.entries.toLocaleString()}개 항목</span>
                </div>
                <p class="scan-summary-line">
                  파일 {props.scanResult.summary.files.toLocaleString()}개 · 디렉터리{" "}
                  {props.scanResult.summary.directories.toLocaleString()}개 · 분석 가능{" "}
                  {props.scanResult.summary.eligibleFiles.toLocaleString()}개 · 제외{" "}
                  {props.scanResult.summary.skippedFiles.toLocaleString()}개
                </p>
                {(props.scanResult.summary.depthLimitReached ||
                  props.scanResult.summary.entryLimitReached) && (
                  <div class="scan-limit-notice" role="status">
                    {props.scanResult.summary.depthLimitReached && (
                      <span>최대 탐색 깊이 {props.scanResult.summary.depthLimit}에 도달했습니다.</span>
                    )}
                    {props.scanResult.summary.entryLimitReached && (
                      <span>항목 수 제한에 도달해 나머지는 생략했습니다.</span>
                    )}
                  </div>
                )}
                {props.scanResult.entries.length > 0 ? (
                  <>
                    {props.graphSourceLocation && (
                      <p class="graph-source-selection" role="status">
                        그래프 근거 위치: {props.graphSourceLocation.relativePath}
                        {props.graphSourceLocation.line !== null && `:${props.graphSourceLocation.line}`}
                      </p>
                    )}
                    <FileTree
                      entries={props.scanResult.entries}
                      selectedPath={props.selectedEntry?.relativePath ?? null}
                      revealPath={props.revealPath}
                      onSelect={props.onSelectEntry}
                    />
                  </>
                ) : (
                  <div class="scan-empty-state" role="status">
                    <span aria-hidden="true">⌕</span>
                    <strong>표시할 파일이 없습니다</strong>
                    <p>
                      비어 있거나 기본 제외 규칙에 해당하는 항목만 있는 프로젝트일 수 있습니다.
                    </p>
                  </div>
                )}
                {props.scanResult.warnings.length > 0 && (
                  <details class="scan-warnings">
                    <summary>
                      경고 {props.scanResult.warnings.length.toLocaleString()}개
                    </summary>
                    <ul>
                      {props.scanResult.warnings.slice(0, 20).map((warning) => (
                        <li>
                          {warning.relativePath && (
                            <code title={warning.relativePath}>{warning.relativePath}</code>
                          )}
                          <span>{warning.message}</span>
                        </li>
                      ))}
                    </ul>
                    {props.scanResult.warnings.length > 20 && (
                      <p class="scan-warning-overflow">
                        나머지 {props.scanResult.warnings.length - 20}개 경고는 목록에서 생략했습니다.
                      </p>
                    )}
                  </details>
                )}
              </section>
            )}
          </section>
        ) : (
          <section class="empty-project-card" aria-labelledby="empty-project-title">
            <div class="empty-illustration" aria-hidden="true">
              <div class="illustration-toolbar"><i /><i /><i /></div>
              <div class="illustration-body">
                <div class="illustration-tree">
                  <span class="tree-line wide" />
                  <span class="tree-line medium" />
                  <span class="tree-line short" />
                  <span class="tree-line medium" />
                </div>
                <div class="illustration-content">
                  <span class="content-line wide" />
                  <span class="content-line medium" />
                  <span class="content-block" />
                </div>
              </div>
            </div>
            <div class="empty-project-copy">
              <span class="empty-kicker">시작하기</span>
              <h2 id="empty-project-title">아직 열린 프로젝트가 없습니다</h2>
              <p>
                프로젝트를 열면 파일 트리와 감지된 기술 정보가 이 공간에 표시됩니다.
              </p>
            </div>
          </section>
        )}

        <div class="content-footnote">
          <span class="footnote-dot" />
          파일 분석은 선택한 프로젝트 범위에서 로컬로 수행됩니다.
        </div>
        </>}
      </main>

      {activeView() === "project" && <aside class="details-panel" aria-label="상세 정보">
        <div class="details-heading">
          <div>
            <p class="eyebrow">INSPECTOR</p>
            <h2>상세 정보</h2>
          </div>
          <span class="details-menu-mark" aria-hidden="true">···</span>
        </div>

        <section class="detail-card" aria-labelledby="selected-entry-title">
          <div class="detail-card-heading">
            <h3 id="selected-entry-title">선택 항목</h3>
            <span class="muted-count">
              {props.selectedEntry
                ? props.selectedEntry.kind === "directory"
                  ? "디렉터리"
                  : "파일"
                : "—"}
            </span>
          </div>
          {props.selectedEntry ? (
            <dl class="entry-details">
              <div>
                <dt>이름</dt>
                <dd title={props.selectedEntry.name}>{props.selectedEntry.name}</dd>
              </div>
              <div>
                <dt>상대 경로</dt>
                <dd title={props.selectedEntry.relativePath}>
                  {props.selectedEntry.relativePath}
                </dd>
              </div>
              <div>
                <dt>종류</dt>
                <dd>{props.selectedEntry.kind === "directory" ? "디렉터리" : "파일"}</dd>
              </div>
              <div>
                <dt>크기</dt>
                <dd>{formatFileSize(props.selectedEntry.sizeBytes)}</dd>
              </div>
              <div>
                <dt>수정 시각</dt>
                <dd>{formatModifiedDate(props.selectedEntry.modifiedUnixMs)}</dd>
              </div>
              {props.selectedEntry.kind === "file" && (
                <>
                  <div>
                    <dt>확장자</dt>
                    <dd>{props.selectedEntry.extension ?? "없음"}</dd>
                  </div>
                  <div>
                    <dt>스캔 분류</dt>
                    <dd>{getFileStatusLabel(props.selectedEntry.fileStatus)}</dd>
                  </div>
                </>
              )}
            </dl>
          ) : (
            <div class="no-selection">
              <span class="selection-mark" aria-hidden="true">—</span>
              <p>트리에서 파일이나 디렉터리를 선택하세요.</p>
            </div>
          )}
        </section>

        <section class="detail-card">
          <div class="detail-card-heading">
            <h3>프로젝트</h3>
            <span class="muted-count">{props.project ? "열림" : "—"}</span>
          </div>
          {props.project ? (
            <div class="project-detail">
              <strong>{props.project.name}</strong>
              <p class="project-detail-path" title={props.project.path}>
                {props.project.path}
              </p>
            </div>
          ) : (
            <div class="no-selection">
              <span class="selection-mark" aria-hidden="true">—</span>
              <p>선택된 프로젝트가 없습니다.</p>
            </div>
          )}
        </section>

        <section class="detail-card core-card">
          <div class="detail-card-heading">
            <h3>데스크톱 코어</h3>
            <span class="status-pill" data-state={props.coreStatus.kind}>
              <i /> {getCoreStatusLabel(props.coreStatus)}
            </span>
          </div>
          <p class="core-message" aria-live={props.coreStatus.kind === "error" ? "assertive" : "polite"}>
            {props.coreStatusMessage}
          </p>
        </section>

        <section class="detail-card task-card" aria-label="비동기 작업 상태">
          <div class="detail-card-heading">
            <h3>현재 작업</h3>
            {props.taskState && (
              <span class="status-pill" data-state={props.taskState.event.status}>
                <i /> {getTaskStatusLabel(props.taskState)}
              </span>
            )}
          </div>
          {props.taskState ? (
            <>
              <p class="task-message">
                {props.taskState.event.message ?? "작업 진행 중"}
              </p>
              {taskProgress()}
              {props.taskState.cancellationError && (
                <p class="inline-error" role="alert">
                  {props.taskState.cancellationError}
                </p>
              )}
              {props.taskState.event.status === "running" &&
                !props.taskState.cancellationRequested && (
                  <button
                    class="cancel-button"
                    type="button"
                    onClick={props.onCancelTask}
                  >
                    작업 취소
                  </button>
                )}
            </>
          ) : (
            <p class="no-task-message">진행 중인 작업이 없습니다.</p>
          )}
        </section>

        <div class="details-footer">
          <span class="footer-shield" aria-hidden="true">✓</span>
          <span>파일 접근은 사용자가 선택한 범위로 제한됩니다.</span>
        </div>
      </aside>}
    </div>
  );
}

export default AppShell;
