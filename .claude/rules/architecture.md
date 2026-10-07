# Architecture invariants

- The CLI is one Rust crate at the repository root: a library (`src/lib.rs`)
  with two thin binaries — `workforest` (`src/main.rs`) and `wf`
  (`src/bin/wf.rs`), which only runs the `workforest` beside it, for
  installs that cannot make it a symlink. The version is `Cargo.toml`'s and
  nothing else's: `packaging/version` prints it for the Makefile and the
  workflows.
- `git.rs` is the only module that spawns git. Consumers get typed
  results; worktree data comes from `--porcelain -z` output, never from
  parsing the human-readable form.
- `cli.rs` is the sole stdout writer. Commands return an `Outcome`
  (`Shell` | `Text` | `Nothing`); stdout carries the shell-wrapper cd
  protocol, so nothing else may print there (hook/script stdout is diverted
  to stderr). Everything human-facing goes through `output.rs`, to stderr.
- Messages and machine output are part of the interface and reproduce the
  0.8.0 Python CLI byte for byte: quoting is Python's `repr` (`util::repr`),
  shell words are `shlex.quote` (`util::shell_quote`), JSON is ASCII-escaped
  (`util::json_pretty`), and `wf config` is what PyYAML's `safe_dump` wrote
  (`config/dump.rs`). Change one deliberately, never as a side effect.
- The environment is data: decisions that depend on it (`$SHELL`,
  `$EDITOR`, what a script inherits) take a `util::Env`, captured once in
  `commands::build_context`, instead of reading the process environment —
  which is also what keeps unit tests from touching it.
- `config/`: files are YAML 1.1 as PyYAML read them (`yes` is a boolean,
  `~` is null; `value.rs` resolves scalars, `yaml.rs` reads through
  `saphyr-parser`), so a config means the same as it always did. Unknown
  keys warn and are dropped, never fail. Writing a file goes through
  `edit.rs` only: a lossless syntax tree (`yaml-edit`), so comments,
  ordering, formatting and unknown keys survive, and an edit is accepted
  only if the result reads back as what was asked for. No command exposes
  it yet (it exists for config editing from the editor plugins); never
  write config by parse-and-dump.
- `tui/` draws the interactive mode itself with ratatui, on the alternate
  screen and on stderr (stdout is the shell wrapper's) — no external
  program. `app.rs` (state, keys) and `view.rs` (drawing) are pure and
  unit-tested; `term.rs`, the loop that connects them to a terminal, is
  the only part that needs one.
- `completions.rs` must never break the shell: any error yields an empty
  candidate list, and output stays plain `NAME<TAB>ANNOTATION` lines.
- `integrations/claude.rs` is experimental: it reads Claude Code's private
  on-disk state. Session lines are rewritten by JSON parsing, never by
  string substitution.
- The editor plugins ship the CLI they drive: `packaging/binary/build.sh`
  makes a release build of the crate — one executable, its resources
  compiled in — per platform, which `make vscode-build`/`make idea-build`
  (this machine's platform) and CI (all four) put in
  `editors/vscode/bin/workforest` (one per
  platform-specific `.vsix`) and `editors/idea/bin/<os>-<arch>/workforest`
  (all of them in one plugin zip). Both trees build fine without it, and
  both run the bundled copy when there is one — it was built with the
  client, so the two always match; only where the package carries none do
  they fall back to `PATH` and the usual install directories. Neither
  client offers a setting for the path, and neither may add a second way to
  obtain the CLI — no downloading, no installing on the user's behalf.
- `editors/vscode/` (the VS Code extension) is a thin client: it only ever
  spawns `workforest` — never git — and reads its machine output
  (`list --json`, `config --json`, `--complete` lines). Terminal prompts are
  replaced by VS Code dialogs plus `--force`/`--keep-branch`/`--delete-branch`.
  Parsing lives in `forest.ts`, which never imports `vscode` and is
  unit-tested with `node:test`; `cli.ts` is the only module that spawns.
  Verify with `npm run check` there (compile, tests, `vsce package`).
  It ships to two registries under two identities —
  `ArkadyBuryakov.workforest-vscode` on the Visual Studio Marketplace, which
  refuses a name another publisher already holds, and
  `ArkadyBuryakov.workforest` on Open VSX, which does not. Only the manifest
  differs: `publish_openvsx.yml` stamps the Open VSX pair on before
  packaging, so nothing in `src/` may hardcode the id — ask VS Code for it
  (`context.extension.id`).
- README.md is the project reference; there is no separate design document.
  The man pages (`workforest.1`, `workforest.5`) are its installed
  counterpart: any change to commands, options, config keys, environment
  variables, or exit codes updates both README.md and the affected page in
  the same change. `tests/man.rs` walks the clap definition and catches
  drift from `cli.rs`, not from prose — keep the wording in sync by hand.
  The pages live under `packaging/pypi/data/share/man/`, the layout the
  PyPI wheel ships data files from; the AUR and Homebrew recipes install
  them from there too.
- Distribution: PyPI is a thin wheel around the executables (maturin,
  `bindings = "bin"`, no Python code; one wheel per platform the plugins
  bundle, the sdist for the rest); the AUR package and the Homebrew
  formula build the crate and install the binary with `wf` as a symlink.
  What ships beside the binary — shell completion, example configs — is
  under `resources/`, which is also what the binary embeds.
- Script groups (`hooks/`): a `bulk`/`pipeline` runs under a forked
  supervisor that leads the process group and takes the tty exactly like a
  command, so records, `wf stop`, `exclusive`, `cleanup`, and `-b` need no
  group-specific code. Bulk members must never take the tty (only one
  group can own it); they run in the supervisor's process group so a
  signal to the group reaches them all, write to a pty when stderr is a
  terminal (so their programs keep colors) and to a pipe otherwise, and
  the supervisor relays their lines prefixed. A bulk always waits for
  every member. An outcome of `-SIGINT` or exit 130 is an interruption
  (warning, `wf` dies by SIGINT), never a failure. The process machinery —
  fork, process groups, the terminal, signal handlers that only set flags
  or forward — is `hooks/run.rs`; what is decided from plain data
  (`Prefixer`, `bulk_outcome`, `failure`, `stop_timeout`) is `hooks/mod.rs`
  and unit-tested. A forked child never returns into its parent's code:
  every supervisor body ends in `exit_with`. Forking happens only while
  the process is single-threaded.
- `editors/idea/` (the JetBrains plugin) is a thin client like the VS Code
  one: it only ever spawns `workforest` — never git — and reads its machine
  output (`list --json`, `--complete` lines, the last stderr line as the
  error). Terminal prompts are replaced by IDE dialogs plus
  `--force`/`--keep-branch`/`--delete-branch`. Parsing lives in
  `Protocol.kt`, which is pure and unit-tested; `WorkforestCli.kt` is the
  only file that spawns. `terminal/` is loaded only with the bundled
  Terminal plugin (optional dependency). Verify with `./gradlew build
  buildPlugin` there (JDK 21).
- The changelog is written once: `CHANGELOG.md` at the root is the only
  one edited by hand. Its versions are the CLI's — the clients ship it, so
  one entry covers all three, back to 0.1.0, which predates them. It is
  an end-user document, shipped verbatim in the .vsix: release notes
  only, nothing about versioning policy or how the copies are made.
  The clients' copies —
  `editors/vscode/CHANGELOG.md` (the source verbatim) and the
  `<change-notes>` block of the JetBrains `plugin.xml` (HTML, the most
  recent releases only) — are placeholders in the repository exactly like
  the version, stamped by `packaging/changelog/generate` at package time
  and never committed: releasing is one commit to `CHANGELOG.md`. The
  `make` targets put the placeholder back around a build; the publish
  workflows only stamp. `release.yml` takes a release's GitHub notes from
  the matching section. Adding a place that publishes a changelog means
  adding a target there, not another file to keep in step.
- Logo assets are generated, never hand-edited: `assets/src/logo.svg`
  (full size) and `assets/src/logo-icon.svg` (adapted for small formats,
  the source of every icon) are the only files touched by hand.
  `assets/generate` writes `assets/logo*.svg` plus the icons vendored into
  `editors/vscode/media/` and `editors/idea/.../resources/`; each carries a
  "do not edit" header. Adding a place that ships the logo means adding a
  target there, not another copy. `.github/workflows/assets.yml` re-runs the
  script on pull requests and commits any difference.
