import { createEffect, createMemo, createSignal, createUniqueId, For, onCleanup, Show } from "solid-js";
import "./Dropdown.css";

export interface DropdownOption {
  value: string;
  label: string;
  /** Secondary text shown on the right, e.g. a model ID. */
  hint?: string;
}

interface DropdownProps {
  id?: string;
  value: string;
  options: DropdownOption[];
  onChange: (value: string) => void;
  disabled?: boolean;
  placeholder?: string;
  ariaLabel?: string;
  /** Allows typing a value that is not in the list (combobox). */
  editable?: boolean;
  maxLength?: number;
  emptyText?: string;
}

/** Styled replacement for native select/datalist, whose popups cannot be themed. */
function Dropdown(props: DropdownProps) {
  const listId = createUniqueId();
  const [open, setOpen] = createSignal(false);
  const [active, setActive] = createSignal(0);
  const [query, setQuery] = createSignal<string | null>(null);
  let root: HTMLDivElement | undefined;

  const selected = () => props.options.find((option) => option.value === props.value);
  const visible = createMemo(() => {
    const text = query()?.trim().toLowerCase();
    if (!props.editable || !text) return props.options;
    return props.options.filter((option) =>
      option.value.toLowerCase().includes(text) || option.label.toLowerCase().includes(text));
  });

  function show() {
    if (props.disabled) return;
    const index = visible().findIndex((option) => option.value === props.value);
    setActive(Math.max(0, index));
    setOpen(true);
  }
  function close() {
    setOpen(false);
    setQuery(null);
  }
  function choose(option: DropdownOption) {
    props.onChange(option.value);
    close();
  }

  function onKeyDown(event: KeyboardEvent) {
    const items = visible();
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (!open()) return show();
      if (!items.length) return;
      const step = event.key === "ArrowDown" ? 1 : -1;
      setActive((index) => (index + step + items.length) % items.length);
    } else if (event.key === "Enter") {
      if (open() && items[active()]) {
        event.preventDefault();
        choose(items[active()]);
      } else if (!props.editable) {
        event.preventDefault();
        show();
      }
    } else if (event.key === "Escape" && open()) {
      event.preventDefault();
      close();
    } else if (event.key === "Tab") {
      close();
    } else if (event.key === " " && !props.editable && !open()) {
      event.preventDefault();
      show();
    }
  }

  createEffect(() => {
    if (!open()) return;
    const handler = (event: PointerEvent) => {
      if (root && !root.contains(event.target as Node)) close();
    };
    document.addEventListener("pointerdown", handler);
    onCleanup(() => document.removeEventListener("pointerdown", handler));
  });

  createEffect(() => {
    if (!open()) return;
    const index = active();
    root?.querySelector<HTMLElement>(`[data-index="${index}"]`)?.scrollIntoView({ block: "nearest" });
  });

  const activeId = () => (open() && visible()[active()] ? `${listId}-${active()}` : undefined);

  return (
    <div class="vr-dropdown" ref={root} classList={{ open: open(), disabled: Boolean(props.disabled) }}>
      <Show when={props.editable} fallback={
        <button
          id={props.id}
          type="button"
          class="vr-dropdown-trigger"
          role="combobox"
          aria-haspopup="listbox"
          aria-expanded={open()}
          aria-controls={listId}
          aria-activedescendant={activeId()}
          aria-label={props.ariaLabel}
          disabled={props.disabled}
          onClick={() => (open() ? close() : show())}
          onKeyDown={onKeyDown}
        >
          <span classList={{ "vr-dropdown-placeholder": !selected() }}>
            {selected()?.label ?? props.placeholder ?? "선택"}
          </span>
        </button>
      }>
        <input
          id={props.id}
          class="vr-dropdown-trigger"
          type="text"
          role="combobox"
          aria-autocomplete="list"
          aria-expanded={open()}
          aria-controls={listId}
          aria-activedescendant={activeId()}
          aria-label={props.ariaLabel}
          autocomplete="off"
          spellcheck={false}
          maxlength={props.maxLength}
          disabled={props.disabled}
          placeholder={props.placeholder}
          value={query() ?? props.value}
          onFocus={show}
          onClick={show}
          onInput={(event) => {
            setQuery(event.currentTarget.value);
            props.onChange(event.currentTarget.value);
            setActive(0);
            setOpen(true);
          }}
          onKeyDown={onKeyDown}
        />
      </Show>
      <span class="vr-dropdown-chevron" aria-hidden="true" />
      <Show when={open()}>
        <ul id={listId} class="vr-dropdown-list" role="listbox" aria-label={props.ariaLabel}>
          <For each={visible()} fallback={<li class="vr-dropdown-empty">{props.emptyText ?? "항목이 없습니다"}</li>}>
            {(option, index) => (
              <li
                id={`${listId}-${index()}`}
                data-index={index()}
                role="option"
                aria-selected={option.value === props.value}
                classList={{ active: index() === active(), selected: option.value === props.value }}
                onPointerEnter={() => setActive(index())}
                onPointerDown={(event) => { event.preventDefault(); choose(option); }}
              >
                <span>{option.label}</span>
                <Show when={option.hint && option.hint !== option.label}><small>{option.hint}</small></Show>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </div>
  );
}

export default Dropdown;
