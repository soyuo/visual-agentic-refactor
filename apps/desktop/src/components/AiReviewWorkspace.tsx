import { createEffect, createSignal, onMount, Show } from "solid-js";
import {
  deleteProviderCredential,
  listProviderCredentials,
  saveProviderCredential,
  toAppCommandError,
  type CredentialProvider,
  type ProviderCredentialStatus,
  type OpenProjectResponse,
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

  const selectedFile = () => {
    const entry = props.selectedEntry;
    return entry?.kind === "file" && entry.fileStatus === "eligible" && !isSensitiveTransferPath(entry.relativePath)
      ? entry
      : null;
  };
  const activeCredential = () => statuses().find((status) => status.provider === selectedProvider());

  createEffect(() => {
    const scopeKey = `${props.project?.path ?? ""}:${selectedProvider()}:${selectedFile()?.relativePath ?? ""}`;
    if (scopeKey !== consentScopeKey()) {
      setConsentScopeKey(scopeKey);
      setConsentDecision(null);
      setConsentDialogOpen(false);
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
          <p>외부 요청 기능은 아직 연결되지 않았습니다. 현재는 이 화면에서 제공자와 파일 범위를 확인하고 명시적으로 동의하거나 거부할 수 있습니다.</p>
          <strong>현재 앱은 외부 요청을 보내지 않습니다. 동의는 이번 화면에서만 유지되며 저장되지 않습니다.</strong>
        </div>
      </section>

      <section class="ai-settings-card transfer-scope-card" aria-labelledby="transfer-scope-title">
        <div class="ai-settings-heading">
          <div>
            <h2 id="transfer-scope-title">검토 전송 범위</h2>
            <p>프로젝트 탐색 화면에서 파일을 선택한 뒤, 전송 전에 범위를 확인합니다.</p>
          </div>
          <button
            class="primary-button"
            type="button"
            disabled={!props.project || !selectedFile() || !activeCredential()?.configured}
            onClick={() => { setConsentDecision(null); setConsentDialogOpen(true); }}
          >
            전송 범위 확인
          </button>
        </div>
        <dl class="transfer-scope-summary">
          <div><dt>제공자</dt><dd>{providers.find((item) => item.id === selectedProvider())?.label}</dd></div>
          <div><dt>파일</dt><dd>{selectedFile()?.relativePath ?? "선택된 분석 가능 파일 없음"}</dd></div>
          <div><dt>범위</dt><dd>{selectedFile() ? "전체 파일 · 줄 범위 지정 없음" : "전송 대상 없음"}</dd></div>
          <div><dt>문맥</dt><dd>{selectedFile() ? "선택 파일 전체 코드, 경로, 기본 검토 지시 · 본문 미리보기 미제공" : "미리보기할 문맥 없음"}</dd></div>
        </dl>
        <Show when={props.selectedEntry?.kind === "file" && isSensitiveTransferPath(props.selectedEntry.relativePath)}>
          <p class="inline-error" role="status">민감 정보 가능성이 있는 파일 이름이 감지되어 전송 대상에서 제외했습니다.</p>
        </Show>
        <Show when={!props.project || !selectedFile()}>
          <p class="muted-message">전송하려면 프로젝트 탐색에서 분석 가능한 파일 하나를 선택하세요.</p>
        </Show>
        <Show when={selectedFile() && !activeCredential()?.configured}>
          <p class="muted-message">선택한 제공자의 자격 증명을 먼저 설정하세요.</p>
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

      <Show when={consentDialogOpen() && selectedFile()}>
        {(file) => (
          <div class="consent-backdrop">
            <section class="consent-dialog" role="dialog" aria-modal="true" aria-labelledby="consent-title">
              <p class="eyebrow">EXTERNAL TRANSFER REVIEW</p>
              <h2 id="consent-title">외부 전송 범위를 확인하세요</h2>
              <p class="consent-intro">아래 범위가 제공자에게 전달될 수 있습니다. 전송은 사용자가 동의한 경우에만 허용됩니다.</p>
              <dl class="transfer-scope-summary consent-summary">
                <div><dt>제공자</dt><dd>{providers.find((item) => item.id === selectedProvider())?.label}</dd></div>
                <div><dt>프로젝트</dt><dd>{props.project?.name}</dd></div>
                <div><dt>파일 경로</dt><dd>{file().relativePath}</dd></div>
                <div><dt>범위</dt><dd>전체 파일 · 줄 범위 지정 없음</dd></div>
                <div><dt>예상 문맥</dt><dd>파일 경로, 파일 전체 코드, 기본 검토 지시 · 본문 미리보기 미제공</dd></div>
              </dl>
              <p class="consent-warning">실제 요청 기능이 연결되지 않았고 전송 문맥 본문도 미리 볼 수 없습니다. 따라서 현재 버전은 동의 후에도 데이터를 보내지 않으며, 이 결정은 저장되거나 재사용되지 않습니다. 실제 전송 기능을 연결할 때는 완성된 문맥 미리보기를 다시 확인하고 동의해야 합니다.</p>
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
