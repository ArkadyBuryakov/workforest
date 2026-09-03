/**
 * Pure data layer: the shapes `workforest` prints and how the extension
 * reads them. Nothing here imports `vscode`, so it is unit-tested with
 * node:test; the CLI's machine-readable output (`list --json`,
 * `config --json`, `--complete` lines) is the only contract in play.
 */

export interface WorktreeInfo {
  name: string;
  branch: string | null; // null when detached
  path: string;
  dirty: boolean;
  running: Record<string, number>; // the scripts running there: name → live instances
}

export interface Forest {
  main: WorktreeInfo;
  worktreesDir: string;
  worktrees: WorktreeInfo[];
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isCountMap(value: unknown): value is Record<string, number> {
  return isRecord(value) && Object.values(value).every((count) => typeof count === 'number');
}

function asWorktree(value: unknown, where: string): WorktreeInfo {
  if (
    !isRecord(value) ||
    typeof value.name !== 'string' ||
    typeof value.path !== 'string' ||
    typeof value.dirty !== 'boolean' ||
    !(typeof value.branch === 'string' || value.branch === null) ||
    !isCountMap(value.running)
  ) {
    throw new Error(`${where}: not a worktree entry`);
  }
  return {
    name: value.name,
    branch: value.branch,
    path: value.path,
    dirty: value.dirty,
    running: value.running,
  };
}

/** Parse `workforest list --json`. */
export function parseForest(json: string): Forest {
  const data: unknown = JSON.parse(json);
  if (!isRecord(data) || typeof data.worktrees_dir !== 'string' || !Array.isArray(data.worktrees)) {
    throw new Error('list --json: unexpected shape');
  }
  return {
    main: asWorktree(data.main, 'main'),
    worktreesDir: data.worktrees_dir,
    worktrees: data.worktrees.map((entry, i) => asWorktree(entry, `worktrees[${i}]`)),
  };
}

export interface Located {
  forest: Forest;
  info: WorktreeInfo;
  isMain: boolean;
}

/** The forest entry (main checkout or worktree) at `path`, if any. */
export function locate(forests: readonly Forest[], path: string): Located | undefined {
  for (const forest of forests) {
    if (forest.main.path === path) {
      return { forest, info: forest.main, isMain: true };
    }
    const info = forest.worktrees.find((w) => w.path === path);
    if (info) {
      return { forest, info, isMain: false };
    }
  }
  return undefined;
}

/**
 * Most recent first: by `lastOpened` when known, else by `created`; ties
 * keep the CLI's order (which is git's: creation order).
 */
export function orderByRecency<T extends { path: string }>(
  worktrees: readonly T[],
  lastOpened: (path: string) => number | undefined,
  created: (path: string) => number,
): T[] {
  const stamp = (w: T): number => lastOpened(w.path) ?? created(w.path);
  return [...worktrees].sort((a, b) => stamp(b) - stamp(a));
}

export interface BranchCandidate {
  name: string;
  location: string; // "local, origin", "origin", ...
}

/** Parse `workforest --complete branches`: `NAME<TAB>LOCATION` lines. */
export function parseCandidates(text: string): BranchCandidate[] {
  return text
    .split('\n')
    .filter((line) => line.length > 0)
    .map((line) => {
      const tab = line.indexOf('\t');
      return tab < 0
        ? { name: line, location: '' }
        : { name: line.slice(0, tab), location: line.slice(tab + 1) };
    });
}

export type ScriptKind = 'command' | 'bulk' | 'pipeline';

export interface ScriptInfo {
  name: string;
  kind: ScriptKind;
  detail: string; // the command, or the members of a group
  background: boolean;
  exclusive: boolean;
}

/** The `scripts` map of `workforest config --json`, sorted by name; `hidden` entries are left out. */
export function parseScripts(configJson: string): ScriptInfo[] {
  const data: unknown = JSON.parse(configJson);
  if (!isRecord(data) || !isRecord(data.config)) {
    throw new Error('config --json: unexpected shape');
  }
  const scripts = data.config.scripts;
  if (!isRecord(scripts)) {
    return [];
  }
  return Object.keys(scripts)
    .sort()
    .filter((name) => !isHidden(scripts[name]))
    .map((name) => scriptInfo(name, scripts[name]));
}

function isHidden(entry: unknown): boolean {
  return isRecord(entry) && entry.hidden === true;
}

function scriptInfo(name: string, entry: unknown): ScriptInfo {
  if (typeof entry === 'string') {
    return { name, kind: 'command', detail: entry, background: false, exclusive: false };
  }
  if (!isRecord(entry)) {
    throw new Error(`scripts.${name}: unexpected shape`);
  }
  const flags = { background: entry.background === true, exclusive: entry.exclusive === true };
  if (Array.isArray(entry.bulk)) {
    return { name, kind: 'bulk', detail: `bulk: ${entry.bulk.join(', ')}`, ...flags };
  }
  if (Array.isArray(entry.pipeline)) {
    return { name, kind: 'pipeline', detail: `pipeline: ${entry.pipeline.join(' → ')}`, ...flags };
  }
  return { name, kind: 'command', detail: String(entry.command ?? ''), ...flags };
}

/** The flags worth showing next to a script's name in a picker. */
export function scriptDescription(script: ScriptInfo): string {
  const marks = [];
  if (script.background) {
    marks.push('background');
  }
  if (script.exclusive) {
    marks.push('exclusive');
  }
  return marks.join(', ');
}

/** How many instances of a script run in the worktree this window is in,
 * how many in the other worktrees, and how many of those there are. */
export interface RunningState {
  here: number;
  others: number;
  otherWorktrees: number;
}

export function runningState(forest: Forest, script: string, herePath: string | undefined): RunningState {
  let here = 0;
  let others = 0;
  let otherWorktrees = 0;
  for (const info of [forest.main, ...forest.worktrees]) {
    const count = info.running[script] ?? 0;
    if (count === 0) {
      continue;
    }
    if (info.path === herePath) {
      here += count;
    } else {
      others += count;
      otherWorktrees += 1;
    }
  }
  return { here, others, otherWorktrees };
}

/** What a running script's row says next to its name: every instance
 * counted, here and elsewhere, however few. The row's coloured icon says
 * the same thing again, but a colour alone cannot be counted. */
export function runningNote(state: RunningState): string {
  const parts = [];
  if (state.here > 0) {
    parts.push(`${state.here} here`);
  }
  if (state.others > 0) {
    parts.push(`${state.others} elsewhere`);
  }
  return parts.join(', ');
}

/** The same in words, for a row's description and a tooltip. */
export function runningLabel(state: RunningState): string {
  const { here, others, otherWorktrees } = state;
  if (here > 0) {
    const mine = here > 1 ? `${here} running here` : 'running here';
    return others > 0 ? `${mine}, ${others} elsewhere` : mine;
  }
  if (others === 0) {
    return '';
  }
  const where = otherWorktrees > 1 ? `${otherWorktrees} worktrees` : 'another worktree';
  return others > 1 ? `${others} running in ${where}` : `running in ${where}`;
}

/**
 * The message for a failed `workforest` call: its last stderr line without
 * the `Error: ` prefix cli.py adds, or `fallback` when it said nothing.
 */
export function failureMessage(stderr: string, fallback: string): string {
  const lines = stderr
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
  const last = lines[lines.length - 1];
  if (last === undefined) {
    return fallback;
  }
  return last.replace(/^Error:\s*/, '');
}

/** The stable phrase cli.py prints when the directory is outside any repository. */
export function isNotARepo(stderr: string): boolean {
  return stderr.includes('Not inside a git repository');
}

/** The worktree name `wf create BRANCH` will use: the last path component. */
export function worktreeNameFor(branch: string): string {
  const parts = branch.split('/');
  return parts[parts.length - 1] ?? branch;
}

/** Shell-quote a word for a POSIX shell (integrated-terminal commands). */
export function shellQuote(word: string): string {
  if (/^[A-Za-z0-9_@%+=:,./-]+$/.test(word)) {
    return word;
  }
  return `'${word.replace(/'/g, `'\\''`)}'`;
}
