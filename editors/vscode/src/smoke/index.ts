/**
 * Smoke test, run inside a real VS Code by scripts/smoke.sh
 * (`code --extensionTestsPath=out/smoke`): the workspace is a scratch
 * repository with one worktree, `workforest.executable` points at this
 * checkout's CLI. Asserts the forest loads, the view's commands run, and
 * `Show Merged Configuration` produces a document. Throwing fails the run.
 */

import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as path from 'node:path';
import * as vscode from 'vscode';

import type { Api } from '../extension';

const log = (line: string): void => {
  console.log(line);
  if (process.env.SMOKE_DIR) {
    fs.appendFileSync(path.join(process.env.SMOKE_DIR, 'smoke.log'), `${line}\n`);
  }
};

const until = async (what: string, check: () => boolean, ms = 15000): Promise<void> => {
  const start = Date.now();
  while (!check()) {
    if (Date.now() - start > ms) {
      throw new Error(`timed out waiting for ${what}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
};

export async function run(): Promise<void> {
  try {
    await smoke();
    log('smoke ok');
  } catch (error) {
    log(`smoke FAILED: ${String(error)}`);
    throw error;
  }
}

async function smoke(): Promise<void> {
  const extension = vscode.extensions.getExtension<Api>('ArkadyBuryakov.workforest');
  assert.ok(extension, 'extension not found');
  const api = await extension.activate();
  await api.model.refresh();

  assert.equal(api.model.current, 'ready');
  const forests = api.model.all;
  assert.equal(forests.length, 1, `expected one forest, got ${forests.length}`);
  const forest = forests[0]!;
  assert.equal(forest.main.name, 'smoke');
  assert.equal(forest.main.branch, 'main');
  assert.deepEqual(
    forest.worktrees.map((w) => [w.name, w.branch, w.dirty]),
    [['feat', 'feat', true]],
  );
  assert.deepEqual(
    api.model.scriptsOf(forest).map((s) => [s.name, s.detail]),
    [['hello', 'echo hello']],
    'the Scripts view data',
  );
  const primary = api.model.primary;
  assert.ok(primary, 'the window folder is not located in the forest');
  assert.equal(primary.isMain, true);
  assert.equal(api.model.isOpenHere(forest.main.path), true);
  log(`forest ok: ${forest.main.path} → ${forest.worktreesDir}`);

  await vscode.commands.executeCommand('workforest.refresh');

  await vscode.commands.executeCommand('workforest.showConfig');
  await until('the config document', () =>
    (vscode.window.activeTextEditor?.document.getText() ?? '').includes('worktrees_dir:'),
  );
  const text = vscode.window.activeTextEditor!.document.getText();
  assert.match(text, /scripts:\n\s+hello: echo hello/);
  log('showConfig ok');

  await vscode.commands.executeCommand('workforest.copyPath', undefined);
  // no node → a picker opened; dismiss it
  await vscode.commands.executeCommand('workbench.action.closeQuickOpen');
  log('copyPath picker ok');
}
