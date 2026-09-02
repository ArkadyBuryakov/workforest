/**
 * The one way the extension talks to workforest: spawn the configured
 * executable, capture its streams, and classify how it ended. Nothing else
 * spawns processes (the integrated terminal runs `workforest run` for the
 * user, but that is the user's shell, not ours).
 */

import { execFile } from 'node:child_process';
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

export class Cli {
  constructor(
    private readonly executableSetting: () => string,
    private readonly log: (line: string) => void,
  ) {}

  get executable(): string {
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
