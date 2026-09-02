/**
 * The Workforest view, the container's only view so that its title actions
 * sit in the sidebar header (the JetBrains tool-window toolbar): two
 * collapsible sections, the scripts of this window's forest and the
 * worktrees (forests first when the window spans several; the main
 * checkout, then the worktrees by recency).
 */

import * as vscode from 'vscode';

import {
  Forest,
  ScriptInfo,
  WorktreeInfo,
  runningBadge,
  runningLabel,
  runningState,
  scriptDescription,
} from './forest';
import { ForestModel } from './model';

export type Section = 'scripts' | 'worktrees';

export class SectionNode {
  constructor(readonly section: Section) {}
}

/** The gap between the sections: an empty row, since trees have no
 * separators of their own. */
export class SpacerNode {}

/** A greyed line standing in for an empty section. */
export class PlaceholderNode {
  constructor(
    readonly section: Section,
    readonly text: string,
  ) {}
}

export class ForestNode {
  constructor(readonly forest: Forest) {}
}

export class EntryNode {
  constructor(
    readonly forest: Forest,
    readonly info: WorktreeInfo,
    readonly isMain: boolean,
    readonly isCurrent: boolean,
  ) {}
}

export class ScriptNode {
  constructor(
    readonly forest: Forest,
    readonly script: ScriptInfo,
  ) {}
}

export type Node = SectionNode | SpacerNode | PlaceholderNode | ForestNode | EntryNode | ScriptNode;

/** The uri a script row carries so decorations (the running badge) can be
 * attached to it: `workforest-script:/NAME?MAIN-CHECKOUT-PATH`. */
const SCRIPT_SCHEME = 'workforest-script';

function scriptUri(forest: Forest, script: ScriptInfo): vscode.Uri {
  return vscode.Uri.from({ scheme: SCRIPT_SCHEME, path: `/${script.name}`, query: forest.main.path });
}

/**
 * The badge on a script row: light blue while it runs in the worktree this
 * window is in, orange — with the count of worktrees when there are
 * several — while it runs only elsewhere.
 */
export class ScriptDecorations implements vscode.FileDecorationProvider, vscode.Disposable {
  private readonly emitter = new vscode.EventEmitter<undefined>();
  readonly onDidChangeFileDecorations = this.emitter.event;
  private readonly subscription: vscode.Disposable;

  constructor(private readonly model: ForestModel) {
    this.subscription = model.onDidChange(() => this.emitter.fire(undefined));
  }

  provideFileDecoration(uri: vscode.Uri): vscode.FileDecoration | undefined {
    if (uri.scheme !== SCRIPT_SCHEME) {
      return undefined;
    }
    const forest = this.model.all.find((candidate) => candidate.main.path === uri.query);
    if (!forest) {
      return undefined;
    }
    const state = runningState(forest, uri.path.slice(1), this.model.primary?.info.path);
    const badge = runningBadge(state);
    if (!badge) {
      return undefined;
    }
    return {
      badge: badge.text,
      color: new vscode.ThemeColor(badge.here ? 'charts.blue' : 'charts.orange'),
      tooltip: runningLabel(state),
    };
  }

  dispose(): void {
    this.subscription.dispose();
    this.emitter.dispose();
  }
}

export class ForestTree implements vscode.TreeDataProvider<Node> {
  private readonly emitter = new vscode.EventEmitter<Node | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;

  constructor(private readonly model: ForestModel) {
    model.onDidChange(() => this.emitter.fire(undefined));
  }

  getChildren(node?: Node): Node[] {
    if (node === undefined) {
      // Empty until a forest is loaded: the welcome content explains why.
      return this.model.current === 'ready'
        ? [new SectionNode('scripts'), new SpacerNode(), new SectionNode('worktrees')]
        : [];
    }
    if (node instanceof SectionNode) {
      return node.section === 'scripts' ? this.scripts() : this.worktrees();
    }
    if (node instanceof ForestNode) {
      return this.entries(node.forest);
    }
    return [];
  }

  private scripts(): Node[] {
    const forest = this.model.scriptsForest;
    const scripts = forest ? this.model.scriptsOf(forest) : [];
    if (!forest || scripts.length === 0) {
      return [new PlaceholderNode('scripts', 'No scripts in the config')];
    }
    return scripts.map((script) => new ScriptNode(forest, script));
  }

  private worktrees(): Node[] {
    const forests = this.model.all;
    if (forests.length > 1) {
      return forests.map((forest) => new ForestNode(forest));
    }
    return forests[0] ? this.entries(forests[0]) : [new PlaceholderNode('worktrees', 'No worktrees')];
  }

  getTreeItem(node: Node): vscode.TreeItem {
    if (node instanceof SectionNode) {
      const item = new vscode.TreeItem(SECTION_TITLES[node.section], vscode.TreeItemCollapsibleState.Expanded);
      item.id = `section:${node.section}`; // a stable id keeps the expanded state across refreshes
      item.contextValue = `section.${node.section}`;
      return item;
    }
    if (node instanceof SpacerNode) {
      const item = new vscode.TreeItem('', vscode.TreeItemCollapsibleState.None);
      item.id = 'spacer';
      item.contextValue = 'spacer';
      return item;
    }
    if (node instanceof PlaceholderNode) {
      const item = new vscode.TreeItem(node.text, vscode.TreeItemCollapsibleState.None);
      item.contextValue = 'placeholder';
      if (node.section === 'scripts') {
        item.command = { command: 'workforest.init', title: 'Initialize Project Config' };
        item.tooltip = 'Add a `scripts` entry to .workforest.yaml (click to scaffold one)';
      } else {
        item.command = { command: 'workforest.create', title: 'Create Worktree' };
        item.tooltip = 'Click to create the first worktree';
      }
      return item;
    }
    if (node instanceof ScriptNode) {
      return scriptItem(node, this.model.primary?.info.path);
    }
    if (node instanceof ForestNode) {
      const item = new vscode.TreeItem(node.forest.main.name, vscode.TreeItemCollapsibleState.Expanded);
      item.description = node.forest.worktreesDir;
      item.iconPath = new vscode.ThemeIcon('list-tree');
      item.contextValue = 'forest';
      item.tooltip = `${node.forest.main.path}\nworktrees in ${node.forest.worktreesDir}`;
      return item;
    }
    const { info, isMain, isCurrent } = node;
    const item = new vscode.TreeItem(info.name, vscode.TreeItemCollapsibleState.None);
    const branch = info.branch ?? '(detached)';
    const state = info.dirty ? '●' : '';
    const here = isCurrent ? '(this window)' : '';
    item.description = [isMain ? `main checkout · ${branch}` : branch, state, here]
      .filter((part) => part.length > 0)
      .join(' ');
    item.iconPath = new vscode.ThemeIcon(
      isMain ? 'repo' : 'git-branch',
      isCurrent ? new vscode.ThemeColor('list.highlightForeground') : undefined,
    );
    item.contextValue = `${isMain ? 'main' : 'worktree'}${isCurrent ? '.current' : ''}`;
    item.tooltip = new vscode.MarkdownString(
      [
        `**${info.name}**${isMain ? ' — main checkout' : ''}`,
        `branch: \`${branch}\``,
        `state: ${info.dirty ? 'uncommitted changes' : 'clean'}`,
        `path: \`${info.path}\``,
      ].join('  \n'),
    );
    return item;
  }

  private entries(forest: Forest): EntryNode[] {
    const node = (info: WorktreeInfo, isMain: boolean): EntryNode =>
      new EntryNode(forest, info, isMain, this.model.isOpenHere(info.path));
    return [node(forest.main, true), ...forest.worktrees.map((info) => node(info, false))];
  }
}

const SECTION_TITLES: Record<Section, string> = { scripts: 'Scripts', worktrees: 'Worktrees' };

const SCRIPT_ICONS: Record<ScriptInfo['kind'], string> = {
  command: 'terminal',
  bulk: 'layers',
  pipeline: 'list-ordered',
};

function scriptItem(node: ScriptNode, herePath: string | undefined): vscode.TreeItem {
  const { script } = node;
  const item = new vscode.TreeItem(script.name, vscode.TreeItemCollapsibleState.None);
  const running = runningLabel(runningState(node.forest, script.name, herePath));
  item.description = scriptDescription(script);
  item.iconPath = new vscode.ThemeIcon(SCRIPT_ICONS[script.kind]);
  item.contextValue = 'script';
  item.resourceUri = scriptUri(node.forest, script); // carries the running badge
  item.tooltip = new vscode.MarkdownString(
    [`**${script.name}**`, `\`\`\`sh\n${script.detail}\n\`\`\``, script.background ? 'background' : '', script.exclusive ? 'exclusive' : '', running]
      .filter((part) => part.length > 0)
      .join('  \n'),
  );
  return item;
}
