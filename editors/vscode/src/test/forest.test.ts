import assert from 'node:assert/strict';
import { test } from 'node:test';

import { executableCandidates, expandHome } from '../cli';
import {
  failureMessage,
  isNotARepo,
  locate,
  orderByRecency,
  parseCandidates,
  parseForest,
  parseScripts,
  runningLabel,
  runningNote,
  runningState,
  scriptDescription,
  shellQuote,
  worktreeNameFor,
} from '../forest';

const LIST_JSON = JSON.stringify({
  main: { name: 'api', branch: 'main', path: '/home/u/dev/api', dirty: false, running: {} },
  worktrees_dir: '/home/u/dev/worktrees/api',
  worktrees: [
    {
      name: 'one',
      branch: 'feature/one',
      path: '/home/u/dev/worktrees/api/one',
      dirty: true,
      running: { dev: 2, test: 1 },
    },
    { name: 'two', branch: null, path: '/home/u/dev/worktrees/api/two', dirty: false, running: { dev: 1 } },
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
  assert.throws(
    () =>
      parseForest(
        '{"main": {"name": "api", "branch": null, "path": "/a", "dirty": false}, "worktrees_dir": "x", "worktrees": []}',
      ),
    /main: not a worktree entry/, // an older CLI, without `running`
  );
});

test('runningState counts the instances here and in the other worktrees', () => {
  const forest = parseForest(LIST_JSON);
  const here = '/home/u/dev/worktrees/api/one';
  assert.deepEqual(runningState(forest, 'dev', here), { here: 2, others: 1, otherWorktrees: 1 });
  assert.deepEqual(runningState(forest, 'test', here), { here: 1, others: 0, otherWorktrees: 0 });
  assert.deepEqual(runningState(forest, 'dev', '/home/u/dev/api'), { here: 0, others: 3, otherWorktrees: 2 });
  assert.deepEqual(runningState(forest, 'lint', here), { here: 0, others: 0, otherWorktrees: 0 });
});

test('runningNote counts every instance, however few', () => {
  assert.equal(runningNote({ here: 1, others: 0, otherWorktrees: 0 }), '1 here');
  assert.equal(runningNote({ here: 3, others: 0, otherWorktrees: 0 }), '3 here');
  assert.equal(runningNote({ here: 1, others: 2, otherWorktrees: 2 }), '1 here, 2 elsewhere');
  assert.equal(runningNote({ here: 3, others: 2, otherWorktrees: 1 }), '3 here, 2 elsewhere');
  assert.equal(runningNote({ here: 0, others: 1, otherWorktrees: 1 }), '1 elsewhere');
  assert.equal(runningNote({ here: 0, others: 0, otherWorktrees: 0 }), '');
});

test('runningLabel says how many and where in words', () => {
  assert.equal(runningLabel({ here: 1, others: 0, otherWorktrees: 0 }), 'running here');
  assert.equal(runningLabel({ here: 3, others: 0, otherWorktrees: 0 }), '3 running here');
  assert.equal(runningLabel({ here: 1, others: 2, otherWorktrees: 2 }), 'running here, 2 elsewhere');
  assert.equal(runningLabel({ here: 0, others: 1, otherWorktrees: 1 }), 'running in another worktree');
  assert.equal(runningLabel({ here: 0, others: 2, otherWorktrees: 1 }), '2 running in another worktree');
  assert.equal(runningLabel({ here: 0, others: 3, otherWorktrees: 3 }), '3 running in 3 worktrees');
  assert.equal(runningLabel({ here: 0, others: 0, otherWorktrees: 0 }), '');
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
          migrate: { command: 'npm run db:migrate', hidden: true },
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
      ['test', 'command', 'npm test', ''], // `migrate` is hidden
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

test('executableCandidates puts the bundled copy first, then PATH and the install dirs', () => {
  assert.deepEqual(
    executableCandidates({ path: '/opt/x/bin::/usr/bin', home: '/home/u', bundled: '/ext/bin/workforest' }),
    [
      '/ext/bin/workforest',
      '/opt/x/bin/workforest',
      '/usr/bin/workforest',
      '/home/u/.local/bin/workforest',
      '/opt/homebrew/bin/workforest',
      '/usr/local/bin/workforest',
      '/usr/bin/workforest',
    ],
  );
});

test('executableCandidates without a bundled copy searches PATH and the install dirs', () => {
  const candidates = executableCandidates({ path: undefined, home: '/home/u', bundled: undefined });
  assert.deepEqual(candidates, [
    '/home/u/.local/bin/workforest',
    '/opt/homebrew/bin/workforest',
    '/usr/local/bin/workforest',
    '/usr/bin/workforest',
  ]);
});
