import assert from 'node:assert/strict';
import { test } from 'node:test';

import { expandHome } from '../cli';
import {
  failureMessage,
  isNotARepo,
  locate,
  orderByRecency,
  parseCandidates,
  parseForest,
  parseScripts,
  scriptDescription,
  shellQuote,
  worktreeNameFor,
} from '../forest';

const LIST_JSON = JSON.stringify({
  main: { name: 'api', branch: 'main', path: '/home/u/dev/api', dirty: false },
  worktrees_dir: '/home/u/dev/worktrees/api',
  worktrees: [
    { name: 'one', branch: 'feature/one', path: '/home/u/dev/worktrees/api/one', dirty: true },
    { name: 'two', branch: null, path: '/home/u/dev/worktrees/api/two', dirty: false },
  ],
});

test('parseForest reads list --json', () => {
  const forest = parseForest(LIST_JSON);
  assert.equal(forest.main.name, 'api');
  assert.equal(forest.worktreesDir, '/home/u/dev/worktrees/api');
  assert.deepEqual(
    forest.worktrees.map((w) => [w.name, w.branch, w.dirty]),
    [
      ['one', 'feature/one', true],
      ['two', null, false],
    ],
  );
});

test('parseForest rejects a foreign shape', () => {
  assert.throws(() => parseForest('{"worktrees": []}'), /unexpected shape/);
  assert.throws(
    () => parseForest('{"main": {"name": 1}, "worktrees_dir": "x", "worktrees": []}'),
    /main: not a worktree entry/,
  );
});

test('locate finds main and worktrees by path', () => {
  const forest = parseForest(LIST_JSON);
  assert.equal(locate([forest], '/home/u/dev/api')?.isMain, true);
  const one = locate([forest], '/home/u/dev/worktrees/api/one');
  assert.equal(one?.isMain, false);
  assert.equal(one?.info.name, 'one');
  assert.equal(locate([forest], '/elsewhere'), undefined);
});

test('orderByRecency prefers last opened, then creation, keeping ties in order', () => {
  const ws = [{ path: 'a' }, { path: 'b' }, { path: 'c' }, { path: 'd' }];
  const lastOpened: Record<string, number> = { b: 50, c: 500 };
  const created: Record<string, number> = { a: 100, b: 100, c: 100, d: 100 };
  assert.deepEqual(
    orderByRecency(ws, (p) => lastOpened[p], (p) => created[p] ?? 0).map((w) => w.path),
    ['c', 'a', 'd', 'b'],
  );
});

test('parseCandidates splits NAME<TAB>LOCATION lines', () => {
  assert.deepEqual(parseCandidates('feat\tlocal, origin\norigin/fix\torigin\nbare\n'), [
    { name: 'feat', location: 'local, origin' },
    { name: 'origin/fix', location: 'origin' },
    { name: 'bare', location: '' },
  ]);
  assert.deepEqual(parseCandidates(''), []);
});

test('parseScripts flattens every entry form, sorted', () => {
  const scripts = parseScripts(
    JSON.stringify({
      config: {
        scripts: {
          test: 'npm test',
          backend: { command: 'docker compose up', background: true, exclusive: true },
          dev: { bulk: ['backend', 'frontend'] },
          fresh: { pipeline: ['migrate', 'dev'], background: true },
        },
      },
      sources: [],
    }),
  );
  assert.deepEqual(
    scripts.map((s) => [s.name, s.kind, s.detail, scriptDescription(s)]),
    [
      ['backend', 'command', 'docker compose up', 'background, exclusive'],
      ['dev', 'bulk', 'bulk: backend, frontend', ''],
      ['fresh', 'pipeline', 'pipeline: migrate → dev', 'background'],
      ['test', 'command', 'npm test', ''],
    ],
  );
  assert.deepEqual(parseScripts('{"config": {}}'), []);
});

test('failureMessage takes the last stderr line without the prefix', () => {
  assert.equal(failureMessage("created worktree\nError: worktree 'x' not found\n", 'fb'), "worktree 'x' not found");
  assert.equal(failureMessage('  \n', 'fallback'), 'fallback');
});

test('isNotARepo matches the CLI phrase', () => {
  assert.equal(isNotARepo('Error: Not inside a git repository\n'), true);
  assert.equal(isNotARepo('Error: something else'), false);
});

test('worktreeNameFor is the last branch component', () => {
  assert.equal(worktreeNameFor('feature/login'), 'login');
  assert.equal(worktreeNameFor('fix'), 'fix');
});

test('shellQuote leaves safe words alone and single-quotes the rest', () => {
  assert.equal(shellQuote('make'), 'make');
  assert.equal(shellQuote("it's here"), `'it'\\''s here'`);
});

test('expandHome expands a leading ~ only', () => {
  assert.equal(expandHome('~/.local/bin/workforest', '/home/u'), '/home/u/.local/bin/workforest');
  assert.equal(expandHome('workforest', '/home/u'), 'workforest');
  assert.equal(expandHome('/opt/~/x', '/home/u'), '/opt/~/x');
});
