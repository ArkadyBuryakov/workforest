# Architecture invariants

- `gitutil.py` is the only module that spawns git. Consumers get typed
  results; worktree data comes from `--porcelain -z` output, never from
  parsing the human-readable form.
- `cli.py` is the sole stdout writer. Commands return
  `ShellAction | str | None`; stdout carries the shell-wrapper cd protocol,
  so nothing else may print there (hook/script stdout is diverted to stderr).
- `tui.py`: everything except the fzf subprocess is pure and unit-tested;
  fzf is the only sanctioned external tool there.
- `completions.py` must never break the shell: any error yields an empty
  candidate list, and output stays plain `NAME<TAB>ANNOTATION` lines.
- `integrations/claude.py` is experimental: it reads Claude Code's private
  on-disk state. Session lines are rewritten by JSON parsing, never by
  string substitution.
- The editor plugins ship the CLI they drive: `packaging/binary/build.sh`
  freezes the wheel into one self-contained executable (PyInstaller) per
  platform, which CI puts in `editors/vscode/bin/workforest` (one per
  platform-specific `.vsix`) and `editors/idea/bin/<os>-<arch>/workforest`
  (all of them in one plugin zip). Both trees build fine without it and
  both prefer an installed `workforest`: the bundled copy is the last
  candidate after the setting, `PATH`, and the usual install directories.
  Neither client may add a second way to obtain the CLI — no downloading,
  no installing on the user's behalf.
- `editors/vscode/` (the VS Code extension) is a thin client: it only ever
  spawns `workforest` — never git — and reads its machine output
  (`list --json`, `config --json`, `--complete` lines). Terminal prompts are
  replaced by VS Code dialogs plus `--force`/`--keep-branch`/`--delete-branch`.
  Parsing lives in `forest.ts`, which never imports `vscode` and is
  unit-tested with `node:test`; `cli.ts` is the only module that spawns.
  Verify with `npm run check` there (compile, tests, `vsce package`).
- README.md is the project reference; there is no separate design document.
  The man pages under `man/` (`workforest.1`, `workforest.5`) are its
  installed counterpart: any change to commands, options, config keys,
  environment variables, or exit codes updates both README.md and the
  affected page in the same change. `tests/test_man.py` catches drift from
  `cli.py`, not from prose — keep the wording in sync by hand.
- Script groups (`hooks.py`): a `bulk`/`pipeline` runs under a forked
  supervisor that leads the process group and takes the tty exactly like a
  command, so records, `wf stop`, `exclusive`, `cleanup`, and `-b` need no
  group-specific code. Bulk members must never take the tty (only one
  group can own it); they run in the supervisor's process group so a
  signal to the group reaches them all, write to a pty when stderr is a
  terminal (so their programs keep colors) and to a pipe otherwise, and
  the supervisor relays their lines prefixed. A bulk always waits for
  every member. An outcome of `-SIGINT` or exit 130 is an interruption
  (warning, `wf` dies by SIGINT), never a failure. Forked-child bodies are
  `# pragma: no cover`; the logic lives in tested pure helpers
  (`_Prefixer`, `_pump`, `_bulk_outcome`, `_run_step`).
- `editors/idea/` (the JetBrains plugin) is a thin client like the VS Code
  one: it only ever spawns `workforest` — never git — and reads its machine
  output (`list --json`, `--complete` lines, the last stderr line as the
  error). Terminal prompts are replaced by IDE dialogs plus
  `--force`/`--keep-branch`/`--delete-branch`. Parsing lives in
  `Protocol.kt`, which is pure and unit-tested; `WorkforestCli.kt` is the
  only file that spawns. `terminal/` is loaded only with the bundled
  Terminal plugin (optional dependency). Verify with `./gradlew build
  buildPlugin` there (JDK 21).
- Logo assets are generated, never hand-edited: `assets/src/logo.svg`
  (full size) and `assets/src/logo-icon.svg` (adapted for small formats,
  the source of every icon) are the only files touched by hand.
  `assets/generate` writes `assets/logo*.svg` plus the icons vendored into
  `editors/vscode/media/` and `editors/idea/.../resources/`; each carries a
  "do not edit" header. Adding a place that ships the logo means adding a
  target there, not another copy. `.github/workflows/assets.yml` re-runs the
  script on pull requests and commits any difference.
