import { createEffect, createMemo, createResource, For, Show } from "solid-js";
import { readProjectSource } from "../lib/commands";

interface SourcePreviewProps {
  projectPath: string;
  relativePath: string;
  /** 1-based line to highlight and scroll into view. */
  line: number | null;
}

/** Read-only local source view for the selected file. */
export default function SourcePreview(props: SourcePreviewProps) {
  const [source] = createResource(
    () => ({ projectPath: props.projectPath, relativePath: props.relativePath }),
    (request) => readProjectSource(request),
  );
  const lines = createMemo(() => source()?.split(/\r?\n/) ?? []);
  let container: HTMLDivElement | undefined;

  createEffect(() => {
    const line = props.line;
    if (!line || lines().length === 0 || !container) return;
    queueMicrotask(() => container?.querySelector(`[data-line="${line}"]`)?.scrollIntoView({ block: "center" }));
  });

  return (
    <section class="source-preview" aria-label={`${props.relativePath} 소스`}>
      <header class="source-preview-heading">
        <code title={props.relativePath}>{props.relativePath}</code>
        <Show when={lines().length > 0}><span>{lines().length.toLocaleString()}줄</span></Show>
      </header>
      <Show when={!source.loading} fallback={<p class="muted-message" role="status">소스를 읽는 중입니다…</p>}>
        <Show when={!source.error} fallback={<p class="inline-error" role="alert">{(source.error as { message?: string })?.message ?? "소스를 읽을 수 없습니다."}</p>}>
          <div class="source-preview-body" ref={container}>
            <For each={lines()}>{(text, index) => (
              <div class="source-line" data-line={index() + 1} classList={{ highlighted: props.line === index() + 1 }}>
                <span class="source-line-number" aria-hidden="true">{index() + 1}</span>
                <span class="source-line-text">{text || " "}</span>
              </div>
            )}</For>
          </div>
        </Show>
      </Show>
    </section>
  );
}
