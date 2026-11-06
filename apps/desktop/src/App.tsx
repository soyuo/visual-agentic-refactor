import { createSignal, onCleanup, onMount } from "solid-js";
import "./App.css";
import AppShell, {
  type CoreStatus,
  type TaskUiState,
} from "./components/AppShell";
import {
  cancelTask,
  getAppInfo,
  openProject,
  listenToTaskProgress,
  toAppCommandError,
} from "./lib/commands";
import { selectProjectFolder } from "./lib/projectPicker";
import type { OpenProjectResponse } from "./lib/commands";

function App() {
  const [coreStatus, setCoreStatus] = createSignal<CoreStatus>({ kind: "loading" });
  const [taskState, setTaskState] = createSignal<TaskUiState | null>(null);
  const [project, setProject] = createSignal<OpenProjectResponse | null>(null);
  const [isOpeningProject, setIsOpeningProject] = createSignal(false);
  const [projectError, setProjectError] = createSignal<string | null>(null);

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

    onCleanup(() => {
      disposed = true;
      unlisten?.();
    });
  });

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
    if (isOpeningProject()) return;

    setIsOpeningProject(true);
    try {
      const selectedPath = await selectProjectFolder();
      if (selectedPath === null) return;

      setProjectError(null);
      const openedProject = await openProject(selectedPath);
      setProject(openedProject);
    } catch (error: unknown) {
      setProjectError(toAppCommandError(error).message);
    } finally {
      setIsOpeningProject(false);
    }
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
      isOpeningProject={isOpeningProject()}
      projectError={projectError()}
      onCancelTask={() => void requestCancellation()}
      onOpenProject={() => void handleOpenProject()}
    />
  );
}

export default App;
