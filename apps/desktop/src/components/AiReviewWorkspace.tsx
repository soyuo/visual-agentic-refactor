import { createSignal, onMount, Show } from "solid-js";
import {
  deleteProviderCredential,
  listProviderCredentials,
  saveProviderCredential,
  toAppCommandError,
  type CredentialProvider,
  type ProviderCredentialStatus,
} from "../lib/commands";
import "./AiReviewWorkspace.css";

const providers: Array<{ id: CredentialProvider; label: string }> = [
  { id: "openai", label: "OpenAI" },
  { id: "anthropic", label: "Anthropic" },
];

function AiReviewWorkspace() {
  const [statuses, setStatuses] = createSignal<ProviderCredentialStatus[]>([]);
  const [selectedProvider, setSelectedProvider] = createSignal<CredentialProvider>("openai");
  const [credential, setCredential] = createSignal("");
  const [loading, setLoading] = createSignal(true);
  const [busyProvider, setBusyProvider] = createSignal<CredentialProvider | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [notice, setNotice] = createSignal<string | null>(null);

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
          <p>외부 요청 기능은 아직 연결되지 않았습니다. 제공자, 파일, 범위와 전송할 문맥을 확인하고 동의한 뒤에만 이후 단계에서 요청을 보낼 수 있습니다.</p>
          <strong>현재는 자격 증명만 관리하며, 외부로 전송되는 데이터가 없습니다.</strong>
        </div>
      </section>
    </div>
  );
}

export default AiReviewWorkspace;
