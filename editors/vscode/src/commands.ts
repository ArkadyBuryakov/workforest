/**
 * The commands. Each is a thin flow around one `workforest` subcommand:
 * pick what the CLI needs, confirm what the CLI would have asked on a
 * terminal (we always pass --force / --keep-branch / --delete-branch so it
 * never has to), run it, refresh, and open the result in VS Code.
 */

import * as fs from 'node:fs/promises';
import * as path from 'node:path';
import * as vscode from 'vscode';

import { Cli, CliError, CliMissingError } from './cli';
import {
  Forest,
  ScriptInfo,
  WorktreeInfo,
  failureMessage,
  isLocked,
  isStale,
  oneLine,
  parseCandidates,
  parseMakeScripts,
  parseScripts,
  said,
  scriptDescription,
  shellQuote,
  stateNote,
  worktreeNameFor,
  wouldPrune,
} from './forest';
import { ForestModel } from './model';
import { EntryNode, ForestNode, Node, SCRIPT_ICONS, ScriptNode } from './tree';

export interface Deps {
  cli: Cli;
  model: ForestModel;
  log: vscode.OutputChannel;
}

type OpenMode = 'newWindow' | 'currentWindow' | 'ask';

function settings(): vscode.WorkspaceConfiguration {
  return vscode.workspace.getConfiguration('workforest');
}

// --- picking ---------------------------------------------------------------

async function pickForest(deps: Deps): Promise<Forest | undefined> {
  const forests = deps.model.all;
  if (forests.length === 0) {
    await deps.model.refresh();
  }
  const primary = deps.model.primary;
  if (primary) {
    return primary.forest;
  }
  if (deps.model.all.length === 1) {
    return deps.model.all[0];
  }
  if (deps.model.all.length === 0) {
    void vscode.window.showErrorMessage('Workforest: open a folder inside a git repository first.');
    return undefined;
  }
  const picked = await vscode.window.showQuickPick(
    deps.model.all.map((forest) => ({ label: forest.main.name, description: forest.main.path, forest })),
    { placeHolder: 'Which repository?' },
  );
  return picked?.forest;
}

interface EntryItem extends vscode.QuickPickItem {
  entry: EntryNode;
}

function entryItem(forest: Forest, info: WorktreeInfo, isMain: boolean, deps: Deps): EntryItem {
  const branch = info.branch ?? '(detached)';
  const isCurrent = deps.model.isOpenHere(info.path);
  return {
    label: `$(${isMain ? 'repo' : 'git-branch'}) ${info.name}`,
    description: [isMain ? `main checkout · ${branch}` : branch, stateNote(info), isCurrent ? '(this window)' : '']
      .filter((part) => part.length > 0)
      .join(' '),
    detail: info.path,
    entry: new EntryNode(forest, info, isMain, isCurrent),
  };
}

/** Which rows a command can act on — the same rules the tree's menus
 * follow, for the pickers, which have no menus to hide things in. */
type Eligible = (info: WorktreeInfo) => boolean;

const alive: Eligible = (info) => !isStale(info);
const unlocked: Eligible = (info) => !isLocked(info);

interface PickOptions {
  placeHolder: string;
  includeMain: boolean;
  /** Rows left out of the picker; every row when absent. */
  eligible?: Eligible;
  /** What to say when no row is eligible. */
  none?: string;
}

async function pickEntries(
  deps: Deps,
  options: PickOptions & { many: boolean; forest?: Forest },
): Promise<EntryNode[] | undefined> {
  const forest = options.forest ?? (await pickForest(deps));
  if (!forest) {
    return undefined;
  }
  const eligible = options.eligible ?? (() => true);
  const items: EntryItem[] = [];
  if (options.includeMain) {
    items.push(entryItem(forest, forest.main, true, deps));
  }
  items.push(...forest.worktrees.filter(eligible).map((info) => entryItem(forest, info, false, deps)));
  if (items.length === 0) {
    const none =
      forest.worktrees.length === 0 ? 'no worktrees yet — create one first.' : (options.none ?? 'no worktree to pick.');
    void vscode.window.showInformationMessage(`Workforest: ${none}`);
    return undefined;
  }
  if (options.many) {
    const picked = await vscode.window.showQuickPick(items, {
      placeHolder: options.placeHolder,
      canPickMany: true,
      matchOnDescription: true,
    });
    return picked?.map((item) => item.entry);
  }
  const picked = await vscode.window.showQuickPick(items, {
    placeHolder: options.placeHolder,
    matchOnDescription: true,
  });
  return picked ? [picked.entry] : undefined;
}

/**
 * The worktree this window is in, when it is a managed one: what the
 * header's Delete and Checkout act on, without asking which.
 */
function currentWorktree(deps: Deps): EntryNode | undefined {
  const located = deps.model.primary;
  if (!located || located.isMain) {
    return undefined;
  }
  return new EntryNode(located.forest, located.info, false, true);
}

/** Acting on this window's worktree was nobody's explicit pick: confirm it. */
async function confirmCurrent(question: string, detail: string, verb: string): Promise<boolean> {
  const choice = await vscode.window.showWarningMessage(question, { modal: true, detail }, verb);
  return choice !== undefined;
}

/** How to name a worktree's branch in a sentence. */
function onBranch(info: WorktreeInfo): string {
  return info.branch === null ? 'a detached HEAD' : `branch ${info.branch}`;
}

/** What stands in the way of acting on a row, in the CLI's own words. */
function lockedMessage(info: WorktreeInfo): string {
  const reason = oneLine(info.locked ?? '');
  return `worktree ${info.name} is locked${reason.length > 0 ? ` (${reason})` : ''} — unlock it first.`;
}

function staleMessage(info: WorktreeInfo): string {
  return isLocked(info)
    ? `worktree ${info.name} is stale and locked — unlock it, then prune the stale worktrees.`
    : `worktree ${info.name} is stale: its directory is gone — prune the stale worktrees.`;
}

/**
 * The menus hide what a row cannot do, but a command also arrives from a
 * keybinding, the palette, or the header acting on this window's own
 * worktree: say why not instead of running into the CLI's refusal after a
 * confirmation or two.
 */
function refuse(entries: readonly EntryNode[], options: { stale: boolean; locked: boolean }): boolean {
  for (const { info } of entries) {
    if (options.stale && isStale(info)) {
      void vscode.window.showWarningMessage(`Workforest: ${staleMessage(info)}`);
      return true;
    }
    if (options.locked && isLocked(info)) {
      void vscode.window.showWarningMessage(`Workforest: ${lockedMessage(info)}`);
      return true;
    }
  }
  return false;
}

/** The entry a command was invoked on (tree item), else a picker. */
async function entryFrom(deps: Deps, node: Node | undefined, options: PickOptions): Promise<EntryNode | undefined> {
  if (node instanceof EntryNode && (options.includeMain || !node.isMain)) {
    return node;
  }
  const forest = node instanceof ForestNode ? node.forest : undefined;
  const picked = await pickEntries(deps, { ...options, many: false, ...(forest ? { forest } : {}) });
  return picked?.[0];
}

// --- opening ---------------------------------------------------------------

async function openFolder(deps: Deps, fsPath: string, mode: OpenMode): Promise<void> {
  let forceNewWindow: boolean;
  if (mode === 'ask') {
    const choice = await vscode.window.showQuickPick(
      [
        { label: '$(empty-window) New Window', newWindow: true },
        { label: '$(window) This Window', newWindow: false },
      ],
      { placeHolder: `Open ${fsPath} in…` },
    );
    if (!choice) {
      return;
    }
    forceNewWindow = choice.newWindow;
  } else {
    forceNewWindow = mode === 'newWindow';
  }
  await deps.model.recency.touch(fsPath);
  await vscode.commands.executeCommand('vscode.openFolder', vscode.Uri.file(fsPath), { forceNewWindow });
}

function openMode(): OpenMode {
  return settings().get<OpenMode>('openIn', 'newWindow');
}

// --- error reporting -------------------------------------------------------

const INSTALL_URL = 'https://github.com/ArkadyBuryakov/workforest#install';

async function guarded(deps: Deps, action: () => Promise<void>): Promise<void> {
  try {
    await action();
  } catch (error) {
    if (error instanceof CliMissingError) {
      const choice = await vscode.window.showErrorMessage(
        `Workforest: ${error.message}. This build of the extension carries no copy for your platform; install workforest to use it.`,
        'Install Workforest',
      );
      if (choice) {
        await vscode.env.openExternal(vscode.Uri.parse(INSTALL_URL));
      }
      await deps.model.refresh();
      return;
    }
    if (error instanceof CliError) {
      const choice = await vscode.window.showErrorMessage(`Workforest: ${error.message}`, 'Show Log');
      if (choice) {
        deps.log.show();
      }
      return;
    }
    deps.log.appendLine(`unexpected: ${String(error)}`);
    void vscode.window.showErrorMessage(`Workforest: ${String(error)}`);
  }
}

// --- create ----------------------------------------------------------------

interface BranchItem extends vscode.QuickPickItem {
  branch: string;
}

async function pickBranch(deps: Deps, forest: Forest): Promise<string | undefined> {
  const raw = await deps.cli.expect(['--complete', 'branches'], forest.main.path, 'listing branches');
  const candidates = parseCandidates(raw).map<BranchItem>((c) => ({
    label: c.name,
    description: c.location,
    branch: c.name,
  }));
  const picker = vscode.window.createQuickPick<BranchItem>();
  picker.placeholder = 'Branch to create a worktree for (type a new name to create the branch)';
  picker.matchOnDescription = true;
  picker.items = candidates;
  picker.onDidChangeValue((value) => {
    const query = value.trim();
    if (query.length === 0 || candidates.some((c) => c.branch === query)) {
      picker.items = candidates;
      return;
    }
    picker.items = [
      { label: `$(add) ${query}`, description: 'new branch', alwaysShow: true, branch: query },
      ...candidates,
    ];
  });
  return new Promise((resolve) => {
    picker.onDidAccept(() => {
      const selected = picker.selectedItems[0]?.branch ?? picker.value.trim();
      picker.hide();
      resolve(selected.length > 0 ? selected : undefined);
    });
    picker.onDidHide(() => {
      picker.dispose();
      resolve(undefined);
    });
    picker.show();
  });
}

export async function create(deps: Deps, node?: Node): Promise<void> {
  await guarded(deps, async () => {
    const forest = node instanceof ForestNode ? node.forest : node instanceof EntryNode ? node.forest : await pickForest(deps);
    if (!forest) {
      return;
    }
    const branch = await pickBranch(deps, forest);
    if (!branch) {
      return;
    }
    await vscode.window.withProgress(
      { location: vscode.ProgressLocation.Notification, title: `Workforest: creating a worktree for ${branch}…` },
      () => deps.cli.expect(['create', branch, '--no-open'], forest.main.path, `creating ${branch}`),
    );
    await deps.model.refresh();
    const created = deps.model.all
      .flatMap((f) => f.worktrees)
      .find((w) => w.branch === branch || w.name === worktreeNameFor(branch));
    if (!created) {
      void vscode.window.showInformationMessage(`Workforest: worktree for ${branch} created.`);
      return;
    }
    await openFolder(deps, created.path, openMode());
  });
}

// --- open ------------------------------------------------------------------

async function openEntry(deps: Deps, node: Node | undefined, mode: OpenMode): Promise<void> {
  await guarded(deps, async () => {
    const entry = await entryFrom(deps, node, { placeHolder: 'Worktree to open', includeMain: true, eligible: alive });
    if (entry && !refuse([entry], { stale: true, locked: false })) {
      await openFolder(deps, entry.info.path, mode);
    }
  });
}

export const open = (deps: Deps, node?: Node): Promise<void> => openEntry(deps, node, openMode());
export const openInNewWindow = (deps: Deps, node?: Node): Promise<void> => openEntry(deps, node, 'newWindow');
export const openInCurrentWindow = (deps: Deps, node?: Node): Promise<void> =>
  openEntry(deps, node, 'currentWindow');

export async function openTerminal(deps: Deps, node?: Node): Promise<void> {
  await guarded(deps, async () => {
    const entry = await entryFrom(deps, node, {
      placeHolder: 'Worktree to open a terminal in',
      includeMain: true,
      eligible: alive,
    });
    if (entry && !refuse([entry], { stale: true, locked: false })) {
      const terminal = vscode.window.createTerminal({ name: entry.info.name, cwd: entry.info.path });
      terminal.show();
    }
  });
}

export async function copyPath(deps: Deps, node?: Node): Promise<void> {
  await guarded(deps, async () => {
    const entry = await entryFrom(deps, node, { placeHolder: 'Worktree whose path to copy', includeMain: true });
    if (entry) {
      await vscode.env.clipboard.writeText(entry.info.path);
    }
  });
}

// --- delete / checkout -----------------------------------------------------

/** The CLI's dirty-worktree confirmation, as a modal; refreshes first so
 * the `dirty` flags are current. */
async function confirmDirty(deps: Deps, entries: EntryNode[], verb: string): Promise<EntryNode[] | undefined> {
  await deps.model.refresh();
  const fresh = entries.map((entry) => {
    const forest = deps.model.all.find((f) => f.main.path === entry.forest.main.path) ?? entry.forest;
    const info = forest.worktrees.find((w) => w.path === entry.info.path);
    return info ? new EntryNode(forest, info, false, entry.isCurrent) : undefined;
  });
  const alive = fresh.filter((entry): entry is EntryNode => entry !== undefined);
  const dirty = alive.filter((entry) => entry.info.dirty);
  if (dirty.length > 0) {
    const names = dirty.map((entry) => entry.info.name).join(', ');
    const one = dirty.length === 1;
    const choice = await vscode.window.showWarningMessage(
      `${one ? 'Worktree' : 'Worktrees'} ${names} ${one ? 'has' : 'have'} uncommitted changes.`,
      { modal: true, detail: `They are discarded with the ${one ? 'worktree' : 'worktrees'}.` },
      `${verb} anyway`,
    );
    if (!choice) {
      return undefined;
    }
  }
  return alive;
}

/** Deleting the folder this window shows: move the window to the main
 * checkout, as `wf delete` moves the shell. */
async function leaveIfCurrent(deps: Deps, entries: EntryNode[]): Promise<void> {
  const current = entries.find((entry) => deps.model.isOpenHere(entry.info.path));
  if (current) {
    await openFolder(deps, current.forest.main.path, 'currentWindow');
  }
}

export async function remove(deps: Deps, node?: Node): Promise<void> {
  await guarded(deps, async () => {
    let entries: EntryNode[] | undefined;
    const current = node === undefined ? currentWorktree(deps) : undefined;
    if (node instanceof EntryNode && !node.isMain) {
      entries = [node];
    } else if (current) {
      const detail = `This window's worktree, on ${onBranch(current.info)}.`;
      if (!(await confirmCurrent(`Delete worktree ${current.info.name}?`, detail, 'Delete'))) {
        return;
      }
      entries = [current];
    } else {
      entries = await pickEntries(deps, {
        placeHolder: 'Worktrees to delete',
        includeMain: false,
        eligible: unlocked,
        none: 'every worktree is locked — unlock one to delete it.',
        many: true,
        ...(node instanceof ForestNode ? { forest: node.forest } : {}),
      });
    }
    if (!entries || entries.length === 0) {
      return;
    }
    const confirmed = await confirmDirty(deps, entries, 'Delete');
    // A stale one is deletable — that prunes its record; a locked one is not.
    if (!confirmed || confirmed.length === 0 || refuse(confirmed, { stale: false, locked: true })) {
      return;
    }
    const branches = confirmed.map((entry) => entry.info.branch).filter((b): b is string => b !== null);
    let branchFlag = '--keep-branch';
    if (branches.length > 0) {
      const one = branches.length === 1;
      const choice = await vscode.window.showQuickPick(
        [
          { label: `$(git-branch) No, keep ${one ? 'it' : 'them'}`, flag: '--keep-branch' },
          { label: `$(trash) Yes, delete ${one ? 'it' : 'them'}`, flag: '--delete-branch' },
        ],
        { placeHolder: `Also delete ${one ? `branch ${branches[0]}` : `the branches ${branches.join(', ')}`}?` },
      );
      if (!choice) {
        return;
      }
      branchFlag = choice.flag;
    }
    const forest = confirmed[0]!.forest;
    const names = confirmed.map((entry) => entry.info.name);
    await deps.cli.expect(['delete', ...names, '--force', branchFlag], forest.main.path, 'delete');
    await leaveIfCurrent(deps, confirmed);
    await deps.model.refresh();
  });
}

export async function checkout(deps: Deps, node?: Node): Promise<void> {
  await guarded(deps, async () => {
    const current = node === undefined ? currentWorktree(deps) : undefined;
    if (current && refuse([current], { stale: true, locked: true })) {
      return;
    }
    if (current) {
      const question = `Check out ${current.info.branch ?? current.info.name} in the main checkout?`;
      const detail = `Deletes ${current.info.name}, this window's worktree.`;
      if (!(await confirmCurrent(question, detail, 'Check Out'))) {
        return;
      }
    }
    const entry =
      current ??
      (await entryFrom(deps, node, {
        placeHolder: 'Worktree to check out in the main checkout',
        includeMain: false,
        eligible: (info) => alive(info) && unlocked(info),
        none: 'every worktree is locked or stale.',
      }));
    if (!entry) {
      return;
    }
    const confirmed = await confirmDirty(deps, [entry], 'Check out');
    const target = confirmed?.[0];
    if (!target || refuse([target], { stale: true, locked: true })) {
      return;
    }
    await deps.cli.expect(['checkout', target.info.name, '--force'], target.forest.main.path, 'checkout');
    await leaveIfCurrent(deps, [target]);
    await deps.model.refresh();
    if (!deps.model.isOpenHere(target.forest.main.path)) {
      const choice = await vscode.window.showInformationMessage(
        `Workforest: ${target.info.branch ?? target.info.name} is now in the main checkout.`,
        'Open Main Checkout',
      );
      if (choice) {
        await openFolder(deps, target.forest.main.path, openMode());
      }
    }
  });
}

// --- lock / unlock / prune -------------------------------------------------

export async function lock(deps: Deps, node?: Node): Promise<void> {
  await guarded(deps, async () => {
    const entry = await entryFrom(deps, node, {
      placeHolder: 'Worktree to lock',
      includeMain: false,
      eligible: unlocked,
      none: 'every worktree is locked already.',
    });
    if (!entry) {
      return;
    }
    const reason = await vscode.window.showInputBox({
      title: `Lock worktree ${entry.info.name}`,
      prompt: 'Delete, Checkout and Prune refuse a locked worktree until it is unlocked.',
      placeHolder: 'Why (optional)',
    });
    if (reason === undefined) {
      return; // Esc; an empty answer locks without a reason
    }
    const args = ['lock', entry.info.name];
    if (reason.trim().length > 0) {
      args.push('--reason', reason.trim());
    }
    await deps.cli.expect(args, entry.forest.main.path, 'lock');
    await deps.model.refresh();
  });
}

export async function unlock(deps: Deps, node?: Node): Promise<void> {
  await guarded(deps, async () => {
    const entry = await entryFrom(deps, node, {
      placeHolder: 'Worktree to unlock',
      includeMain: false,
      eligible: isLocked,
      none: 'no worktree is locked.',
    });
    if (!entry) {
      return;
    }
    await deps.cli.expect(['unlock', entry.info.name], entry.forest.main.path, 'unlock');
    await deps.model.refresh();
  });
}

export async function prune(deps: Deps, node?: Node): Promise<void> {
  await guarded(deps, async () => {
    const forest = node instanceof ForestNode ? node.forest : node instanceof EntryNode ? node.forest : await pickForest(deps);
    if (!forest) {
      return;
    }
    // The CLI's own dry run decides what would go: pruning is repository-wide
    // and reaches records this view does not show.
    const preview = await deps.cli.run(['prune', '--dry-run'], forest.main.path);
    if (preview.code !== 0) {
      throw new CliError(failureMessage(preview.stderr, 'prune failed'), preview.code, preview.stderr);
    }
    const plan = said(preview.stderr);
    if (!wouldPrune(preview.stderr)) {
      void vscode.window.showInformationMessage(`Workforest: ${plan || 'no stale worktree records'}.`);
      await deps.model.refresh();
      return;
    }
    const choice = await vscode.window.showWarningMessage(
      'Prune the stale worktree records?',
      {
        modal: true,
        detail: `${plan}\n\nOnly git's records go, never files. A worktree on a drive that is not mounted looks exactly like a deleted one — lock those instead.`,
      },
      'Prune',
    );
    if (!choice) {
      return;
    }
    const result = await deps.cli.run(['prune'], forest.main.path);
    if (result.code !== 0) {
      throw new CliError(failureMessage(result.stderr, 'prune failed'), result.code, result.stderr);
    }
    await deps.model.refresh();
    void vscode.window.showInformationMessage(`Workforest: ${said(result.stderr)}`);
  });
}

// --- scripts ---------------------------------------------------------------

interface ScriptItem extends vscode.QuickPickItem {
  script: ScriptInfo;
}

/** The `scripts` entries plus the makefile targets `wf make` offers here. */
async function loadScripts(deps: Deps, cwd: string): Promise<ScriptInfo[]> {
  const configJson = await deps.cli.expect(['config', '--json'], cwd, 'reading the configuration');
  const make = await deps.cli.run(['--complete', 'make'], cwd);
  return [...parseScripts(configJson), ...(make.code === 0 ? parseMakeScripts(make.stdout, configJson) : [])];
}

async function pickScript(deps: Deps, cwd: string, placeHolder: string): Promise<ScriptInfo | undefined> {
  const scripts = await loadScripts(deps, cwd);
  if (scripts.length === 0) {
    const choice = await vscode.window.showInformationMessage(
      'Workforest: no scripts are configured. Add a `scripts` entry to .workforest.yaml.',
      'Initialize Project Config',
    );
    if (choice) {
      await vscode.commands.executeCommand('workforest.init');
    }
    return undefined;
  }
  const picked = await vscode.window.showQuickPick(
    scripts.map<ScriptItem>((script) => ({
      label: `$(${SCRIPT_ICONS[script.kind]}) ${script.name}`,
      description: scriptDescription(script),
      detail: script.detail,
      script,
    })),
    { placeHolder, matchOnDetail: true },
  );
  return picked?.script;
}

/**
 * The worktree scripts run in: the worktree tree item; for a Scripts view
 * item or the palette, this window's worktree when it is in that forest,
 * else the main checkout (a forest node) or a pick.
 */
async function scriptTarget(deps: Deps, node: Node | undefined, placeHolder: string): Promise<EntryNode | undefined> {
  if (node instanceof EntryNode) {
    return node;
  }
  const primary = deps.model.primary;
  if (node instanceof ScriptNode) {
    if (primary && primary.forest.main.path === node.forest.main.path) {
      return new EntryNode(primary.forest, primary.info, primary.isMain, true);
    }
    return new EntryNode(node.forest, node.forest.main, true, false);
  }
  if (primary && !(node instanceof ForestNode)) {
    return new EntryNode(primary.forest, primary.info, primary.isMain, true);
  }
  return entryFrom(deps, node, { placeHolder, includeMain: true, eligible: alive });
}

/** The script a command was invoked on (Scripts view item), else a picker. */
async function scriptFrom(deps: Deps, node: Node | undefined, target: EntryNode, verb: string): Promise<ScriptInfo | undefined> {
  if (node instanceof ScriptNode) {
    return node.script;
  }
  return pickScript(deps, target.info.path, `Script to ${verb} in ${target.info.name}`);
}

export async function runScript(deps: Deps, node?: Node): Promise<void> {
  await guarded(deps, async () => {
    const target = await scriptTarget(deps, node, 'Worktree to run the script in');
    if (!target || refuse([target], { stale: true, locked: false })) {
      return;
    }
    const script = await scriptFrom(deps, node, target, 'run');
    if (!script) {
      return;
    }
    // The user's shell runs it: Ctrl-C, colors, and a tty exactly like `wf run`.
    const verb = script.kind === 'make' ? 'make' : 'run';
    const terminal = vscode.window.createTerminal({ name: `wf ${verb} ${script.name}`, cwd: target.info.path });
    terminal.sendText(`${shellQuote(deps.cli.executable)} ${verb} ${shellQuote(script.name)}`, true);
    terminal.show();
  });
}

export async function stopScript(deps: Deps, node?: Node): Promise<void> {
  await guarded(deps, async () => {
    const target = await scriptTarget(deps, node, 'Worktree whose script to stop');
    if (!target || refuse([target], { stale: true, locked: false })) {
      return;
    }
    const script = await scriptFrom(deps, node, target, 'stop');
    if (!script) {
      return;
    }
    const stop = script.kind === 'make' ? ['stop', '--make', script.name] : ['stop', script.name];
    await deps.cli.expect(stop, target.info.path, `stopping ${script.name}`);
    void vscode.window.showInformationMessage(`Workforest: stopped ${script.name} in ${target.info.name}.`);
  });
}

// --- configuration ---------------------------------------------------------

export async function showConfig(deps: Deps): Promise<void> {
  await guarded(deps, async () => {
    const forest = await pickForest(deps);
    if (!forest) {
      return;
    }
    const dump = await deps.cli.expect(['config'], forest.main.path, 'reading the configuration');
    const document = await vscode.workspace.openTextDocument({
      language: 'yaml',
      content: `# workforest config — merged, as seen from ${forest.main.path}\n${dump}`,
    });
    await vscode.window.showTextDocument(document, { preview: true });
  });
}

async function scaffold(deps: Deps, local: boolean): Promise<void> {
  await guarded(deps, async () => {
    const forest = await pickForest(deps);
    if (!forest) {
      return;
    }
    let target = path.join(forest.main.path, '.workforest.yaml');
    const args = ['init'];
    if (local) {
      // The CLI takes the first existing IDE folder; this is VS Code, so make ours exist.
      const dir = path.join(forest.main.path, '.vscode');
      await fs.mkdir(dir, { recursive: true });
      target = path.join(dir, '.workforest.yaml');
      args.push('--local');
    }
    await deps.cli.expect(args, forest.main.path, 'init');
    const document = await vscode.workspace.openTextDocument(vscode.Uri.file(target));
    await vscode.window.showTextDocument(document);
  });
}

export const init = (deps: Deps): Promise<void> => scaffold(deps, false);
export const initLocal = (deps: Deps): Promise<void> => scaffold(deps, true);
