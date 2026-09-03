/** Entry point: wires the model, the view, the status bar, and the commands. */

import * as vscode from 'vscode';

import { Cli, bundledExecutable } from './cli';
import * as commands from './commands';
import { ForestModel } from './model';
import { Recency } from './recency';
import { ForestTree, Node } from './tree';

/** What `activate` returns: the smoke test (src/smoke) reads the model. */
export interface Api {
  model: ForestModel;
}

export function activate(context: vscode.ExtensionContext): Api {
  const log = vscode.window.createOutputChannel('Workforest');
  const cli = new Cli((line) => log.appendLine(line), bundledExecutable(context.extensionPath));
  const model = new ForestModel(cli, log, new Recency(context.globalState));
  const view = vscode.window.createTreeView('workforest.forest', { treeDataProvider: new ForestTree(model) });
  const deps: commands.Deps = { cli, model, log };

  const status = vscode.window.createStatusBarItem('workforest.current', vscode.StatusBarAlignment.Left, 50);
  status.name = 'Workforest';
  status.command = 'workforest.open';
  const updateStatus = (): void => {
    const enabled = vscode.workspace.getConfiguration('workforest').get<boolean>('statusBar', true);
    const located = model.primary;
    if (!enabled || !located) {
      status.hide();
      return;
    }
    const { info, isMain } = located;
    status.text = `$(list-tree) ${info.name}${isMain ? ' (main)' : ''}`;
    status.tooltip = new vscode.MarkdownString(
      [
        `Workforest: ${isMain ? 'the main checkout' : `worktree **${info.name}**`}`,
        `branch: \`${info.branch ?? '(detached)'}\``,
        `path: \`${info.path}\``,
        '',
        'Click to open another worktree.',
      ].join('  \n'),
    );
    status.show();
  };

  const command = (id: string, handler: (deps: commands.Deps, node?: Node) => Promise<void>): vscode.Disposable =>
    vscode.commands.registerCommand(id, (node?: Node) => handler(deps, node));

  context.subscriptions.push(
    log,
    model,
    view,
    status,
    model.onDidChange(updateStatus),
    command('workforest.create', commands.create),
    command('workforest.open', commands.open),
    command('workforest.openInNewWindow', commands.openInNewWindow),
    command('workforest.openInCurrentWindow', commands.openInCurrentWindow),
    command('workforest.delete', commands.remove),
    command('workforest.checkout', commands.checkout),
    command('workforest.runScript', commands.runScript),
    command('workforest.stopScript', commands.stopScript),
    command('workforest.openTerminal', commands.openTerminal),
    command('workforest.copyPath', commands.copyPath),
    command('workforest.showConfig', commands.showConfig),
    command('workforest.init', commands.init),
    command('workforest.initLocal', commands.initLocal),
    command('workforest.refresh', () => model.refresh()),
    vscode.commands.registerCommand('workforest.openSettings', () =>
      vscode.commands.executeCommand('workbench.action.openSettings', '@ext:ArkadyBuryakov.workforest'),
    ),
    vscode.workspace.onDidChangeWorkspaceFolders(() => model.refresh()),
    vscode.window.onDidChangeWindowState((state) => {
      if (state.focused) {
        model.scheduleRefresh(0);
      }
    }),
    vscode.workspace.onDidChangeConfiguration((event) => {
      if (event.affectsConfiguration('workforest.statusBar')) {
        updateStatus();
      }
    }),
  );
  // Every opening of a worktree as a window counts, by whatever route.
  const opened = model.onDidChange(() => {
    opened.dispose();
    const located = model.primary;
    if (located) {
      void model.recency.touch(located.info.path);
    }
  });
  void model.refresh();
  return { model };
}

export function deactivate(): void {}
