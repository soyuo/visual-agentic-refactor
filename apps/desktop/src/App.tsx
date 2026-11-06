import { onCleanup, onMount, createSignal } from "solid-js";
import {
  cancelTask,
  getAppInfo,
  listenToTaskProgress,
  toAppCommandError,
  type TaskProgressEvent,
} from "./lib/commands";
import "./App.css";

interface TaskUiState {
  event: TaskProgressEvent;
  cancellationRequested: boolean;
  cancellationError?: string;
}

function App() {
  const [coreStatus, setCoreStatus] = createSignal<
    | { kind: "loading" }
    | { kind: "ready"; message: string }
    | { kind: "error"; message: string }
    | { kind: "cancelled"; message: string }
  >({ kind: "loading" });
  const [taskState, setTaskState] = createSignal<TaskUiState | null>(null);

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

  const coreStatusMessage = () => {
    const status = coreStatus();
    return status.kind === "loading" ? "데스크톱 코어 연결 중…" : status.message;
  };

  const renderTaskProgress = () => {
    const currentTask = taskState();
    if (!currentTask || currentTask.event.total == null) return null;

    return (
      <progress
        max={currentTask.event.total}
        value={Math.min(currentTask.event.completed, currentTask.event.total)}
      />
    );
  };

  return (
    <main class="container">
      <h1>Visual Agentic Refactor</h1>
      <p
        class="core-status"
        data-state={coreStatus().kind}
        aria-live={
          coreStatus().kind === "error" || coreStatus().kind === "cancelled"
            ? "assertive"
            : "polite"
        }
      >
        {coreStatusMessage()}
      </p>
      {taskState() && (
        <section class="task-status" aria-label="비동기 작업 상태">
          <p>{taskState()?.event.message ?? "작업 진행 중"}</p>
          {renderTaskProgress()}
          <p
            class="task-state"
            data-state={taskState()?.event.status}
            role={taskState()?.event.status === "failed" ? "alert" : "status"}
          >
            {taskState()?.event.status === "cancelled"
              ? "취소됨"
              : taskState()?.event.status === "completed"
                ? "완료"
                : taskState()?.event.status === "failed"
                  ? "오류"
                  : taskState()?.cancellationRequested
                    ? "취소 요청 중"
                    : "진행 중"}
          </p>
          {taskState()?.cancellationError && (
            <p role="alert">{taskState()?.cancellationError}</p>
          )}
          {taskState()?.event.status === "running" &&
            !taskState()?.cancellationRequested && (
              <button type="button" onClick={() => void requestCancellation()}>
                취소
              </button>
            )}
        </section>
      )}
    </main>
  );
}

export default App;
