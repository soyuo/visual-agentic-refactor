import { onMount, createSignal } from "solid-js";
import { getAppInfo, toAppCommandError } from "./lib/commands";
import "./App.css";

function App() {
  const [coreStatus, setCoreStatus] = createSignal<
    | { kind: "loading" }
    | { kind: "ready"; message: string }
    | { kind: "error"; message: string }
  >({ kind: "loading" });

  onMount(() => {
    void getAppInfo()
      .then((appInfo) => {
        setCoreStatus({
          kind: "ready",
          message: `${appInfo.appName} ${appInfo.version}`,
        });
      })
      .catch((error: unknown) => {
        setCoreStatus({ kind: "error", message: toAppCommandError(error).message });
      });
  });

  const coreStatusMessage = () => {
    const status = coreStatus();
    return status.kind === "loading" ? "데스크톱 코어 연결 중…" : status.message;
  };

  return (
    <main class="container">
      <h1>Visual Agentic Refactor</h1>
      <p
        class="core-status"
        data-state={coreStatus().kind}
        aria-live={coreStatus().kind === "error" ? "assertive" : "polite"}
      >
        {coreStatusMessage()}
      </p>
    </main>
  );
}

export default App;
