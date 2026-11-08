import { createMemo, createSignal, For, Show } from "solid-js";
import type { ScanEntry } from "../lib/commands";

interface TreeNode {
  entry: ScanEntry;
  children: TreeNode[];
}

interface FileTreeProps {
  entries: ScanEntry[];
  selectedPath: string | null;
  onSelect: (entry: ScanEntry) => void;
}

interface TreeNodeViewProps {
  node: TreeNode;
  level: number;
  selectedPath: string | null;
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

function TreeNodeView(props: TreeNodeViewProps) {
  const [expanded, setExpanded] = createSignal(false);
  const isDirectory = () => props.node.entry.kind === "directory";
  const hasChildren = () => props.node.children.length > 0;

  function activate() {
    props.onSelect(props.node.entry);
    if (isDirectory()) setExpanded((value) => !value);
  }

  function handleKeyDown(event: KeyboardEvent) {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      activate();
    } else if (event.key === "ArrowRight" && isDirectory() && hasChildren()) {
      event.preventDefault();
      setExpanded(true);
    } else if (event.key === "ArrowLeft" && isDirectory()) {
      event.preventDefault();
      setExpanded(false);
    }
  }

  return (
    <div
      class="file-tree-node"
      classList={{ selected: props.selectedPath === props.node.entry.relativePath }}
      role="treeitem"
      aria-level={props.level}
      aria-selected={props.selectedPath === props.node.entry.relativePath}
      aria-expanded={isDirectory() ? expanded() : undefined}
      tabIndex={0}
      onClick={activate}
      onKeyDown={handleKeyDown}
      title={props.node.entry.relativePath}
    >
      <span class="file-tree-row" style={{ "--tree-level": props.level }}>
        <span class="file-tree-chevron" aria-hidden="true">
          {isDirectory() ? (expanded() ? "▾" : "▸") : ""}
        </span>
        <span class="file-tree-icon" aria-hidden="true">
          {isDirectory() ? (expanded() ? "▰" : "▣") : "▤"}
        </span>
        <span class="file-tree-name">{props.node.entry.name}</span>
      </span>
      <Show when={isDirectory() && expanded() && hasChildren()}>
        <div class="file-tree-children" role="group">
          <For each={props.node.children}>
            {(child) => (
              <TreeNodeView
                node={child}
                level={props.level + 1}
                selectedPath={props.selectedPath}
                onSelect={props.onSelect}
              />
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}

function FileTree(props: FileTreeProps) {
  const roots = createMemo(() => buildTree(props.entries));

  return (
    <div class="file-tree" role="tree" aria-label="프로젝트 파일">
      <For each={roots()}>
        {(node) => (
          <TreeNodeView
            node={node}
            level={1}
            selectedPath={props.selectedPath}
            onSelect={props.onSelect}
          />
        )}
      </For>
    </div>
  );
}

export default FileTree;
