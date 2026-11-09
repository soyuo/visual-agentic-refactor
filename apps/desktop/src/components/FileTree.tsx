import { createEffect, createMemo, createSignal, For, onCleanup } from "solid-js";
import type { ScanEntry } from "../lib/commands";

const ROW_HEIGHT = 30;
const VIEWPORT_HEIGHT = 520;
const OVERSCAN_ROWS = 8;

interface TreeNode {
  entry: ScanEntry;
  children: TreeNode[];
}

interface VisibleTreeNode {
  entry: ScanEntry;
  level: number;
  positionInSet: number;
  setSize: number;
  hasChildren: boolean;
  expanded: boolean;
}

interface FileTreeProps {
  entries: ScanEntry[];
  selectedPath: string | null;
  revealPath: string | null;
  onSelect: (entry: ScanEntry) => void;
}

function buildTree(entries: ScanEntry[]): TreeNode[] {
  const nodes = new Map<string, TreeNode>();
  for (const entry of entries) {
    nodes.set(entry.relativePath, { entry, children: [] });
  }

  const roots: TreeNode[] = [];
  for (const node of nodes.values()) {
    const separator = node.entry.relativePath.lastIndexOf("/");
    const parentPath = separator < 0 ? "" : node.entry.relativePath.slice(0, separator);
    const parent = nodes.get(parentPath);
    if (parent?.entry.kind === "directory") parent.children.push(node);
    else roots.push(node);
  }

  const sortNodes = (siblings: TreeNode[]) => {
    siblings.sort((left, right) => {
      if (left.entry.kind !== right.entry.kind) {
        return left.entry.kind === "directory" ? -1 : 1;
      }
      return left.entry.name.localeCompare(right.entry.name, undefined, {
        numeric: true,
        sensitivity: "base",
      });
    });
    for (const node of siblings) sortNodes(node.children);
  };
  sortNodes(roots);
  return roots;
}

function FileTree(props: FileTreeProps) {
  const roots = createMemo(() => buildTree(props.entries));
  const [expandedPaths, setExpandedPaths] = createSignal<Set<string>>(new Set<string>());
  const [scrollTop, setScrollTop] = createSignal(0);
  const [focusedPath, setFocusedPath] = createSignal<string | null>(null);
  let viewport!: HTMLDivElement;
  const rowRefs = new Map<string, HTMLDivElement>();

  const visibleNodes = createMemo(() => {
    const rows: VisibleTreeNode[] = [];
    const expanded = expandedPaths();

    const visit = (siblings: TreeNode[], level: number) => {
      for (const [index, node] of siblings.entries()) {
        const hasChildren = node.children.length > 0;
        const isExpanded = hasChildren && expanded.has(node.entry.relativePath);
        rows.push({
          entry: node.entry,
          level,
          positionInSet: index + 1,
          setSize: siblings.length,
          hasChildren,
          expanded: isExpanded,
        });
        if (isExpanded) visit(node.children, level + 1);
      }
    };

    visit(roots(), 1);
    return rows;
  });

  let previousEntries = props.entries;
  createEffect(() => {
    const entries = props.entries;
    if (entries !== previousEntries) {
      previousEntries = entries;
      setExpandedPaths(new Set<string>());
      setFocusedPath(null);
      setScrollTop(0);
      if (viewport) viewport.scrollTop = 0;
    }
  });

  const renderedRange = createMemo(() => {
    const all = visibleNodes();
    const firstVisible = Math.floor(scrollTop() / ROW_HEIGHT);
    const start = Math.max(0, firstVisible - OVERSCAN_ROWS);
    const end = Math.min(
      all.length,
      Math.ceil((scrollTop() + VIEWPORT_HEIGHT) / ROW_HEIGHT) + OVERSCAN_ROWS,
    );

    return {
      all,
      start,
      rows: all.slice(start, end),
      topPadding: start * ROW_HEIGHT,
      bottomPadding: Math.max(0, (all.length - end) * ROW_HEIGHT),
    };
  });

  createEffect(() => {
    const all = visibleNodes();
    const focused = focusedPath();
    if (!focused || !all.some((node) => node.entry.relativePath === focused)) {
      setFocusedPath(all[0]?.entry.relativePath ?? null);
    }
  });

  function toggleExpanded(path: string) {
    setExpandedPaths((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }

  function focusNode(index: number) {
    const all = visibleNodes();
    const nextIndex = Math.max(0, Math.min(index, all.length - 1));
    const next = all[nextIndex];
    if (!next) return;

    setFocusedPath(next.entry.relativePath);
    const top = nextIndex * ROW_HEIGHT;
    const bottom = top + ROW_HEIGHT;
    if (top < viewport.scrollTop) viewport.scrollTop = top;
    else if (bottom > viewport.scrollTop + VIEWPORT_HEIGHT) {
      viewport.scrollTop = bottom - VIEWPORT_HEIGHT;
    }
    setScrollTop(viewport.scrollTop);
    requestAnimationFrame(() => rowRefs.get(next.entry.relativePath)?.focus());
  }

  let lastRevealedPath: string | null = null;
  createEffect(() => {
    const path = props.revealPath;
    if (!path) {
      lastRevealedPath = null;
      return;
    }
    if (path === lastRevealedPath) return;
    lastRevealedPath = path;

    const segments = path.split("/");
    const ancestors = new Set<string>();
    let current = "";
    for (const segment of segments.slice(0, -1)) {
      current = current ? `${current}/${segment}` : segment;
      if (props.entries.some((entry) => entry.relativePath === current && entry.kind === "directory")) {
        ancestors.add(current);
      }
    }
    setExpandedPaths((existing) => new Set([...existing, ...ancestors]));
    queueMicrotask(() => {
      const index = visibleNodes().findIndex((node) => node.entry.relativePath === path);
      if (index >= 0) focusNode(index);
    });
  });

  function handleKeyDown(event: KeyboardEvent, node: VisibleTreeNode, index: number) {
    const isDirectory = node.entry.kind === "directory";

    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      props.onSelect(node.entry);
      setFocusedPath(node.entry.relativePath);
      if (node.hasChildren) toggleExpanded(node.entry.relativePath);
    } else if (event.key === "ArrowDown") {
      event.preventDefault();
      focusNode(index + 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      focusNode(index - 1);
    } else if (event.key === "ArrowRight" && isDirectory && node.hasChildren) {
      event.preventDefault();
      if (!node.expanded) toggleExpanded(node.entry.relativePath);
      else focusNode(index + 1);
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      if (isDirectory && node.expanded) {
        toggleExpanded(node.entry.relativePath);
      } else {
        let parentIndex = -1;
        const all = renderedRange().all;
        for (let candidateIndex = index - 1; candidateIndex >= 0; candidateIndex -= 1) {
          if (all[candidateIndex].level < node.level) {
            parentIndex = candidateIndex;
            break;
          }
        }
        if (parentIndex >= 0) focusNode(parentIndex);
      }
    } else if (event.key === "Home") {
      event.preventDefault();
      focusNode(0);
    } else if (event.key === "End") {
      event.preventDefault();
      focusNode(visibleNodes().length - 1);
    }
  }

  return (
    <div
      class="file-tree"
      role="tree"
      aria-label="프로젝트 파일"
      ref={viewport}
      onScroll={() => setScrollTop(viewport.scrollTop)}
    >
      <div aria-hidden="true" style={{ height: `${renderedRange().topPadding}px` }} />
      <For each={renderedRange().rows}>
        {(node, localIndex) => {
          const absoluteIndex = () => renderedRange().start + localIndex();
          const isSelected = () => props.selectedPath === node.entry.relativePath;
          onCleanup(() => rowRefs.delete(node.entry.relativePath));

          return (
            <div
              ref={(element) => rowRefs.set(node.entry.relativePath, element)}
              class="file-tree-node"
              classList={{ selected: isSelected() }}
              role="treeitem"
              aria-level={node.level}
              aria-selected={isSelected()}
              aria-expanded={node.hasChildren ? node.expanded : undefined}
              aria-setsize={node.setSize}
              aria-posinset={node.positionInSet}
              tabIndex={focusedPath() === node.entry.relativePath ? 0 : -1}
              onFocus={() => setFocusedPath(node.entry.relativePath)}
              onClick={() => {
                props.onSelect(node.entry);
                setFocusedPath(node.entry.relativePath);
                if (node.hasChildren) toggleExpanded(node.entry.relativePath);
              }}
              onKeyDown={(event) => handleKeyDown(event, node, absoluteIndex())}
              title={node.entry.relativePath}
            >
              <span class="file-tree-row" style={{ "--tree-level": node.level }}>
                <span class="file-tree-chevron" aria-hidden="true">
                  {node.hasChildren ? (node.expanded ? "▾" : "▸") : ""}
                </span>
                <span class="file-tree-icon" aria-hidden="true">
                  {node.entry.kind === "directory"
                    ? node.expanded
                      ? "▰"
                      : "▣"
                    : "▤"}
                </span>
                <span class="file-tree-name">{node.entry.name}</span>
              </span>
            </div>
          );
        }}
      </For>
      <div aria-hidden="true" style={{ height: `${renderedRange().bottomPadding}px` }} />
    </div>
  );
}

export default FileTree;
