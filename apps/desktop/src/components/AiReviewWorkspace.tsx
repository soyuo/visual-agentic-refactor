import { createEffect, createSignal, onMount, Show } from "solid-js";
import {
  deleteProviderCredential,
  listProviderCredentials,
  saveProviderCredential,
  previewReviewContext,
  toAppCommandError,
  type CredentialProvider,
  type GraphSnapshot,
  type ProviderCredentialStatus,
  type OpenProjectResponse,
  type ReviewContextPreview,
  type ReviewContextScope,
  type ScanEntry,
} from "../lib/commands";
import { isSensitiveTransferPath } from "../lib/externalTransferSafety";
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
  const [consentDialogOpen, setConsentDialogOpen] = createSignal(false);
  const [consentDecision, setConsentDecision] = createSignal<"approved" | "rejected" | "cancelled" | null>(null);
  const [consentScopeKey, setConsentScopeKey] = createSignal("");
  const [scopeKind, setScopeKind] = createSignal<"file" | "graphNode">("file");
  const [contextPreview, setContextPreview] = createSignal<ReviewContextPreview | null>(null);
  const [previewLoading, setPreviewLoading] = createSignal(false);
  const [previewError, setPreviewError] = createSignal<string | null>(null);

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

  createEffect(() => {
    const scopeKey = `${props.project?.path ?? ""}:${selectedProvider()}:${scopeKind()}:${selectedFile()?.relativePath ?? ""}:${props.selectedGraphNodeId ?? ""}`;
    if (scopeKey !== consentScopeKey()) {
      setConsentScopeKey(scopeKey);
      setConsentDecision(null);
      setConsentDialogOpen(false);
      setContextPreview(null);
      setPreviewError(null);
    }
  });

  onMount(() => void refreshStatuses());

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
    setPreviewLoading(true);
    setPreviewError(null);
    setContextPreview(null);
    setConsentDecision(null);
    try {
      setContextPreview(await previewReviewContext({
        projectPath: props.project.path,
        scope,
        maxContextTokens: 32_000,
      }));
    } catch (cause: unknown) {
      setPreviewError(toAppCommandError(cause).message);
    } finally {
      setPreviewLoading(false);
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
          <p>외부 요청 기능은 아직 연결되지 않았습니다. 현재는 제공자와 구성된 문맥을 미리 본 뒤 명시적으로 동의하거나 거부할 수 있습니다.</p>
          <strong>현재 앱은 외부 요청을 보내지 않습니다. 동의는 이번 화면에서만 유지되며 저장되지 않습니다.</strong>
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
              <button class="primary-button" type="button" disabled={!activeCredential()?.configured} onClick={() => { setConsentDecision(null); setConsentDialogOpen(true); }}>이 문맥으로 동의 검토</button>
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
              ? "이번 전송 범위에 동의했습니다. 현재 외부 요청 기능은 연결되지 않아 데이터가 전송되지 않았습니다."
              : consentDecision() === "rejected"
                ? "전송을 거부했습니다. 이 범위로 외부 요청을 보내지 않습니다."
                : "동의를 취소했습니다. 이 범위로 외부 요청을 보내지 않습니다."}
          </p>
        </Show>
      </section>

      <Show when={consentDialogOpen() && contextPreview()}>
        {(preview) => (
          <div class="consent-backdrop">
            <section class="consent-dialog" role="dialog" aria-modal="true" aria-labelledby="consent-title">
              <p class="eyebrow">EXTERNAL TRANSFER REVIEW</p>
              <h2 id="consent-title">외부 전송 범위를 확인하세요</h2>
              <p class="consent-intro">아래 범위가 제공자에게 전달될 수 있습니다. 전송은 사용자가 동의한 경우에만 허용됩니다.</p>
              <dl class="transfer-scope-summary consent-summary">
                <div><dt>제공자</dt><dd>{providers.find((item) => item.id === selectedProvider())?.label}</dd></div>
                <div><dt>프로젝트</dt><dd>{props.project?.name}</dd></div>
                <div><dt>선택 범위</dt><dd>{scopeLabel()}</dd></div>
                <div><dt>포함 파일</dt><dd>{preview().includedFiles.join(", ")}</dd></div>
                <div><dt>예상 크기</dt><dd>{preview().estimatedTokens.toLocaleString()} / {preview().tokenBudget.toLocaleString()} tokens</dd></div>
                <div><dt>생략</dt><dd>{preview().omittedItems.length ? preview().omittedItems.join("; ") : "없음"}</dd></div>
              </dl>
              <pre class="context-preview-body consent-context-preview">{preview().context}</pre>
              <p class="consent-warning">이 동의는 사용자 검토 기록일 뿐, 실제 요청 기능은 아직 연결되지 않았습니다. 현재 버전은 외부로 데이터를 보내지 않으며 결정도 저장하거나 재사용하지 않습니다.</p>
              <div class="consent-actions">
                <button class="secondary-button" type="button" onClick={() => { setConsentDecision("cancelled"); setConsentDialogOpen(false); }}>취소</button>
                <button class="secondary-button" type="button" onClick={() => { setConsentDecision("rejected"); setConsentDialogOpen(false); }}>거부</button>
                <button class="primary-button" type="button" onClick={() => { setConsentDecision("approved"); setConsentDialogOpen(false); }}>동의</button>
              </div>
            </section>
          </div>
        )}
      </Show>
    </div>
  );
}

export default AiReviewWorkspace;
