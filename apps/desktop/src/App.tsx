import { createSignal, onCleanup, onMount } from "solid-js";
import "./App.css";
import AppShell, {
  type CoreStatus,
  type TaskUiState,
} from "./components/AppShell";
import {
  analyzeSpringProject,
  cancelTask,
  getGraphSnapshot,
  generateGraphDiagram,
  getGraphDiagram,
  listAiModels,
  getLastProject,
  getGitStatus,
  getAppInfo,
  openProject,
  scanProject,
  listenToTaskProgress,
  listGraphSnapshots,
  toAppCommandError,
} from "./lib/commands";
import { selectProjectFolder } from "./lib/projectPicker";
import type {
  GitStatusResponse,
  GraphSnapshot,
  OpenProjectResponse,
  ScanEntry,
  ScanProjectResponse,
} from "./lib/commands";

function App() {
  const [coreStatus, setCoreStatus] = createSignal<CoreStatus>({ kind: "loading" });
  const [taskState, setTaskState] = createSignal<TaskUiState | null>(null);
  const [project, setProject] = createSignal<OpenProjectResponse | null>(null);
  const [scanResult, setScanResult] = createSignal<ScanProjectResponse | null>(null);
  const [gitStatus, setGitStatus] = createSignal<GitStatusResponse | null>(null);
  const [gitStatusError, setGitStatusError] = createSignal<string | null>(null);
  const [isGitStatusLoading, setIsGitStatusLoading] = createSignal(false);
  const [selectedEntry, setSelectedEntry] = createSignal<ScanEntry | null>(null);
  const [graphSnapshot, setGraphSnapshot] = createSignal<GraphSnapshot | null>(null);
  const [isGraphLoading, setIsGraphLoading] = createSignal(false);
  const [graphError, setGraphError] = createSignal<string | null>(null);
  const [revealPath, setRevealPath] = createSignal<string | null>(null);
  const [graphSourceLocation, setGraphSourceLocation] = createSignal<{ relativePath: string; line: number | null } | null>(null);
  const [scanStatus, setScanStatus] = createSignal<"idle" | "scanning" | "complete" | "cancelled" | "error">("idle");
  const [scanMessage, setScanMessage] = createSignal<string | null>(null);
  const [isScanning, setIsScanning] = createSignal(false);
  const [isOpeningProject, setIsOpeningProject] = createSignal(false);
  const [projectError, setProjectError] = createSignal<string | null>(null);
  let gitStatusRequestId = 0;
  let graphRequestId = 0;

  onMount(() => {
    void getAppInfo()
      .then((appInfo) => {
        setCoreStatus({
          kind: "ready",
          message: `${appInfo.appName} ${appInfo.version}`,
        });
      })
      .catch((error: unknown) => {
        const commandError = toAppCommandError(error);
        setCoreStatus({
          kind: commandError.code === "cancelled" ? "cancelled" : "error",
          message: commandError.message,
        });
      });

    let unlisten: (() => void) | undefined;
    let disposed = false;
    void listenToTaskProgress((event) => {
      setTaskState((previous) => ({
        event,
        cancellationRequested:
          event.status === "running" &&
          previous?.event.taskId === event.taskId &&
          previous.cancellationRequested,
      }));
    })
      .then((stopListening) => {
        if (disposed) stopListening();
        else unlisten = stopListening;
      })
      .catch((error: unknown) => {
        const commandError = toAppCommandError(error);
        setCoreStatus({ kind: "error", message: commandError.message });
      });

    void restoreLastProject();

    onCleanup(() => {
      disposed = true;
      unlisten?.();
    });
  });

  async function restoreLastProject() {
    setIsOpeningProject(true);
    try {
      const savedProject = await getLastProject();
      if (!savedProject) return;

      const openedProject = await openProject(savedProject.path);
      graphRequestId += 1;
      setGraphSnapshot(null);
      setGraphSourceLocation(null);
      setIsGraphLoading(false);
      setGraphError(null);
      setProject(openedProject);
      setProjectError(openedProject.persistenceError);
      await scanOpenedProject(openedProject.path);
    } catch (error: unknown) {
      setProjectError(toAppCommandError(error).message);
    } finally {
      setIsOpeningProject(false);
    }
  }

  async function requestCancellation() {
    const activeTask = taskState();
    if (!activeTask || activeTask.event.status !== "running") return;

    try {
      const response = await cancelTask(activeTask.event.taskId);
      setTaskState((current) =>
        current?.event.taskId === response.taskId
          ? { ...current, cancellationRequested: response.cancellationRequested }
          : current,
      );
    } catch (error) {
      const commandError = toAppCommandError(error);
      setTaskState((current) =>
        current?.event.taskId === activeTask.event.taskId
          ? { ...current, cancellationError: commandError.message }
          : current,
      );
    }
  }

  async function handleOpenProject() {
    if (isOpeningProject() || isScanning()) return;

    setIsOpeningProject(true);
    try {
      const selectedPath = await selectProjectFolder();
      if (selectedPath === null) return;

      const openedProject = await openProject(selectedPath);
      graphRequestId += 1;
      setGraphSnapshot(null);
      setGraphSourceLocation(null);
      setIsGraphLoading(false);
      setGraphError(null);
      setProject(openedProject);
      setProjectError(openedProject.persistenceError);
      setScanResult(null);
      setGitStatus(null);
      setGitStatusError(null);
      setSelectedEntry(null);
      setScanMessage(null);
      setIsOpeningProject(false);
      await scanOpenedProject(openedProject.path);
    } catch (error: unknown) {
      setProjectError(toAppCommandError(error).message);
    } finally {
      setIsOpeningProject(false);
    }
  }

  async function scanOpenedProject(path: string) {
    if (isScanning()) return;

    void refreshGitStatus(path);
    setIsScanning(true);
    setScanStatus("scanning");
    setScanMessage(null);
    try {
      const result = await scanProject({ path });
      setScanResult(result);
      setProjectError(result.persistenceError);
      setSelectedEntry(null);
      setScanStatus("complete");
    } catch (error: unknown) {
      const commandError = toAppCommandError(error);
      setScanStatus(commandError.code === "cancelled" ? "cancelled" : "error");
      setScanMessage(commandError.message);
    } finally {
      setIsScanning(false);
    }
  }

  async function refreshGitStatus(path: string) {
    const requestId = ++gitStatusRequestId;
    setIsGitStatusLoading(true);
    setGitStatusError(null);

    try {
      const result = await getGitStatus(path);
      if (requestId === gitStatusRequestId) setGitStatus(result);
    } catch (error: unknown) {
      if (requestId === gitStatusRequestId) {
        setGitStatusError(toAppCommandError(error).message);
      }
    } finally {
      if (requestId === gitStatusRequestId) setIsGitStatusLoading(false);
    }
  }

  function handleRescanProject() {
    const currentProject = project();
    if (currentProject) void scanOpenedProject(currentProject.path);
  }

  async function loadLatestGraph(): Promise<GraphSnapshot | null> {
    const currentProject = project();
    const requestId = ++graphRequestId;
    if (!currentProject) {
      setGraphSnapshot(null);
      setGraphError(null);
      setIsGraphLoading(false);
      return null;
    }

    setIsGraphLoading(true);
    setGraphError(null);
    try {
      const snapshots = await listGraphSnapshots(currentProject.path);
      const latest = snapshots[0];
      const snapshot = latest
        ? await getGraphSnapshot(currentProject.path, latest.snapshotId)
        : null;
      if (requestId === graphRequestId) setGraphSnapshot(snapshot);
      return snapshot;
    } catch (error: unknown) {
      if (requestId === graphRequestId) setGraphError(toAppCommandError(error).message);
      return null;
    } finally {
      if (requestId === graphRequestId) setIsGraphLoading(false);
    }
  }

  async function handleAnalyzeGraph() {
    const currentProject = project();
    if (!currentProject || isGraphLoading()) return;

    const requestId = ++graphRequestId;
    setIsGraphLoading(true);
    setGraphError(null);
    try {
      const result = await analyzeSpringProject({ path: currentProject.path });
      const snapshot = await getGraphSnapshot(currentProject.path, result.snapshotId);
      if (!snapshot) throw new Error("분석 결과 그래프를 불러오지 못했습니다.");
      if (requestId === graphRequestId) setGraphSnapshot(snapshot);
    } catch (error: unknown) {
      if (requestId === graphRequestId) setGraphError(toAppCommandError(error).message);
    } finally {
      if (requestId === graphRequestId) setIsGraphLoading(false);
    }
  }

  /** Must match `ANALYZER_VERSION` in src-tauri/src/spring.rs. */
  const CURRENT_ANALYZER_VERSION = "spring-static-v2";

  async function handleApiViewSelected() {
    const snapshot = await loadLatestGraph();
    // Re-analyze when no endpoints exist or the snapshot came from an older analyzer.
    if (!snapshot || snapshot.analyzerVersion !== CURRENT_ANALYZER_VERSION || !snapshot.nodes.some((node) => node.nodeType === "apiEndpoint")) {
      await handleAnalyzeGraph();
    }
  }

  async function handleGenerateAiDiagram(provider: "openai" | "anthropic" | "claudeCode" | "codex", model: string) {
    const currentProject = project();
    const snapshot = graphSnapshot();
    if (!currentProject || !snapshot) throw new Error("먼저 graph를 분석해 주세요.");
    return generateGraphDiagram({ projectPath: currentProject.path, snapshot, provider, model });
  }

  async function handleLoadAiDiagram(provider: "openai" | "anthropic" | "claudeCode" | "codex", model: string) {
    const currentProject = project();
    const snapshot = graphSnapshot();
    if (!currentProject || !snapshot) return null;
    return getGraphDiagram({ projectPath: currentProject.path, snapshotId: snapshot.snapshotId, provider, model });
  }

  function handleOpenGraphSource(relativePath: string, line: number | null) {
    const entry = scanResult()?.entries.find((item) => item.relativePath === relativePath) ?? null;
    setSelectedEntry(entry);
    setGraphSourceLocation({ relativePath, line });
    setRevealPath(null);
    queueMicrotask(() => setRevealPath(relativePath));
  }

  const getCoreStatusMessage = () => {
    const status = coreStatus();
    return status.kind === "loading" ? "데스크톱 코어 연결 중…" : status.message;
  };

  return (
    <AppShell
      coreStatus={coreStatus()}
      coreStatusMessage={getCoreStatusMessage()}
      taskState={taskState()}
      project={project()}
      scanResult={scanResult()}
      gitStatus={gitStatus()}
      gitStatusError={gitStatusError()}
      isGitStatusLoading={isGitStatusLoading()}
      selectedEntry={selectedEntry()}
      onSelectEntry={setSelectedEntry}
      scanStatus={scanStatus()}
      scanMessage={scanMessage()}
      isScanning={isScanning()}
      isOpeningProject={isOpeningProject()}
      projectError={projectError()}
      onCancelTask={() => void requestCancellation()}
      onOpenProject={() => void handleOpenProject()}
      onRescanProject={handleRescanProject}
      onRefreshGitStatus={() => {
        const currentProject = project();
        if (currentProject) void refreshGitStatus(currentProject.path);
      }}
      graphSnapshot={graphSnapshot()}
      isGraphLoading={isGraphLoading()}
      graphError={graphError()}
      onAnalyzeGraph={() => void handleAnalyzeGraph()}
      onGraphViewSelected={() => void loadLatestGraph()}
      onApiViewSelected={() => void handleApiViewSelected()}
      onGenerateAiDiagram={handleGenerateAiDiagram}
      onLoadAiDiagram={handleLoadAiDiagram}
      onListAiModels={listAiModels}
      onOpenGraphSource={handleOpenGraphSource}
      revealPath={revealPath()}
      graphSourceLocation={graphSourceLocation()}
    />
  );
}

export default App;
