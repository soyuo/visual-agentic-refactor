import { createEffect, createSignal, For, onCleanup, Show } from "solid-js";
import {
  inspectPullRequest,
  listAiModels,
  mergePullRequest,
  preparePullRequestWorkspace,
  previewPullRequestCiFailure,
  pushPullRequestUpdate,
  removePullRequestWorkspace,
  repairPullRequestCi,
  resolvePullRequestConflicts,
  startPullRequestMerge,
  toAppCommandError,
  type AiChangeResult,
  type AiModelOption,
  type CiFailureContext,
  type CredentialProvider,
  type OpenProjectResponse,
  type PullRequestStatus,
} from "../lib/commands";
import "./AiReviewWorkspace.css";

type AiProvider = Exclude<CredentialProvider, "github">;
const providers: Array<{ id: AiProvider; label: string }> = [
  { id: "openai", label: "OpenAI" },
  { id: "anthropic", label: "Anthropic" },
  { id: "claudeCode", label: "Claude Code (공식 앱)" },
  { id: "codex", label: "Codex (공식 앱/CLI)" },
];
const WATCH_INTERVAL_MS = 30_000;
const MAX_AUTO_ATTEMPTS = 3;

interface Props {
  project: OpenProjectResponse | null;
}

function PullRequestWorkspace(props: Props) {
  const [numberInput, setNumberInput] = createSignal("");
  const [status, setStatus] = createSignal<PullRequestStatus | null>(null);
  const [busy, setBusy] = createSignal<string | null>(null);
  const [notice, setNotice] = createSignal<string | null>(null);
  const [provider, setProvider] = createSignal<AiProvider>("openai");
  const [model, setModel] = createSignal("");
  const [modelOptions, setModelOptions] = createSignal<AiModelOption[]>([]);
  const [conflicts, setConflicts] = createSignal<string[] | null>(null);
  const [ciPreview, setCiPreview] = createSignal<CiFailureContext | null>(null);
  const [change, setChange] = createSignal<AiChangeResult | null>(null);
  const [commitMessage, setCommitMessage] = createSignal("");
  const [mergeMethod, setMergeMethod] = createSignal<"merge" | "squash" | "rebase">("merge");
  const [watching, setWatching] = createSignal(false);
  const [autoFix, setAutoFix] = createSignal(false);
  const [attemptedShas, setAttemptedShas] = createSignal<string[]>([]);
  const [log, setLog] = createSignal<string[]>([]);

  const projectPath = () => props.project?.path ?? "";
  const prNumber = () => status()?.number ?? Number.parseInt(numberInput(), 10);
  const addLog = (line: string) => setLog((items) => [`${new Date().toLocaleTimeString()} ${line}`, ...items].slice(0, 50));
  const fail = (error: unknown) => setNotice(toAppCommandError(error).message);

  let modelVersion = 0;
  createEffect(() => {
    const current = provider();
    const version = ++modelVersion;
    listAiModels(current)
      .then((options) => {
        if (version !== modelVersion) return;
        setModelOptions(options);
        if (!model().trim() && options[0]) setModel(options[0].id);
      })
      .catch(() => version === modelVersion && setModelOptions([]));
  });

  async function step<T>(label: string, action: () => Promise<T>): Promise<T | undefined> {
    setBusy(label);
    setNotice(null);
    try {
      return await action();
    } catch (error) {
      fail(error);
      addLog(`${label} 실패: ${toAppCommandError(error).message}`);
      return undefined;
    } finally {
      setBusy(null);
    }
  }

  async function refresh() {
    const number = prNumber();
    if (!projectPath() || !Number.isInteger(number) || number <= 0) {
      setNotice("PR 번호를 입력해 주세요.");
      return;
    }
    const next = await step("상태 조회", () => inspectPullRequest(projectPath(), number));
    if (next) {
      if (status()?.headSha !== next.headSha) {
        setChange(null);
        setConflicts(null);
        setCiPreview(null);
      }
      setStatus(next);
    }
    return next;
  }

  const prepare = () => step("작업 공간 준비", async () => {
    const next = await preparePullRequestWorkspace(projectPath(), prNumber());
    setStatus(next);
    setChange(null);
    setConflicts(null);
    addLog(`격리 작업 공간을 ${next.headSha.slice(0, 7)}로 준비했습니다.`);
    return next;
  });

  const removeWorkspace = () => step("작업 공간 삭제", async () => {
    await removePullRequestWorkspace(projectPath(), prNumber());
    setChange(null);
    setConflicts(null);
    await refresh();
  });

  const startMerge = () => step("base 병합", async () => {
    const result = await startPullRequestMerge(projectPath(), prNumber());
    setConflicts(result.conflictedFiles);
    addLog(result.clean ? "충돌 없이 병합되었습니다. 커밋 후 push할 수 있습니다." : `충돌 파일 ${result.conflictedFiles.length}개`);
    if (result.clean) setCommitMessage(`Merge ${status()?.baseRef} into ${status()?.headRef}`);
  });

  function confirmTransfer(kind: string, files: string[]) {
    return window.confirm(
      `${kind}을(를) 위해 아래 내용이 ${providers.find((item) => item.id === provider())?.label} (${model().trim()})로 전송됩니다.\n\n` +
      files.map((file) => `• ${file}`).join("\n") +
      "\n\n민감 파일은 제외됩니다. 결과는 격리 작업 공간에만 반영되고, 커밋·push는 따로 승인합니다. 계속할까요?",
    );
  }

  const resolveConflicts = () => {
    const files = conflicts() ?? [];
    if (!confirmTransfer("충돌 해결", files)) return;
    return step("AI 충돌 해결", async () => {
      const result = await resolvePullRequestConflicts(projectPath(), prNumber(), provider(), model().trim());
      setChange(result);
      setCommitMessage(`Merge ${status()?.baseRef} into ${status()?.headRef} (AI 충돌 해결)`);
      addLog(`AI가 충돌을 해결했습니다: ${result.summary}`);
    });
  };

  const loadCiPreview = () => step("CI 로그 조회", async () => {
    const preview = await previewPullRequestCiFailure(projectPath(), prNumber());
    setCiPreview(preview);
    return preview;
  });

  async function repairCi(confirmed: boolean) {
    const preview = ciPreview() ?? await loadCiPreview();
    if (!preview) return undefined;
    if (!confirmed && !confirmTransfer("CI 수정", [`CI 실패 로그 (run ${preview.runId})`, ...preview.candidateFiles])) return undefined;
    return step("AI CI 수정", async () => {
      const result = await repairPullRequestCi(projectPath(), prNumber(), provider(), model().trim());
      setChange(result);
      setCommitMessage(`fix(ci): CI 실패 수정 (run ${preview.runId})`);
      addLog(`AI가 CI 수정 패치를 만들었습니다: ${result.summary}`);
      return result;
    });
  }

  const push = (confirmed: boolean) => {
    const current = status();
    if (!current) return undefined;
    if (!confirmed && !window.confirm(`격리 작업 공간의 변경을 커밋하고 ${current.headRef}에 push합니다. 강제 push는 하지 않습니다. 계속할까요?`)) return undefined;
    return step("커밋·push", async () => {
      const pushed = await pushPullRequestUpdate(projectPath(), current.number, commitMessage(), current.headSha);
      addLog(`커밋 ${pushed.commitSha.slice(0, 7)}을(를) push했습니다.`);
      setChange(null);
      setConflicts(null);
      setCiPreview(null);
      await refresh();
      return pushed;
    });
  };

  const merge = () => {
    const current = status();
    if (!current || !window.confirm(`PR #${current.number}을(를) ${current.baseRef}에 ${mergeMethod()} 방식으로 병합합니다. 계속할까요?`)) return;
    return step("PR 병합", async () => {
      setStatus(await mergePullRequest(projectPath(), current.number, current.headSha, mergeMethod()));
      addLog("PR을 병합했습니다.");
      setWatching(false);
    });
  };

  function toggleAutoFix(enabled: boolean) {
    if (enabled && !window.confirm(
      `CI가 실패하면 AI가 수정 패치를 만들어 자동으로 커밋·push합니다 (최대 ${MAX_AUTO_ATTEMPTS}회, 강제 push 없음).\n` +
      "CI 로그와 관련 파일이 선택한 AI 제공자로 전송됩니다. 허용할까요?",
    )) return;
    setAutoFix(enabled);
  }

  // CI watch: poll the PR; on a new failing head, optionally run one bounded auto-repair attempt.
  async function watchTick() {
    if (busy()) return;
    const current = await refresh();
    if (!current) return;
    if (current.state !== "OPEN") {
      addLog(`PR 상태가 ${current.state}입니다. 감시를 멈춥니다.`);
      setWatching(false);
      return;
    }
    if (current.checksState === "success") {
      addLog("CI가 모두 성공했습니다. 감시를 멈춥니다.");
      setWatching(false);
      return;
    }
    if (current.checksState !== "failure" || !autoFix()) return;
    if (attemptedShas().includes(current.headSha)) return;
    if (attemptedShas().length >= MAX_AUTO_ATTEMPTS) {
      addLog(`자동 수정 ${MAX_AUTO_ATTEMPTS}회를 모두 사용했습니다. 직접 확인이 필요합니다.`);
      setAutoFix(false);
      return;
    }
    if (!model().trim()) return;
    setAttemptedShas((items) => [...items, current.headSha]);
    addLog(`자동 수정 시도 ${attemptedShas().length}/${MAX_AUTO_ATTEMPTS}`);
    if (!await prepare()) return;
    setCiPreview(null);
    const result = await repairCi(true);
    if (!result) {
      setAutoFix(false);
      return;
    }
    await push(true);
  }

  createEffect(() => {
    if (!watching()) return;
    void watchTick();
    const timer = window.setInterval(() => void watchTick(), WATCH_INTERVAL_MS);
    onCleanup(() => window.clearInterval(timer));
  });

  const aiReady = () => Boolean(model().trim()) && !busy();
  const canMerge = () => {
    const current = status();
    return Boolean(current && current.state === "OPEN" && current.mergeable === "MERGEABLE" && (current.checksState === "success" || current.checksState === "none"));
  };

  return (
    <div class="ai-review-workspace">
      <div class="content-heading">
        <div>
          <p class="eyebrow">PULL REQUEST · CI</p>
          <h1>PR 관리</h1>
          <p class="content-description">
            PR 충돌과 CI 실패를 격리 작업 공간에서 AI로 고치고, diff를 확인한 뒤 승인해 push·병합합니다.
          </p>
        </div>
      </div>

      <section class="ai-settings-card review-setup-bar" aria-label="PR과 AI 제공자">
        <div class="review-setup-field">
          <label for="pr-number">PR 번호</label>
          <input id="pr-number" type="text" inputmode="numeric" value={numberInput()} placeholder="예: 92"
            onInput={(event) => { setNumberInput(event.currentTarget.value.replace(/\D/g, "")); setStatus(null); setAttemptedShas([]); }}
            onKeyDown={(event) => event.key === "Enter" && void refresh()} />
        </div>
        <div class="review-setup-field">
          <label for="pr-provider">AI 제공자</label>
          <select id="pr-provider" value={provider()} onChange={(event) => setProvider(event.currentTarget.value as AiProvider)}>
            {providers.map((item) => <option value={item.id}>{item.label}</option>)}
          </select>
        </div>
        <div class="review-setup-field">
          <label for="pr-model">모델 ID</label>
          <input id="pr-model" type="text" list="pr-model-options" value={model()} maxlength={128} onInput={(event) => setModel(event.currentTarget.value)} />
          <datalist id="pr-model-options"><For each={modelOptions()}>{(option) => <option value={option.id}>{option.displayName}</option>}</For></datalist>
        </div>
        <button class="primary-button" type="button" disabled={Boolean(busy()) || !numberInput()} onClick={() => void refresh()}>
          {busy() === "상태 조회" ? "조회 중…" : "상태 불러오기"}
        </button>
      </section>

      <Show when={notice()}><p class="credential-notice" role="status">{notice()}</p></Show>

      <Show when={status()} keyed>
        {(pr) => <div class="fix-steps">
          <section class="fix-step" data-done={pr.state === "MERGED" ? "true" : "false"}>
            <header><span>#</span><strong>{pr.title}</strong><em>{pr.headRef} → {pr.baseRef} · {pr.headSha.slice(0, 7)} · {pr.state}</em></header>
            <div class="fix-step-actions">
              <span class="status-chip" classList={{ ok: pr.mergeable === "MERGEABLE" }}>
                {pr.mergeable === "MERGEABLE" ? "병합 가능" : pr.mergeable === "CONFLICTING" ? "충돌 있음" : "병합 상태 확인 중"}
              </span>
              <span class="status-chip" classList={{ ok: pr.checksState === "success" }}>
                CI: {{ success: "성공", failure: "실패", pending: "진행 중", none: "없음" }[pr.checksState]}
              </span>
              <a class="text-button" href={pr.url} target="_blank" rel="noreferrer">GitHub에서 보기</a>
            </div>
            <Show when={pr.checks.length}>
              <ul class="review-diagnostics" aria-label="CI 체크">
                <For each={pr.checks}>{(check) => <li>{check.name}: {check.state}</li>}</For>
              </ul>
            </Show>
          </section>

          <section class="fix-step" data-done={pr.workspaceReady ? "true" : "false"}>
            <header><span>1</span><strong>격리 작업 공간</strong><em>{pr.workspaceReady ? "준비됨" : "원본 작업 트리는 건드리지 않습니다"}</em></header>
            <div class="fix-step-actions">
              <button class="primary-button" type="button" disabled={Boolean(busy())} onClick={() => void prepare()}>
                {busy() === "작업 공간 준비" ? "준비 중…" : pr.workspaceReady ? "최신 head로 다시 준비" : "작업 공간 준비"}
              </button>
              <Show when={pr.workspaceReady}>
                <button class="text-button" type="button" disabled={Boolean(busy())} onClick={() => void removeWorkspace()}>작업 공간 삭제</button>
              </Show>
            </div>
          </section>

          <Show when={pr.workspaceReady && pr.mergeable === "CONFLICTING"}>
            <section class="fix-step" data-done={change() && conflicts() ? "true" : "false"}>
              <header><span>2</span><strong>충돌 해결</strong><em>{conflicts() ? `${conflicts()!.length}개 파일` : `${pr.baseRef} 병합 필요`}</em></header>
              <Show when={conflicts()?.length}>
                <p class="fix-step-note">{conflicts()!.join(" · ")}</p>
              </Show>
              <div class="fix-step-actions">
                <button class="secondary-button" type="button" disabled={Boolean(busy()) || conflicts() !== null} onClick={() => void startMerge()}>
                  {busy() === "base 병합" ? "병합 중…" : `${pr.baseRef} 병합 시작`}
                </button>
                <button class="primary-button" type="button" disabled={!aiReady() || !conflicts()?.length || Boolean(change())} onClick={() => void resolveConflicts()}>
                  {busy() === "AI 충돌 해결" ? "AI 해결 중…" : "AI로 충돌 해결"}
                </button>
              </div>
            </section>
          </Show>

          <Show when={pr.workspaceReady && pr.checksState === "failure"}>
            <section class="fix-step" data-done={change() && ciPreview() ? "true" : "false"}>
              <header><span>2</span><strong>CI 실패 수정</strong><em>{ciPreview() ? `run ${ciPreview()!.runId}` : "실패 로그 확인 필요"}</em></header>
              <Show when={ciPreview()} keyed>
                {(preview) => <>
                  <p class="fix-step-note">수정 대상: {preview.candidateFiles.length ? preview.candidateFiles.join(" · ") : "로그에서 찾지 못함"}</p>
                  <details><summary>실패 로그</summary><pre class="patch-diff-preview">{preview.logExcerpt}</pre></details>
                </>}
              </Show>
              <div class="fix-step-actions">
                <button class="secondary-button" type="button" disabled={Boolean(busy())} onClick={() => void loadCiPreview()}>
                  {busy() === "CI 로그 조회" ? "조회 중…" : "실패 로그 보기"}
                </button>
                <button class="primary-button" type="button" disabled={!aiReady() || Boolean(change())} onClick={() => void repairCi(false)}>
                  {busy() === "AI CI 수정" ? "AI 수정 중…" : "AI로 CI 수정"}
                </button>
              </div>
            </section>
          </Show>

          <Show when={change() || conflicts()?.length === 0}>
            <section class="fix-step">
              <header><span>3</span><strong>diff 확인 · 커밋 · push</strong><em>{change()?.files.join(", ") ?? "병합 커밋"}</em></header>
              <Show when={change()} keyed>
                {(result) => <>
                  <p class="fix-step-note">{result.summary}</p>
                  <pre class="patch-diff-preview" aria-label="변경 diff">{result.diff || "(diff 없음)"}</pre>
                </>}
              </Show>
              <div class="review-setup-field">
                <label for="pr-commit-message">커밋 메시지</label>
                <input id="pr-commit-message" type="text" maxlength={500} value={commitMessage()} onInput={(event) => setCommitMessage(event.currentTarget.value)} />
              </div>
              <div class="fix-step-actions">
                <button class="primary-button" type="button" disabled={Boolean(busy()) || !commitMessage().trim()} onClick={() => void push(false)}>
                  {busy() === "커밋·push" ? "push 중…" : "승인 후 커밋·push"}
                </button>
                <button class="text-button" type="button" disabled={Boolean(busy())} onClick={() => void prepare()}>변경 버리기</button>
              </div>
            </section>
          </Show>

          <section class="fix-step" data-done={watching() ? "true" : "false"}>
            <header><span>4</span><strong>CI 감시</strong><em>{watching() ? `${WATCH_INTERVAL_MS / 1000}초마다 확인 중` : "꺼짐"}</em></header>
            <div class="fix-step-actions">
              <button class="secondary-button" type="button" aria-pressed={watching()} disabled={pr.state !== "OPEN"} onClick={() => setWatching(!watching())}>
                {watching() ? "감시 중지" : "성공할 때까지 감시"}
              </button>
              <label class="fix-step-note">
                <input type="checkbox" checked={autoFix()} disabled={!model().trim()} onChange={(event) => { toggleAutoFix(event.currentTarget.checked); event.currentTarget.checked = autoFix(); }} />
                {" "}실패 시 AI 자동 수정·push 허용 ({attemptedShas().length}/{MAX_AUTO_ATTEMPTS})
              </label>
            </div>
          </section>

          <section class="fix-step" data-done={pr.state === "MERGED" ? "true" : "false"}>
            <header><span>5</span><strong>병합</strong><em>{canMerge() ? "병합 가능" : "충돌이 없고 CI가 성공해야 병합할 수 있습니다"}</em></header>
            <div class="fix-step-actions">
              <select aria-label="병합 방식" value={mergeMethod()} onChange={(event) => setMergeMethod(event.currentTarget.value as "merge" | "squash" | "rebase")}>
                <option value="merge">merge commit</option>
                <option value="squash">squash</option>
                <option value="rebase">rebase</option>
              </select>
              <button class="primary-button" type="button" disabled={Boolean(busy()) || !canMerge()} onClick={() => void merge()}>
                {busy() === "PR 병합" ? "병합 중…" : "승인 후 병합"}
              </button>
            </div>
          </section>
        </div>}
      </Show>

      <Show when={log().length}>
        <section class="ai-settings-card" aria-label="PR 작업 기록">
          <h2>작업 기록</h2>
          <ul class="review-diagnostics"><For each={log()}>{(line) => <li>{line}</li>}</For></ul>
        </section>
      </Show>
    </div>
  );
}

export default PullRequestWorkspace;
