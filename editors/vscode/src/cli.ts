/**
 * The one way the extension talks to workforest: find the executable, spawn
 * it, capture its streams, and classify how it ended. Nothing else spawns
 * processes (the integrated terminal runs `workforest run` for the user,
 * but that is the user's shell, not ours). Nothing here imports `vscode`
 * either, so the pure parts are unit-tested with node:test.
 */

import { execFile } from 'node:child_process';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';

import { failureMessage } from './forest';

export interface CliResult {
  code: number;
  stdout: string;
  stderr: string;
}

/** The executable could not be started at all (not installed, not on PATH). */
export class CliMissingError extends Error {
  constructor(readonly executable: string) {
    super(`'${executable}' was not found`);
    this.name = 'CliMissingError';
  }
}

/** workforest ran and reported an error (non-zero exit). */
export class CliError extends Error {
  constructor(
    message: string,
    readonly code: number,
    readonly stderr: string,
  ) {
    super(message);
    this.name = 'CliError';
  }
}

/** `~/x` → `$HOME/x`; anything else unchanged. */
export function expandHome(executable: string, home: string = os.homedir()): string {
  if (executable === '~') {
    return home;
  }
  if (executable.startsWith('~/')) {
    return path.join(home, executable.slice(2));
  }
  return executable;
}

/**
 * Where `uv tool`, pipx, and Homebrew put it. Searched after PATH, which a
 * GUI-launched VS Code may have inherited without the user's shell rc.
 */
const KNOWN_DIRS = ['~/.local/bin', '/opt/homebrew/bin', '/usr/local/bin', '/usr/bin'];

/**
 * Where to look for the command, best first: an explicit setting wins,
 * then PATH, then the usual install locations, and last the copy bundled
 * in the extension. Pure — the caller decides which of these exists.
 */
export function executableCandidates(options: {
  setting: string;
  path: string | undefined;
  home: string;
  bundled: string | undefined;
}): string[] {
  const setting = options.setting.trim();
  if (setting) {
    return [expandHome(setting, options.home)];
  }
  const dirs = [
    ...(options.path ?? '').split(path.delimiter),
    ...KNOWN_DIRS.map((dir) => expandHome(dir, options.home)),
  ];
  const candidates = dirs.filter((dir) => dir !== '').map((dir) => path.join(dir, 'workforest'));
  if (options.bundled !== undefined) {
    candidates.push(options.bundled);
  }
  return candidates;
}

function isRunnable(candidate: string): boolean {
  try {
    fs.accessSync(candidate, fs.constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

/**
 * The `workforest` this .vsix ships, if any: the platform-specific builds
 * carry one, the universal one does not. Unpacking a .vsix can drop the
 * executable bit, so restore it once, here.
 */
export function bundledExecutable(extensionPath: string): string | undefined {
  const candidate = path.join(extensionPath, 'bin', 'workforest');
  if (!fs.existsSync(candidate)) {
    return undefined;
  }
  if (!isRunnable(candidate)) {
    try {
      fs.chmodSync(candidate, 0o755);
    } catch {
      return undefined;
    }
  }
  return candidate;
}

export class Cli {
  constructor(
    private readonly executableSetting: () => string,
    private readonly log: (line: string) => void,
    private readonly bundled: string | undefined = undefined,
  ) {}

  /** The command to spawn: the first candidate that is there and runnable. */
  get executable(): string {
    const candidates = executableCandidates({
      setting: this.executableSetting(),
      path: process.env.PATH,
      home: os.homedir(),
      bundled: this.bundled,
    });
    const found = candidates.find(isRunnable);
    if (found !== undefined) {
      return found;
    }
    // Nothing runnable: spawn what the user configured, else the bare name,
    // so the ENOENT -> CliMissingError names the command they expect.
    return expandHome(this.executableSetting().trim() || 'workforest');
  }

  /**
   * Run workforest and resolve with its result whatever the exit code.
   * Rejects with CliMissingError only when the binary cannot be spawned.
   */
  run(args: string[], cwd: string): Promise<CliResult> {
    const executable = this.executable;
    this.log(`$ ${executable} ${args.join(' ')}  (in ${cwd})`);
    return new Promise((resolve, reject) => {
      execFile(
        executable,
        args,
        {
          cwd,
          env: { ...process.env, NO_COLOR: '1' },
          maxBuffer: 16 * 1024 * 1024,
        },
        (error, stdout, stderr) => {
          if (stderr.trim()) {
            this.log(stderr.trimEnd());
          }
          if (!error) {
            resolve({ code: 0, stdout, stderr });
          } else if (typeof error.code === 'number') {
            resolve({ code: error.code, stdout, stderr });
          } else if (error.code === 'ENOENT' || error.code === 'EACCES') {
            reject(new CliMissingError(executable));
          } else {
            reject(error);
          }
        },
      );
    });
  }

  /** Run and return stdout; a non-zero exit becomes a CliError. */
  async expect(args: string[], cwd: string, what: string): Promise<string> {
    const result = await this.run(args, cwd);
    if (result.code !== 0) {
      throw new CliError(failureMessage(result.stderr, `${what} failed`), result.code, result.stderr);
    }
    return result.stdout;
  }
}
