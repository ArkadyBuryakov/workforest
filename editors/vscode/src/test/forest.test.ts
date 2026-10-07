import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';

import { executableCandidates, expandHome } from '../cli';
import {
  WorktreeInfo,
  entryContextValue,
  failureMessage,
  isNotARepo,
  locate,
  oneLine,
  orderByRecency,
  parseCandidates,
  parseForest,
  parseMakeScripts,
  parseScripts,
  runningLabel,
  runningNote,
  runningState,
  said,
  scriptDescription,
  shellQuote,
  stateLines,
  stateNote,
  worktreeNameFor,
  wouldPrune,
} from '../forest';

const LIST_JSON = JSON.stringify({
  main: { name: 'api', branch: 'main', path: '/home/u/dev/api', dirty: false, locked: null, prunable: null, running: {} },
  worktrees_dir: '/home/u/dev/worktrees/api',
  worktrees: [
    {
      name: 'one',
      branch: 'feature/one',
      path: '/home/u/dev/worktrees/api/one',
      dirty: true,
      locked: 'on the\nusb drive',
      prunable: null,
      running: { dev: 2, test: 1 },
    },
    {
      name: 'two',
      branch: null,
      path: '/home/u/dev/worktrees/api/two',
      dirty: false,
      locked: null,
      prunable: null,
      running: { dev: 1 },
    },
  ],
});

function info(state: { dirty?: boolean | null; locked?: string | null; prunable?: string | null }): WorktreeInfo {
  return {
    name: 'feat',
    branch: 'feat',
    path: '/home/u/dev/worktrees/api/feat',
    dirty: state.dirty ?? false,
    locked: state.locked ?? null,
    prunable: state.prunable ?? null,
    running: {},
  };
}

test('parseForest reads the lock and the stale flag', () => {
  const stale = JSON.parse(LIST_JSON) as { worktrees: Record<string, unknown>[] };
  stale.worktrees.push({
    name: 'gone',
    branch: 'gone',
    path: '/home/u/dev/worktrees/api/gone',
    dirty: null, // a stale worktree is never asked
    locked: '', // locked without a reason is still locked
    prunable: 'gitdir file points to non-existent location',
    running: {},
  });
  const forest = parseForest(JSON.stringify(stale));
  assert.deepEqual(
    forest.worktrees.map((w) => [w.name, w.dirty, w.locked, w.prunable]),
    [
      ['one', true, 'on the\nusb drive', null],
      ['two', false, null, null],
      ['gone', null, '', 'gitdir file points to non-existent location'],
    ],
  );
  assert.equal(forest.main.locked, null);
});

test('parseForest rejects a listing without the lock and stale fields', () => {
  // An older CLI: better a logged failure than rows whose state is a guess.
  const old = JSON.parse(LIST_JSON) as { main: Record<string, unknown> };
  delete old.main.locked;
  assert.throws(() => parseForest(JSON.stringify(old)), /main: not a worktree entry/);
});

test('entryContextValue is a token bag for every combination of flags', () => {
  // Every (isMain, isCurrent, locked, stale) there is. The main checkout is
  // never locked or stale in practice; the encoding does not depend on that.
  const expected: [boolean, boolean, boolean, boolean, string][] = [
    [false, false, false, false, '|worktree|'],
    [false, false, false, true, '|worktree|stale|'],
    [false, false, true, false, '|worktree|locked|'],
    [false, false, true, true, '|worktree|locked|stale|'],
    [false, true, false, false, '|worktree|current|'],
    [false, true, false, true, '|worktree|current|stale|'],
    [false, true, true, false, '|worktree|current|locked|'],
    [false, true, true, true, '|worktree|current|locked|stale|'],
    [true, false, false, false, '|main|'],
    [true, false, false, true, '|main|stale|'],
    [true, false, true, false, '|main|locked|'],
    [true, false, true, true, '|main|locked|stale|'],
    [true, true, false, false, '|main|current|'],
    [true, true, false, true, '|main|current|stale|'],
    [true, true, true, false, '|main|current|locked|'],
    [true, true, true, true, '|main|current|locked|stale|'],
  ];
  assert.equal(expected.length, 16);
  for (const [isMain, isCurrent, locked, stale, value] of expected) {
    const row = info({ locked: locked ? '' : null, prunable: stale ? 'gone' : null });
    assert.equal(entryContextValue(row, isMain, isCurrent), value);
  }
});

test('every token of a contextValue is found by its own containment test, and no other', () => {
  // What the `when` clauses in package.json do: /\|TOKEN\|/ per flag.
  const tokens = ['main', 'worktree', 'current', 'locked', 'stale'];
  for (const isMain of [false, true]) {
    for (const isCurrent of [false, true]) {
      for (const locked of [false, true]) {
        for (const stale of [false, true]) {
          const row = info({ locked: locked ? 'why' : null, prunable: stale ? 'gone' : null });
          const value = entryContextValue(row, isMain, isCurrent);
          const present = new Set([isMain ? 'main' : 'worktree']);
          if (isCurrent) present.add('current');
          if (locked) present.add('locked');
          if (stale) present.add('stale');
          for (const token of tokens) {
            assert.equal(new RegExp(`\\|${token}\\|`).test(value), present.has(token), `${token} in ${value}`);
          }
        }
      }
    }
  }
});

test('the when clauses test tokens, never the shape of the whole value', () => {
  // Not proof that the menus are right — only VS Code evaluates a `when` —
  // but an anchored or dotted pattern, the kind a new flag silently breaks,
  // cannot come back unnoticed.
  const manifest = JSON.parse(readFileSync(join(__dirname, '..', '..', 'package.json'), 'utf8')) as {
    contributes: { menus: Record<string, { command: string; when?: string }[]> };
  };
  const clauses = Object.values(manifest.contributes.menus)
    .flat()
    .map((item) => item.when ?? '')
    .filter((when) => when.includes('=~'));
  assert.ok(clauses.length >= 14);
  const known = new Set(['main', 'worktree', 'current', 'locked', 'stale']);
  for (const when of clauses) {
    const patterns = [...when.matchAll(/viewItem =~ \/(.*?)\/(?= |\)|$)/g)].map((match) => match[1] ?? '');
    assert.ok(patterns.length > 0, when);
    for (const pattern of patterns) {
      const body = /^\\\|\(?([a-z|]+)\)?\\\|$/.exec(pattern);
      assert.ok(body, `not a token test: ${pattern}`);
      for (const token of (body[1] ?? '').split('|')) {
        assert.ok(known.has(token), `unknown token ${token} in ${when}`);
      }
    }
  }
});

test('stateNote marks what a row cannot say with its branch', () => {
  assert.equal(stateNote(info({})), '');
  assert.equal(stateNote(info({ dirty: true })), '●');
  assert.equal(stateNote(info({ locked: '' })), 'locked');
  assert.equal(stateNote(info({ dirty: true, locked: 'why' })), '● locked');
  assert.equal(stateNote(info({ dirty: null, prunable: 'gone' })), 'stale');
  assert.equal(stateNote(info({ dirty: null, prunable: 'gone', locked: '' })), 'stale locked');
});

test('stateLines spell the state and the lock reason out for the tooltip', () => {
  assert.deepEqual(stateLines(info({})), ['state: clean']);
  assert.deepEqual(stateLines(info({ dirty: true })), ['state: uncommitted changes']);
  assert.deepEqual(stateLines(info({ locked: '' })), ['state: clean', 'locked']);
  assert.deepEqual(stateLines(info({ locked: 'on the\nusb\tdrive' })), ['state: clean', 'locked: on the usb drive']);
  assert.deepEqual(stateLines(info({ dirty: null, prunable: 'gitdir file\nis gone', locked: 'x' })), [
    'state: stale — gitdir file is gone',
    'locked: x',
  ]);
});

test('wouldPrune reads the dry run, said joins what it reported', () => {
  const plan = 'would prune 2 stale worktree records: a, b\n1 stale worktree record is locked: z — unlock to prune\n';
  assert.equal(wouldPrune(plan), true);
  assert.equal(said(plan), 'would prune 2 stale worktree records: a, b · 1 stale worktree record is locked: z — unlock to prune');
  assert.equal(wouldPrune('no stale worktree records\n'), false);
  assert.equal(wouldPrune('1 stale worktree record is locked: z — unlock to prune\n'), false);
  assert.equal(said(''), '');
});

test('oneLine flattens any whitespace', () => {
  assert.equal(oneLine('  a\n\tb   c\n'), 'a b c');
  assert.equal(oneLine(''), '');
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

test('parseMakeScripts turns --complete make lines into make-kind scripts', () => {
  const configJson = JSON.stringify({ config: { make: { exclusive_scripts: ['dev'] } }, sources: [] });
  const scripts = parseMakeScripts('check\ndev\n', configJson);
  assert.deepEqual(
    scripts.map((s) => [s.name, s.kind, s.detail, s.runningKey, scriptDescription(s)]),
    [
      ['check', 'make', 'make check', 'make:check', 'make'],
      ['dev', 'make', 'make dev', 'make:dev', 'make, exclusive'],
    ],
  );
  assert.deepEqual(parseMakeScripts('', configJson), []);
  // no `make` section, or none of the shapes we expect: no target is exclusive
  assert.equal(parseMakeScripts('check\n', '{"config": {}}')[0]?.exclusive, false);
});

test('runningState counts a make target by its make: key', () => {
  const forest = parseForest(
    JSON.stringify({
      main: { name: 'api', branch: 'main', path: '/m', dirty: false, locked: null, prunable: null, running: { 'make:check': 2 } },
      worktrees_dir: '/w',
      worktrees: [],
    }),
  );
  const script = parseMakeScripts('check\n', '{"config": {}}')[0]!;
  assert.deepEqual(runningState(forest, script.runningKey, '/m'), { here: 2, others: 0, otherWorktrees: 0 });
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
