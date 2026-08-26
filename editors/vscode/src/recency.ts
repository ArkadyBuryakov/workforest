/**
 * "Recently used" for the worktree list: when this VS Code last opened the
 * worktree as a window folder — remembered across sessions in the
 * extension's global state — else when the worktree was created.
 */

import * as fs from 'node:fs/promises';
import * as path from 'node:path';
import type * as vscode from 'vscode';

const KEY = 'lastOpened';

export class Recency {
  constructor(private readonly state: vscode.Memento) {}

  private get table(): Record<string, number> {
    return this.state.get<Record<string, number>>(KEY, {});
  }

  async touch(fsPath: string, now: number = Date.now()): Promise<void> {
    await this.state.update(KEY, { ...this.table, [fsPath]: now });
  }

  lastOpened(fsPath: string): number | undefined {
    return this.table[fsPath];
  }

  /** Forget paths that are no longer worktrees of a known forest. */
  async prune(keep: ReadonlySet<string>): Promise<void> {
    const table = this.table;
    const kept = Object.fromEntries(Object.entries(table).filter(([p]) => keep.has(p)));
    if (Object.keys(kept).length !== Object.keys(table).length) {
      await this.state.update(KEY, kept);
    }
  }
}

/** A linked worktree's `.git` is a file git writes once, when it creates it. */
export async function createdAt(worktreePath: string): Promise<number> {
  try {
    return (await fs.stat(path.join(worktreePath, '.git'))).mtimeMs;
  } catch {
    return 0;
  }
}
