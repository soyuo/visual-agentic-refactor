import { createEffect, createSignal, onMount, Show } from "solid-js";
import { listen } from "@tauri-apps/api/event";
import {
  cancelTask,
  createFixTask,
  disconnectOpenAiSubscription,
  deleteProviderCredential,
  listProviderCredentials,
  getOpenAiSubscriptionStatus,
  listReviewSessions,
  listFixTasks,
  prepareFixTaskWorkspace,
  removeFixTaskWorkspace,
  loadReviewSession,
  approveReviewTransfer,
  runAiReview,
  setFindingDecision as persistFindingDecision,
  saveProviderCredential,
  signInOpenAiSubscription,
  previewReviewContext,
  toAppCommandError,
  type CredentialProvider,
  type AiReviewTaskStartedEvent,
  type GraphSnapshot,
  type ProviderCredentialStatus,
  type OpenProjectResponse,
  type OpenAiSubscriptionStatus,
  type ReviewContextPreview,
  type ReviewContextScope,
  type ReviewFindingSelection,
  type ScanEntry,
  type StoredReviewSession,
  type StoredReviewSessionSummary,
  type StoredFixTask,
} from "../lib/commands";
import { isSensitiveTransferPath } from "../lib/externalTransferSafety";
import { findingDecisionLabel, reviewOutcomeLabel, reviewOutcomeState } from "../lib/aiReviewViewModel";
import "./AiReviewWorkspace.css";

const providers: Array<{ id: CredentialProvider; label: string }> = [
  { id: "openai", label: "OpenAI" },
  { id: "anthropic", label: "Anthropic" },
];

interface AiReviewWorkspaceProps {
  project: OpenProjectResponse | null;
  selectedEntry: ScanEntry | null;
  graphSnapshot: GraphSnapshot | null;
  selectedGraphNodeId: string | null;
}

function AiReviewWorkspace(props: AiReviewWorkspaceProps) {
  const [statuses, setStatuses] = createSignal<ProviderCredentialStatus[]>([]);
  const [selectedProvider, setSelectedProvider] = createSignal<CredentialProvider>("openai");
  const [credential, setCredential] = createSignal("");
  const [loading, setLoading] = createSignal(true);
  const [busyProvider, setBusyProvider] = createSignal<CredentialProvider | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [notice, setNotice] = createSignal<string | null>(null);
  const [subscriptionStatus, setSubscriptionStatus] = createSignal<OpenAiSubscriptionStatus | null>(null);
  const [subscriptionBusy, setSubscriptionBusy] = createSignal(false);
  const [consentDialogOpen, setConsentDialogOpen] = createSignal(false);
  const [consentDecision, setConsentDecision] = createSignal<"approved" | "rejected" | "cancelled" | null>(null);
  const [consentScopeKey, setConsentScopeKey] = createSignal("");
  const [scopeKind, setScopeKind] = createSignal<"file" | "graphNode">("file");
  const [contextPreview, setContextPreview] = createSignal<ReviewContextPreview | null>(null);
  const [previewLoading, setPreviewLoading] = createSignal(false);
  const [previewError, setPreviewError] = createSignal<string | null>(null);
  const [model, setModel] = createSignal("");
  const [reviewRunning, setReviewRunning] = createSignal(false);
  const [reviewTaskId, setReviewTaskId] = createSignal<number | null>(null);
  const [reviewError, setReviewError] = createSignal<string | null>(null);
  const [reviewNotice, setReviewNotice] = createSignal<string | null>(null);
  const [reviewMetadata, setReviewMetadata] = createSignal<string | null>(null);
  const [sessions, setSessions] = createSignal<StoredReviewSessionSummary[]>([]);
  const [activeSession, setActiveSession] = createSignal<StoredReviewSession | null>(null);
  const [selectedSessionId, setSelectedSessionId] = createSignal("");
  const [sessionLoading, setSessionLoading] = createSignal(false);
  const [sessionError, setSessionError] = createSignal<string | null>(null);
  const [savingDecisionKeys, setSavingDecisionKeys] = createSignal<string[]>([]);
  const [fixTasks, setFixTasks] = createSignal<StoredFixTask[]>([]);
  const [workspaceTaskId, setWorkspaceTaskId] = createSignal<string | null>(null);
  const [creatingFixTaskId, setCreatingFixTaskId] = createSignal<string | null>(null);
  let previewRequestVersion = 0;
  let sessionRequestVersion = 0;
  let consentDialog: HTMLElement | undefined;
  let returnFocus: HTMLElement | null = null;

  const selectedFile = () => {
    const entry = props.selectedEntry;
    return entry?.kind === "file" && entry.fileStatus === "eligible" && !isSensitiveTransferPath(entry.relativePath)
      ? entry
      : null;
  };
  const activeCredential = () => statuses().find((status) => status.provider === selectedProvider());
  const selectedGraphNode = () => props.graphSnapshot?.nodes.find((node) => node.nodeId === props.selectedGraphNodeId) ?? null;
  const graphNodePath = () => {
    const node = selectedGraphNode();
    const path = node?.filePath ?? node?.sourceEvidence?.relativePath ?? null;
    return path && /\.(java|kt|kts)$/i.test(path) && !isSensitiveTransferPath(path) ? path : null;
  };
  const currentScope = (): ReviewContextScope | null => {
    if (scopeKind() === "graphNode") {
      const node = selectedGraphNode();
      return node && graphNodePath() ? { graphNode: { nodeId: node.nodeId } } : null;
    }
    const file = selectedFile();
    return file ? { file: { relativePath: file.relativePath, range: null } } : null;
  };
  const scopeLabel = () => scopeKind() === "graphNode"
    ? selectedGraphNode()?.qualifiedName ?? selectedGraphNode()?.name ?? graphNodePath()
    : selectedFile()?.relativePath;
  const reviewSelection = (): ReviewFindingSelection | null => {
    const scope = currentScope();
    if (!scope) return null;
    if ("file" in scope) return { file: scope.file };
    const node = selectedGraphNode();
    if (!node) return null;
    return {
      graphNode: {
        nodeId: node.nodeId,
        label: node.qualifiedName ?? node.name,
        range: node.sourceEvidence,
      },
    };
  };
  const scopeKey = () => JSON.stringify([
    props.project?.path ?? "",
    selectedProvider(),
    model().trim(),
    scopeKind(),
    selectedFile()?.relativePath ?? "",
    props.selectedGraphNodeId ?? "",
    props.graphSnapshot?.snapshotId ?? "",
  ]);

  createEffect(() => {
    const currentScopeKey = scopeKey();
    if (currentScopeKey !== consentScopeKey()) {
      previewRequestVersion += 1;
      setPreviewLoading(false);
      setConsentScopeKey(currentScopeKey);
      setConsentDecision(null);
      setConsentDialogOpen(false);
      setContextPreview(null);
      setPreviewError(null);
    }
  });

  createEffect(() => {
    const path = props.project?.path ?? "";
    const session = activeSession();
    if (!path || !session) {
      setFixTasks([]);
      return;
    }
    void listFixTasks(path).then((tasks) => {
      if (props.project?.path === path && activeSession()?.sessionId === session.sessionId) setFixTasks(tasks);
    }).catch(() => setFixTasks([]));
  });

  onMount(() => {
    void refreshStatuses();
    void refreshSubscriptionStatus();
  });

  createEffect(() => {
    const path = props.project?.path ?? "";
    const requestVersion = ++sessionRequestVersion;
    setSessions([]);
    setActiveSession(null);
    setSelectedSessionId("");
    setSessionError(null);
    if (!path) {
      setSessionLoading(false);
      return;
    }
    setSessionLoading(true);
    void listReviewSessions(path).then((items) => {
      if (requestVersion === sessionRequestVersion && props.project?.path === path) setSessions(items);
    }).catch((cause: unknown) => {
      if (requestVersion === sessionRequestVersion && props.project?.path === path) {
        setSessionError(toAppCommandError(cause).message);
      }
    }).finally(() => {
      if (requestVersion === sessionRequestVersion) setSessionLoading(false);
    });
  });

  createEffect(() => {
    if (consentDialogOpen()) queueMicrotask(() => consentDialog?.querySelector<HTMLElement>('button:not([disabled])')?.focus());
  });

  async function refreshStatuses() {
    setLoading(true);
    setError(null);
    try {
      setStatuses(await listProviderCredentials());
    } catch (cause: unknown) {
      setError(toAppCommandError(cause).message);
    } finally {
      setLoading(false);
    }
  }

  async function refreshSubscriptionStatus() {
    try {
      setSubscriptionStatus(await getOpenAiSubscriptionStatus());
    } catch (error) {
      setError(toAppCommandError(error).message);
    }
  }

  async function connectOpenAiSubscription() {
    setSubscriptionBusy(true);
    setError(null);
    try {
      setSubscriptionStatus(await signInOpenAiSubscription());
      setNotice("OpenAI ChatGPT 구독 로그인이 완료되었습니다.");
    } catch (error) {
      setError(toAppCommandError(error).message);
    } finally {
      setSubscriptionBusy(false);
    }
  }

  async function disconnectOpenAiSubscriptionAccount() {
    setSubscriptionBusy(true);
    setError(null);
    try {
      await disconnectOpenAiSubscription();
      await refreshSubscriptionStatus();
      setNotice("OpenAI ChatGPT 구독 연결을 해제했습니다.");
    } catch (error) {
      setError(toAppCommandError(error).message);
    } finally {
      setSubscriptionBusy(false);
    }
  }

  async function saveSelectedCredential() {
    const value = credential();
    if (!value.trim() || busyProvider()) return;

    const provider = selectedProvider();
    setBusyProvider(provider);
    setError(null);
    setNotice(null);
    try {
      await saveProviderCredential(provider, value);
      setCredential("");
      setNotice(`${providers.find((item) => item.id === provider)?.label} 자격 증명을 OS 저장소에 저장했습니다.`);
      await refreshStatuses();
    } catch (cause: unknown) {
      setError(toAppCommandError(cause).message);
    } finally {
      setBusyProvider(null);
    }
  }

  async function removeCredential(provider: CredentialProvider) {
    if (busyProvider()) return;

    setBusyProvider(provider);
    setError(null);
    setNotice(null);
    try {
      await deleteProviderCredential(provider);
      setNotice(`${providers.find((item) => item.id === provider)?.label} 자격 증명을 삭제했습니다.`);
      await refreshStatuses();
    } catch (cause: unknown) {
      setError(toAppCommandError(cause).message);
    } finally {
      setBusyProvider(null);
    }
  }

  async function loadContextPreview() {
    const scope = currentScope();
    if (!props.project || !scope || previewLoading()) return;
    const requestVersion = ++previewRequestVersion;
    const requestScopeKey = scopeKey();
    setPreviewLoading(true);
    setPreviewError(null);
    setContextPreview(null);
    setConsentDecision(null);
    try {
      const preview = await previewReviewContext({
        projectPath: props.project.path,
        scope,
        maxContextTokens: 32_000,
      });
      if (requestVersion === previewRequestVersion && requestScopeKey === scopeKey()) {
        setContextPreview(preview);
      }
    } catch (cause: unknown) {
      if (requestVersion === previewRequestVersion && requestScopeKey === scopeKey()) {
        setPreviewError(toAppCommandError(cause).message);
      }
    } finally {
      if (requestVersion === previewRequestVersion) setPreviewLoading(false);
    }
  }

  async function loadSession(sessionId: string) {
    const projectPath = props.project?.path;
    const requestVersion = ++sessionRequestVersion;
    setSelectedSessionId(sessionId);
    setSessionError(null);
    setReviewMetadata(null);
    setActiveSession(null);
    if (!projectPath || !sessionId) {
      setSessionLoading(false);
      return;
    }

    setSessionLoading(true);
    try {
      const session = await loadReviewSession(projectPath, sessionId);
      if (requestVersion === sessionRequestVersion && props.project?.path === projectPath) {
        setActiveSession(session);
      }
    } catch (cause: unknown) {
      if (requestVersion === sessionRequestVersion && props.project?.path === projectPath) {
        setSessionError(toAppCommandError(cause).message);
      }
    } finally {
      if (requestVersion === sessionRequestVersion) setSessionLoading(false);
    }
  }

  function openConsentDialog() {
    returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setConsentDecision(null);
    setReviewError(null);
    setConsentDialogOpen(true);
  }

  function closeConsentDialog(decision: "rejected" | "cancelled") {
    setConsentDecision(decision);
    setConsentDialogOpen(false);
    queueMicrotask(() => returnFocus?.focus());
  }

  function handleConsentKeyDown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      closeConsentDialog("cancelled");
      return;
    }
    if (event.key !== "Tab" || !consentDialog) return;
    const focusable = [...consentDialog.querySelectorAll<HTMLElement>(
      'button:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])',
    )];
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }

  async function startReview() {
    const project = props.project;
    const preview = contextPreview();
    const selection = reviewSelection();
    const provider = selectedProvider();
    const modelId = model().trim();
    const approvedScopeKey = scopeKey();
    if (!project || !preview?.graphSnapshotId || !selection || !modelId || reviewRunning()) return;

    const request = {
      projectPath: project.path,
      previewId: preview.previewId,
      snapshotId: preview.graphSnapshotId,
      selection,
      provider,
      model: modelId,
    };
    setConsentDialogOpen(false);
    setConsentDecision("approved");
    setReviewRunning(true);
    setReviewTaskId(null);
    setReviewError(null);
    setReviewNotice(null);
    setReviewMetadata(null);
    const requestId = crypto.randomUUID();
    let unlistenTaskStarted: (() => void) | undefined;
    try {
      await approveReviewTransfer(request);
      if (approvedScopeKey !== scopeKey()) {
        setReviewNotice("선택 범위가 바뀌어 외부 요청을 시작하지 않았습니다. 새 문맥을 미리보기 하세요.");
        return;
      }
      unlistenTaskStarted = await listen<AiReviewTaskStartedEvent>("ai-review-task-started", (event) => {
        if (event.payload.requestId === requestId) setReviewTaskId(event.payload.taskId);
      });
      const response = await runAiReview({ ...request, requestId });
      if (approvedScopeKey !== scopeKey() || props.project?.path !== project.path) return;
      setActiveSession(response.session);
      setSelectedSessionId(response.session.sessionId);
      setReviewMetadata(`${response.providerId} · ${response.model} · 입력 ${response.usage.inputTokens ?? "?"} / 출력 ${response.usage.outputTokens ?? "?"} tokens`);
      setReviewNotice("검토 결과와 초기 finding 결정을 저장했습니다.");
      const items = await listReviewSessions(project.path);
      if (props.project?.path === project.path) setSessions(items);
    } catch (cause: unknown) {
      if (approvedScopeKey === scopeKey() && props.project?.path === project.path) {
        const commandError = toAppCommandError(cause);
        if (commandError.code === "cancelled") {
          setReviewNotice("검토가 취소되었습니다. 결과는 저장되지 않았습니다.");
        } else {
          setReviewError(commandError.message);
        }
        setConsentDecision(null);
      }
    } finally {
      unlistenTaskStarted?.();
      setReviewTaskId(null);
      setReviewRunning(false);
      queueMicrotask(() => returnFocus?.focus());
    }
  }

  async function cancelReview() {
    const taskId = reviewTaskId();
    if (taskId === null) return;
    try {
      await cancelTask(taskId);
      setReviewNotice("검토 취소를 요청했습니다.");
    } catch (cause: unknown) {
      setReviewError(toAppCommandError(cause).message);
    }
  }

  async function decideFinding(findingId: string, status: "approved" | "rejected") {
    const projectPath = props.project?.path;
    const session = activeSession();
    if (!projectPath || !session || session.validationState === "invalid") return;
    const decisionKey = `${session.sessionId}:${findingId}`;
    if (savingDecisionKeys().includes(decisionKey)) return;
    setSavingDecisionKeys((current) => [...current, decisionKey]);
    setSessionError(null);
    try {
      const decision = await persistFindingDecision(projectPath, session.sessionId, findingId, status);
      setActiveSession((current) => current?.sessionId !== session.sessionId ? current : {
        ...current,
        findings: current.findings.map((item) => item.findingId === findingId ? { ...item, decision } : item),
      });
    } catch (cause: unknown) {
      setSessionError(toAppCommandError(cause).message);
    } finally {
      setSavingDecisionKeys((current) => current.filter((key) => key !== decisionKey));
    }
  }

  async function createFindingFixTask(findingId: string) {
    const projectPath = props.project?.path;
    const session = activeSession();
    if (!projectPath || !session || creatingFixTaskId() || session.validationState === "invalid") return;
    setCreatingFixTaskId(findingId);
    setSessionError(null);
    try {
      const task = await createFixTask(projectPath, session.sessionId, findingId);
      setFixTasks((tasks) => [task, ...tasks.filter((item) => item.taskId !== task.taskId)]);
    } catch (cause: unknown) {
      setSessionError(toAppCommandError(cause).message);
    } finally {
      setCreatingFixTaskId(null);
    }
  }

  async function changeFixTaskWorkspace(task: StoredFixTask, action: "prepare" | "remove") {
    const projectPath = props.project?.path;
    if (!projectPath || workspaceTaskId() !== null) return;
    setWorkspaceTaskId(task.taskId);
    setSessionError(null);
    try {
      const updated = action === "prepare"
        ? await prepareFixTaskWorkspace(projectPath, task.taskId)
        : await removeFixTaskWorkspace(projectPath, task.taskId);
      setFixTasks((tasks) => tasks.map((item) => item.taskId === updated.taskId ? updated : item));
    } catch (cause: unknown) {
      setSessionError(toAppCommandError(cause).message);
    } finally {
      setWorkspaceTaskId(null);
    }
  }

  return (
    <div class="ai-review-workspace">
      <div class="content-heading">
        <div>
          <p class="eyebrow">AI REVIEW · PRIVACY</p>
          <h1>AI 리뷰 설정</h1>
          <p class="content-description">
            제공자 자격 증명은 운영체제의 보안 저장소에 보관합니다.
          </p>
        </div>
      </div>

      <section class="ai-settings-card" aria-labelledby="credential-title">
        <div class="ai-settings-heading">
          <div>
            <h2 id="credential-title">제공자 자격 증명</h2>
            <p>키는 화면에 다시 표시하거나 앱 데이터베이스에 저장하지 않습니다.</p>
          </div>
          <button class="secondary-button" type="button" disabled={loading() || busyProvider() !== null} onClick={() => void refreshStatuses()}>
            새로고침
          </button>
        </div>

        <div class="credential-provider-list" aria-live="polite">
          <Show when={!loading()} fallback={<p class="muted-message">OS 저장소 상태를 확인하고 있습니다.</p>}>
            {statuses().map((status) => (
              <div class="credential-provider-row">
                <div>
                  <strong>{status.displayName}</strong>
                  <span classList={{ "credential-configured": status.configured }}>
                    {status.configured ? "저장됨 · 값 숨김" : "설정되지 않음"}
                  </span>
                </div>
                <Show when={status.configured}>
                  <button
                    class="text-button danger-text"
                    type="button"
                    disabled={busyProvider() !== null}
                    onClick={() => void removeCredential(status.provider)}
                  >
                    {busyProvider() === status.provider ? "처리 중..." : "삭제"}
                  </button>
                </Show>
              </div>
            ))}
          </Show>
        </div>

        <div class="credential-provider-row subscription-auth-row">
          <div>
            <strong>OpenAI ChatGPT 구독</strong>
            <span classList={{ "credential-configured": subscriptionStatus()?.connected }}>
              {subscriptionStatus()?.connected
                ? `연결됨${subscriptionStatus()?.accountEmail ? ` · ${subscriptionStatus()?.accountEmail}` : ""}`
                : "API 키와 별도의 공식 브라우저 로그인"}
            </span>
          </div>
          <Show when={subscriptionStatus()?.connected} fallback={
            <button class="secondary-button" type="button" disabled={subscriptionBusy()} onClick={() => void connectOpenAiSubscription()}>
              {subscriptionBusy() ? "브라우저 로그인 중…" : "브라우저로 연결"}
            </button>
          }>
            <button class="text-button danger-text" type="button" disabled={subscriptionBusy()} onClick={() => void disconnectOpenAiSubscriptionAccount()}>
              연결 해제
            </button>
          </Show>
        </div>

        <form class="credential-form" onSubmit={(event) => { event.preventDefault(); void saveSelectedCredential(); }}>
          <label for="credential-provider">제공자</label>
          <select
            id="credential-provider"
            value={selectedProvider()}
            disabled={busyProvider() !== null}
            onChange={(event) => {
              setSelectedProvider(event.currentTarget.value as CredentialProvider);
              setCredential("");
            }}
          >
            {providers.map((provider) => <option value={provider.id}>{provider.label}</option>)}
          </select>
          <label for="provider-credential">API 키</label>
          <input
            id="provider-credential"
            type="password"
            autocomplete="new-password"
            spellcheck={false}
            value={credential()}
            onInput={(event) => setCredential(event.currentTarget.value)}
            placeholder="저장할 키를 입력하세요"
          />
          <button class="primary-button" type="submit" disabled={!credential().trim() || busyProvider() !== null}>
            {busyProvider() === selectedProvider() ? "저장 중..." : "저장 또는 교체"}
          </button>
        </form>

        {error() && <p class="inline-error" role="alert">{error()}</p>}
        {notice() && <p class="credential-notice" role="status">{notice()}</p>}
      </section>

      <section class="ai-transfer-note" aria-labelledby="transfer-note-title">
        <span class="transfer-shield" aria-hidden="true">!</span>
        <div>
          <h2 id="transfer-note-title">외부 전송 동의</h2>
          <p>검토를 시작하면 미리 본 문맥이 선택한 외부 AI 제공자에게 전송됩니다. 요청마다 전송 범위와 모델을 확인하고 동의해야 합니다.</p>
          <strong>API 키는 운영체제 자격 증명 저장소에 보관하며, 리뷰 세션에는 전체 문맥을 저장하지 않습니다.</strong>
        </div>
        <div class="review-model-field">
          <label for="review-model">모델 ID</label>
          <input
            id="review-model"
            type="text"
            value={model()}
            maxlength={128}
            autocomplete="off"
            spellcheck={false}
            placeholder="제공자가 지원하는 모델 ID"
            onInput={(event) => setModel(event.currentTarget.value)}
          />
          <span>모델 ID는 제공자에서 발급한 값을 입력하세요.</span>
        </div>
      </section>

      <section class="ai-settings-card transfer-scope-card" aria-labelledby="transfer-scope-title">
        <div class="ai-settings-heading">
          <div>
            <h2 id="transfer-scope-title">검토 전송 범위</h2>
            <p>선택 파일 또는 그래프 항목의 근거를 모아 전송 전에 전체 문맥을 미리봅니다.</p>
          </div>
          <Show when={selectedFile() || graphNodePath()}>
            <div class="scope-kind-switch" role="group" aria-label="문맥 범위 선택">
              <button classList={{ active: scopeKind() === "file" }} type="button" disabled={!selectedFile()} onClick={() => setScopeKind("file")}>파일</button>
              <button classList={{ active: scopeKind() === "graphNode" }} type="button" disabled={!graphNodePath()} onClick={() => setScopeKind("graphNode")}>그래프 항목</button>
            </div>
          </Show>
          <button
            class="primary-button"
            type="button"
            disabled={!props.project || !currentScope() || previewLoading()}
            onClick={() => void loadContextPreview()}
          >
            {previewLoading() ? "문맥 구성 중..." : "문맥 미리보기 구성"}
          </button>
        </div>
        <dl class="transfer-scope-summary">
          <div><dt>제공자</dt><dd>{providers.find((item) => item.id === selectedProvider())?.label}</dd></div>
          <div><dt>선택 범위</dt><dd>{scopeLabel() ?? "선택된 파일 또는 그래프 항목 없음"}</dd></div>
          <div><dt>포함 파일</dt><dd>{contextPreview()?.includedFiles.join(", ") ?? "미리보기 전"}</dd></div>
          <div><dt>예상 크기</dt><dd>{contextPreview() ? `약 ${contextPreview()!.estimatedTokens.toLocaleString()} / ${contextPreview()!.tokenBudget.toLocaleString()} tokens · ${contextPreview()!.tokenEstimateNote}` : "미리보기 전"}</dd></div>
          <div><dt>그래프</dt><dd>{contextPreview()?.graphSnapshotId ? `${contextPreview()!.graphSnapshotId} · ${contextPreview()!.graphCompleteness}` : "저장된 그래프 근거 없음"}</dd></div>
        </dl>
        <Show when={props.selectedEntry?.kind === "file" && isSensitiveTransferPath(props.selectedEntry.relativePath)}>
          <p class="inline-error" role="status">민감 정보 가능성이 있는 파일 이름이 감지되어 전송 대상에서 제외했습니다.</p>
        </Show>
        <Show when={!props.project || !currentScope()}>
          <p class="muted-message">미리보려면 분석 가능한 파일을 선택하거나, 코드 그래프에서 소스 위치가 있는 항목을 선택하세요.</p>
        </Show>
        <Show when={currentScope() && !activeCredential()?.configured}>
          <p class="muted-message">문맥 미리보기는 로컬에서 구성합니다. 외부 전송 동의를 검토하려면 제공자 자격 증명을 설정하세요.</p>
        </Show>
        <Show when={previewError()}><p class="inline-error" role="alert">{previewError()}</p></Show>
        <Show when={contextPreview()}>
          {(preview) => <>
            <div class="context-preview-toolbar">
              <strong>실제 구성 문맥</strong>
              <button class="primary-button" type="button" disabled={!activeCredential()?.configured || !preview().graphSnapshotId || !model().trim()} onClick={openConsentDialog}>검토 시작 전 동의 확인</button>
            </div>
            <pre class="context-preview-body">{preview().context}</pre>
            <Show when={preview().omittedItems.length > 0}>
              <div class="context-omissions"><strong>생략 항목</strong><ul>{preview().omittedItems.map((item) => <li>{item}</li>)}</ul></div>
            </Show>
          </>}
        </Show>
        <Show when={consentDecision()}>
          <p class="credential-notice" role="status">
            {consentDecision() === "approved"
              ? "이번 전송 범위에 동의했습니다. 검토 결과가 로컬 SQLite에 저장됩니다."
              : consentDecision() === "rejected"
                ? "전송을 거부했습니다. 이 범위로 외부 요청을 보내지 않습니다."
                : "동의를 취소했습니다. 이 범위로 외부 요청을 보내지 않습니다."}
          </p>
        </Show>
        <Show when={reviewRunning()}>
          <div class="review-progress" role="status" aria-live="polite">
            <span>선택한 문맥을 제공자에게 보내 리뷰를 진행하고 있습니다.</span>
            <button class="secondary-button" type="button" disabled={reviewTaskId() === null} onClick={() => void cancelReview()}>검토 취소</button>
          </div>
        </Show>
        <Show when={reviewError()}><p class="inline-error" role="alert">{reviewError()}</p></Show>
        <Show when={reviewNotice()}><p class="credential-notice" role="status">{reviewNotice()}</p></Show>
      </section>

      <section class="ai-settings-card review-history" aria-labelledby="review-history-title">
        <div class="ai-settings-heading">
          <div>
            <h2 id="review-history-title">저장된 리뷰</h2>
            <p>검증된 finding과 사용자 결정을 로컬에서 다시 불러옵니다.</p>
          </div>
        </div>
        <Show when={sessionLoading()}><p role="status">리뷰 목록을 불러오는 중입니다…</p></Show>
        <Show when={sessionError()}><p class="inline-error" role="alert">{sessionError()}</p></Show>
        <Show when={sessions().length > 0}>
          <label for="saved-review-session">리뷰 세션</label>
          <select id="saved-review-session" value={selectedSessionId()} onChange={(event) => void loadSession(event.currentTarget.value)}>
            <option value="">세션 선택</option>
            {sessions().map((session) => <option value={session.sessionId}>{new Date(session.createdAtUnixMs).toLocaleString()} · {session.validationState} · {session.findingCount}개 finding</option>)}
          </select>
        </Show>
        <Show when={!sessionLoading() && !sessionError() && sessions().length === 0}>
          <p class="muted-message">저장된 리뷰가 없습니다. 미리보기를 확인하고 리뷰를 시작하세요.</p>
        </Show>
        <Show when={activeSession()}>
          {(session) => <div class="review-results" aria-labelledby="review-results-title">
            <div class="review-results-heading">
              <div>
                <h3 id="review-results-title">리뷰 결과</h3>
                <p>스냅샷 {session().snapshotId} · {new Date(session().createdAtUnixMs).toLocaleString()}</p>
              </div>
              <span class={`review-state review-state-${reviewOutcomeState(session().validationState, session().findings.length)}`}>
                {reviewOutcomeLabel(reviewOutcomeState(session().validationState, session().findings.length))}
              </span>
            </div>
            <Show when={reviewMetadata()}><p class="muted-message">{reviewMetadata()}</p></Show>
            <Show when={session().diagnostics.length > 0}>
              <ul class="review-diagnostics" aria-label="검토 진단">
                {session().diagnostics.map((diagnostic) => <li>{diagnostic.message}</li>)}
              </ul>
            </Show>
            <Show when={session().findings.length === 0}>
              <p class="muted-message">검증된 finding이 없습니다.</p>
            </Show>
            <div class="review-finding-list">
              {session().findings.map((finding) => {
                const result = finding.finding;
                const decision = finding.decision;
                const decisionKey = `${session().sessionId}:${finding.findingId}`;
                const saving = savingDecisionKeys().includes(decisionKey);
                const fixTask = fixTasks().find((task) => task.sessionId === session().sessionId && task.findingId === finding.findingId);
                return <article class="review-finding-card" aria-labelledby={`finding-${finding.findingId}`}>
                  <div class="review-finding-heading">
                    <h4 id={`finding-${finding.findingId}`}>{result.title}</h4>
                    <div class="review-finding-badges">
                      <span class={`severity severity-${result.severity}`}>{result.severity}</span>
                      <span>{result.confidence} 신뢰도</span>
                    </div>
                  </div>
                  <p>{result.description}</p>
                  <dl class="review-finding-meta">
                    <div><dt>분류</dt><dd>{result.category}</dd></div>
                    {result.evidence.map((evidence, index) => <div>
                      <dt>근거 {index + 1}</dt>
                      <dd>
                        <span>{evidence.relativePath}:{evidence.startLine}:{evidence.startColumn}–{evidence.endLine}:{evidence.endColumn}</span>
                        <blockquote class="finding-evidence">{evidence.excerpt}</blockquote>
                      </dd>
                    </div>)}
                  </dl>
                  <Show when={result.affectedSymbols.length > 0}><p><strong>관련 심볼:</strong> {result.affectedSymbols.join(", ")}</p></Show>
                  <Show when={result.suggestion}><p><strong>제안:</strong> {result.suggestion}</p></Show>
                  <Show when={result.testSuggestion}><p><strong>검증 제안:</strong> {result.testSuggestion}</p></Show>
                  <div class="finding-decision-row">
                    <span role="status">사용자 결정: {findingDecisionLabel(decision.status)}{decision.decidedAtUnixMs ? ` · ${new Date(decision.decidedAtUnixMs).toLocaleString()}` : ""}</span>
                    <div role="group" aria-label={`${result.title} 결정`}>
                      <button class="secondary-button" type="button" disabled={saving || session().validationState === "invalid"} aria-pressed={decision.status === "approved"} onClick={() => void decideFinding(finding.findingId, "approved")}>{saving ? "저장 중…" : "승인"}</button>
                      <button class="secondary-button" type="button" disabled={saving || session().validationState === "invalid"} aria-pressed={decision.status === "rejected"} onClick={() => void decideFinding(finding.findingId, "rejected")}>{saving ? "저장 중…" : "거절"}</button>
                    </div>
                  </div>
                  <Show when={decision.status === "approved"}>
                    <div class="finding-decision-row">
                      <span role="status">수정 작업: {fixTask?.status ?? "없음"}</span>
                      <button class="secondary-button" type="button" disabled={Boolean(fixTask) || creatingFixTaskId() !== null} onClick={() => void createFindingFixTask(finding.findingId)}>
                        {creatingFixTaskId() === finding.findingId ? "생성 중…" : fixTask ? "작업 등록됨" : "수정 작업 등록"}
                      </button>
                      <Show when={fixTask} keyed>
                        {(task) => <>
                          <button class="secondary-button" type="button" disabled={workspaceTaskId() !== null || task.workspaceState === "ready"} onClick={() => void changeFixTaskWorkspace(task, "prepare")}>
                            {workspaceTaskId() === task.taskId ? "준비 중…" : task.workspaceState === "ready" ? "workspace 준비됨" : "workspace 준비"}
                          </button>
                          <Show when={task.workspaceState === "ready"}>
                            <button class="secondary-button" type="button" disabled={workspaceTaskId() !== null} onClick={() => void changeFixTaskWorkspace(task, "remove")}>
                              {workspaceTaskId() === task.taskId ? "삭제 중…" : "workspace 삭제"}
                            </button>
                          </Show>
                        </>}
                      </Show>
                    </div>
                  </Show>
                </article>;
              })}
            </div>
          </div>}
        </Show>
      </section>

      <Show when={consentDialogOpen() && contextPreview()}>
        {(preview) => (
          <div class="consent-backdrop">
            <section ref={consentDialog} class="consent-dialog" role="dialog" aria-modal="true" aria-labelledby="consent-title" aria-describedby="consent-description" onKeyDown={handleConsentKeyDown}>
              <p class="eyebrow">EXTERNAL TRANSFER REVIEW</p>
              <h2 id="consent-title">외부 전송 범위를 확인하세요</h2>
              <p id="consent-description" class="consent-intro">동의하면 아래 미리보기 문맥이 선택한 제공자에게 한 번 전송됩니다. API 키는 전송 문맥이나 리뷰 기록에 포함되지 않습니다.</p>
              <dl class="transfer-scope-summary consent-summary">
                <div><dt>제공자</dt><dd>{providers.find((item) => item.id === selectedProvider())?.label}</dd></div>
                <div><dt>모델</dt><dd>{model().trim()}</dd></div>
                <div><dt>프로젝트</dt><dd>{props.project?.name}</dd></div>
                <div><dt>선택 범위</dt><dd>{scopeLabel()}</dd></div>
                <div><dt>포함 파일</dt><dd>{preview().includedFiles.join(", ")}</dd></div>
                <div><dt>예상 크기</dt><dd>{preview().estimatedTokens.toLocaleString()} / {preview().tokenBudget.toLocaleString()} tokens</dd></div>
                <div><dt>생략</dt><dd>{preview().omittedItems.length ? preview().omittedItems.join("; ") : "없음"}</dd></div>
              </dl>
              <pre class="context-preview-body consent-context-preview">{preview().context}</pre>
              <p class="consent-warning">요청은 선택한 외부 AI 제공자에게 전송됩니다. 검토 결과와 각 finding의 승인·거절 결정은 이 기기의 SQLite에 저장됩니다.</p>
              <div class="consent-actions">
                <button class="secondary-button" type="button" onClick={() => closeConsentDialog("cancelled")}>취소</button>
                <button class="secondary-button" type="button" onClick={() => closeConsentDialog("rejected")}>거부</button>
                <button class="primary-button" type="button" disabled={!model().trim() || model().trim().length > 128 || !preview().graphSnapshotId || reviewRunning()} onClick={() => void startReview()}>동의하고 리뷰 요청</button>
              </div>
            </section>
          </div>
        )}
      </Show>
    </div>
  );
}

export default AiReviewWorkspace;
