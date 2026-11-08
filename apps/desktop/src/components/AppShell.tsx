import type {
  OpenProjectResponse,
  ScanProjectResponse,
  TaskProgressEvent,
} from "../lib/commands";

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
  scanStatus: "idle" | "scanning" | "complete" | "cancelled" | "error";
  scanMessage: string | null;
  isScanning: boolean;
  isOpeningProject: boolean;
  projectError: string | null;
  onCancelTask: () => void;
  onOpenProject: () => void;
  onRescanProject: () => void;
}

const navigationItems = [
  { label: "프로젝트 탐색", number: "01", active: true },
  { label: "코드 그래프", number: "02", phase: "준비 중" },
  { label: "API 목록", number: "03", phase: "준비 중" },
  { label: "AI 리뷰", number: "04", phase: "준비 중" },
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

function AppShell(props: AppShellProps) {
  const taskProgress = () => {
    const task = props.taskState;
    if (!task || task.event.total == null) return null;

    const total = Math.max(1, task.event.total);
    return (
      <progress max={total} value={Math.min(task.event.completed, total)} />
    );
  };

  return (
    <div class="app-shell">
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
          <strong>프로젝트 탐색</strong>
        </div>
        <div class="local-badge"><span /> 로컬 작업 공간</div>
      </header>

      <aside class="sidebar" aria-label="주 탐색">
        <p class="section-label">WORKSPACE</p>
        <nav class="navigation-list">
          {navigationItems.map((item) => (
            <button
              class="navigation-item"
              classList={{ active: item.active }}
              type="button"
              disabled={!item.active}
              aria-current={item.active ? "page" : undefined}
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
      </main>

      <aside class="details-panel" aria-label="상세 정보">
        <div class="details-heading">
          <div>
            <p class="eyebrow">INSPECTOR</p>
            <h2>상세 정보</h2>
          </div>
          <span class="details-menu-mark" aria-hidden="true">···</span>
        </div>

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
      </aside>
    </div>
  );
}

export default AppShell;
