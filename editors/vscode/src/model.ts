/**
 * What the window knows about its forests: one `workforest list --json`
 * per workspace folder, deduplicated by main checkout, refreshed on demand
 * and when the repository's worktree bookkeeping changes.
 */

import * as fs from 'node:fs/promises';
import * as path from 'node:path';
import * as vscode from 'vscode';

import { Cli, CliMissingError } from './cli';
import {
  Forest,
  Located,
  ScriptInfo,
  failureMessage,
  isNotARepo,
  locate,
  orderByRecency,
  parseForest,
  parseScripts,
} from './forest';
import { Recency, createdAt } from './recency';

export type ForestState = 'missing' | 'noRepo' | 'ready';

const EXIT_CONFIG = 4;

export class ForestModel implements vscode.Disposable {
  private readonly emitter = new vscode.EventEmitter<void>();
  readonly onDidChange = this.emitter.event;

  private forests: Forest[] = [];
  private scripts = new Map<string, ScriptInfo[]>(); // main path → the forest's scripts
  private state: ForestState = 'noRepo';
  private folderLocations = new Map<string, Located>(); // workspace folder fsPath → where it sits
  private watchers: vscode.Disposable[] = [];
  private inFlight: Promise<void> | undefined;
  private queued = false;
  private debounce: NodeJS.Timeout | undefined;
  private lastConfigWarning = '';

  constructor(
    private readonly cli: Cli,
    private readonly log: vscode.OutputChannel,
    readonly recency: Recency,
  ) {}

  get all(): readonly Forest[] {
    return this.forests;
  }

  get current(): ForestState {
    return this.state;
  }

  /** The `scripts` of a forest's merged configuration (empty until loaded). */
  scriptsOf(forest: Forest): readonly ScriptInfo[] {
    return this.scripts.get(forest.main.path) ?? [];
  }

  /** The forest the Scripts view shows: this window's, else the first one. */
  get scriptsForest(): Forest | undefined {
    return this.primary?.forest ?? this.forests[0];
  }

  /** Where the window's first folder sits, when it is part of a forest. */
  get primary(): Located | undefined {
    const first = vscode.workspace.workspaceFolders?.[0];
    return first ? this.folderLocations.get(first.uri.fsPath) : undefined;
  }

  /** Is `fsPath` (a forest entry path) one of this window's folders? */
  isOpenHere(fsPath: string): boolean {
    for (const located of this.folderLocations.values()) {
      if (located.info.path === fsPath) {
        return true;
      }
    }
    return false;
  }

  /** Refresh; concurrent calls collapse into at most one follow-up run. */
  refresh(): Promise<void> {
    if (this.inFlight) {
      this.queued = true;
      return this.inFlight;
    }
    this.inFlight = this.load()
      .catch((error: unknown) => {
        this.log.appendLine(`refresh failed: ${String(error)}`);
      })
      .finally(() => {
        this.inFlight = undefined;
        if (this.queued) {
          this.queued = false;
          void this.refresh();
        }
      });
    return this.inFlight;
  }

  /** A refresh soon, coalescing bursts of file events. */
  scheduleRefresh(delayMs = 300): void {
    if (this.debounce) {
      clearTimeout(this.debounce);
    }
    this.debounce = setTimeout(() => {
      this.debounce = undefined;
      void this.refresh();
    }, delayMs);
  }

  private async load(): Promise<void> {
    const folders = (vscode.workspace.workspaceFolders ?? []).filter((f) => f.uri.scheme === 'file');
    const byMain = new Map<string, Forest>();
    const scripts = new Map<string, ScriptInfo[]>();
    const locations = new Map<string, Located>();
    let state: ForestState = 'noRepo';
    for (const folder of folders) {
      let forest: Forest | undefined;
      try {
        forest = await this.forestOf(folder.uri.fsPath);
      } catch (error) {
        if (error instanceof CliMissingError) {
          state = 'missing';
          break;
        }
        throw error;
      }
      if (!forest) {
        continue;
      }
      state = 'ready';
      const known = byMain.get(forest.main.path);
      if (!known) {
        forest.worktrees = await this.byRecency(forest);
        byMain.set(forest.main.path, forest);
        scripts.set(forest.main.path, await this.scriptsIn(forest));
      }
      const real = await realpath(folder.uri.fsPath);
      const located = locate([known ?? forest], real);
      if (located) {
        locations.set(folder.uri.fsPath, located);
      }
    }
    this.forests = [...byMain.values()];
    this.scripts = scripts;
    this.folderLocations = locations;
    this.state = state;
    await vscode.commands.executeCommand('setContext', 'workforest.state', state);
    await this.recency.prune(
      new Set(this.forests.flatMap((f) => [f.main.path, ...f.worktrees.map((w) => w.path)])),
    );
    this.watch();
    this.emitter.fire();
  }

  private async byRecency(forest: Forest): Promise<Forest['worktrees']> {
    const created = new Map<string, number>();
    for (const worktree of forest.worktrees) {
      created.set(worktree.path, await createdAt(worktree.path));
    }
    return orderByRecency(
      forest.worktrees,
      (p) => this.recency.lastOpened(p),
      (p) => created.get(p) ?? 0,
    );
  }

  private async scriptsIn(forest: Forest): Promise<ScriptInfo[]> {
    const result = await this.cli.run(['config', '--json'], forest.main.path);
    if (result.code !== 0) {
      this.log.appendLine(`config --json failed in ${forest.main.path} (exit ${result.code})`);
      return [];
    }
    try {
      return parseScripts(result.stdout);
    } catch (error) {
      this.log.appendLine(`config --json: ${String(error)}`);
      return [];
    }
  }

  private async forestOf(cwd: string): Promise<Forest | undefined> {
    const result = await this.cli.run(['list', '--json'], cwd);
    if (result.code === 0) {
      return parseForest(result.stdout);
    }
    if (result.code === EXIT_CONFIG) {
      const message = failureMessage(result.stderr, 'configuration error');
      if (message !== this.lastConfigWarning) {
        this.lastConfigWarning = message;
        void vscode.window.showWarningMessage(`Workforest: ${message}`);
      }
    } else if (!isNotARepo(result.stderr)) {
      this.log.appendLine(`list --json failed in ${cwd} (exit ${result.code})`);
    }
    return undefined;
  }

  /**
   * Watch each main checkout's worktree bookkeeping: `.git/worktrees/NAME`
   * appears and disappears with the worktree, its HEAD moves with the
   * branch; `.git/HEAD` moves with `wf checkout`. Index rewrites in there
   * (every `git status`) are ignored, or our own refresh would loop.
   * The project configuration files feed the Scripts view, and
   * `.git/workforest/running/` the running badges.
   */
  private watch(): void {
    for (const watcher of this.watchers) {
      watcher.dispose();
    }
    this.watchers = [];
    for (const forest of this.forests) {
      const gitDir = path.join(forest.main.path, '.git');
      const bookkeeping = path.join(gitDir, 'worktrees');
      const worktrees = vscode.workspace.createFileSystemWatcher(
        new vscode.RelativePattern(vscode.Uri.file(bookkeeping), '**'),
      );
      const relevant = (uri: vscode.Uri): boolean => {
        const parts = path.relative(bookkeeping, uri.fsPath).split(path.sep);
        return parts.length === 1 || (parts.length === 2 && parts[1] === 'HEAD');
      };
      const onEvent = (uri: vscode.Uri): void => {
        if (relevant(uri)) {
          this.scheduleRefresh();
        }
      };
      const head = vscode.workspace.createFileSystemWatcher(
        new vscode.RelativePattern(vscode.Uri.file(gitDir), 'HEAD'),
      );
      // One record file per running script, in every worktree of the project.
      const running = vscode.workspace.createFileSystemWatcher(
        new vscode.RelativePattern(vscode.Uri.file(path.join(gitDir, 'workforest', 'running')), '**'),
      );
      const refreshRunning = (): void => this.scheduleRefresh();
      this.watchers.push(
        worktrees,
        worktrees.onDidCreate(onEvent),
        worktrees.onDidDelete(onEvent),
        worktrees.onDidChange(onEvent),
        head,
        head.onDidChange(() => this.scheduleRefresh()),
        running,
        running.onDidCreate(refreshRunning),
        running.onDidDelete(refreshRunning),
        running.onDidChange(refreshRunning),
      );
      for (const dir of ['', '.vscode', '.idea']) {
        const config = vscode.workspace.createFileSystemWatcher(
          new vscode.RelativePattern(vscode.Uri.file(path.join(forest.main.path, dir)), '.workforest.{yaml,yml,json}'),
        );
        const refresh = (): void => this.scheduleRefresh();
        this.watchers.push(config, config.onDidCreate(refresh), config.onDidDelete(refresh), config.onDidChange(refresh));
      }
    }
  }

  dispose(): void {
    if (this.debounce) {
      clearTimeout(this.debounce);
    }
    for (const watcher of this.watchers) {
      watcher.dispose();
    }
    this.emitter.dispose();
  }
}

async function realpath(fsPath: string): Promise<string> {
  try {
    return await fs.realpath(fsPath);
  } catch {
    return fsPath;
  }
}
