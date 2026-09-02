<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/ArkadyBuryakov/workforest/HEAD/assets/logo-dark.svg">
    <img src="https://raw.githubusercontent.com/ArkadyBuryakov/workforest/HEAD/assets/logo-light.svg" alt="Workforest logo" width="160">
  </picture>
</p>

# Workforest

## Elevator pitch

Your repo has one working directory; your AI agents want five. Workforest
gives every feature — and every agent — its own disposable git worktree, so
parallel work on the same repo never collides.

## About

Git worktree forest management: one main checkout plus any number of
disposable, per-branch worktrees in a predictable location — created, set up,
opened, and cleaned up with one command.

```
~/dev/
├── api/                  # main checkout
└── worktrees/
    └── api/
        ├── feature-x/    # wf create feature-x
        └── fix-y/
```

- **Create** a worktree for any branch (local, remote, or brand new) and have
  it set up automatically: symlinks for untracked assets (`node_modules`,
  `.env`, …) and project-defined setup scripts.
- **Open** it in your editor — in your terminal, in a new terminal window or
  multiplexer pane, or in a GUI app, all from a small shell-command config.
- **Run** named project scripts with well-known `WF_*` environment variables.
- **Delete** worktrees safely, or **checkout**: collapse one back into the
  main checkout.
- Drive everything from an interactive fzf **TUI** (`wf` with no arguments).

## Install

```sh
# Arch Linux
yay -S workforest        # AUR

# macOS (or Linux with Homebrew)
brew install arkadyburyakov/tap/workforest

# anywhere else
uv tool install workforest   # or: pipx install workforest
```

This installs two commands: `workforest` and its alias `wf`. Then add one
line to your `~/.bashrc` / `~/.zshrc`:

```sh
eval "$(workforest shell-init)"
```

This upgrades both `workforest` and `wf` to one shell function (needed so
`open` can change your shell's directory — a plain binary cannot; either
spelling, or an alias of either, behaves the same), registers completions,
and — for `uv tool`/`pipx` installs, whose files live in a venv — puts the
man pages on `$MANPATH`. Without it everything still works, but "open in
current shell" prints the `cd` command instead of performing it.

Reference: `man workforest` (commands) and `man 5 workforest` (the
configuration files); `wf` works in place of `workforest` for both.

Requirements: Linux or macOS, git ≥ 2.36, Python ≥ 3.14 (the AUR and
Homebrew packages bring their own). Optional: `fzf` for the TUI.

For the editor integration alone there is nothing to install: the [VS Code
extension](#vs-code-extension) and the [JetBrains plugin](#jetbrains-ide-plugin)
bring their own binary.

## Quick start

```sh
wf create feature/login     # create worktree + run hooks + open in $EDITOR
wf list                     # what's in the forest
wf open login -o 'lazygit'  # open with any command instead
wf run test                 # run a named script from config
wf run -b backend           # detached; `wf stop backend` ends it
wf run make check -j2       # extra args are appended to the script command
wf checkout login           # fold the branch back into the main checkout
wf delete fix-y             # remove a worktree (asks about dirty changes)
wf                          # interactive TUI (fzf)
```

## Configuration

Layered, YAML or JSON; later layers override earlier ones:

| Layer | Location | Typical content |
|---|---|---|
| system | `/etc/workforest/config.yaml` | org-wide defaults |
| user | `~/.config/workforest/config.yaml` | your terminal/editor setup |
| project (shared) | `.workforest.yaml` in the repo root | repo policy, committed |
| project (local) | `.vscode/` or `.idea/` `.workforest.yaml` | personal overrides, untracked |

Scalars and lists replace; the `openers`/`wrappers`/`scripts` mappings
merge per key (`null` removes an entry). `workforest config` shows the
merged result and where each layer came from; `workforest init` writes a
short, fully commented starter (`--local` for a personal one) — inert until
you uncomment something. Nothing in the environment
changes the result — files and flags only.

All keys, with defaults:

```yaml
worktrees_dir: "$WF_MAIN/../worktrees/$WF_NAME"  # where the forest lives
opener: ""              # default opener: an `openers` name or a shell command;
                        #   "" → $VISUAL → $EDITOR
openers: {}             # name -> what `-o NAME` runs, and where
wrappers: {}            # name -> command that runs $WF_COMMAND elsewhere (window, pane, direnv)
symlinks: []            # untracked assets linked from main into new worktrees
setup_scripts: []       # shell snippets run in a fresh worktree
scripts: {}             # name -> command or group for `wf run NAME` (see Scripts)
stop_timeout: 30        # seconds a stopped script gets after SIGTERM before SIGKILL
```

### Openers

An opener is a shell command that runs with the worktree root as working
directory, either **in your terminal** (the default: the shell function does
`cd` there and runs it) or **in the background** (`background: true`:
spawned detached, `wf` returns immediately — for GUI apps and commands that
hand off to a daemon or multiplexer). Optionally it runs **through a
wrapper**, a command that receives it as `$WF_COMMAND` and takes it
somewhere else: a new terminal window, a tmux window, a `direnv exec`. With
no config at all, `wf` runs `$VISUAL`/`$EDITOR` in your terminal.

```yaml
opener: win                 # what `wf create` / `wf open` run by default

wrappers:                   # get the opener command as $WF_COMMAND
  kitty:
    command: kitty --title "$WF_TITLE" --directory "$WF_WORKTREE" $SHELL -c "$WF_COMMAND"
    background: true
  tmux:                     # the tmux server has its own environment: WF_ENV re-creates ours
    command: tmux new-window -n "$WF_TITLE" -c "$WF_WORKTREE" "export $WF_ENV; $WF_COMMAND"
    background: true

openers:
  edit: $EDITOR "$WF_TARGET"        # in your terminal
  code:                             # GUI app: detached
    command: code "$WF_WORKTREE"
    background: true
  kitty:                              # edit's command, in a new kitty window
    from: edit
    wrap: kitty
  tmux:                              # edit's command, in a new tmux window
    from: edit
    wrap: tmux
  git: lazygit
```

An entry is a shell command string, or a mapping with either `command` (a
shell command, never a name) or `from` (another opener's command — one
level: the target has a `command` of its own — inheriting its `background`
unless the entry sets one), plus optionally `background: true` or
`wrap: NAME`. `wrap` and `background` never sit on the same entry: the
wrapper decides where the whole thing runs, via its own `background` flag.
`-o VALUE` (and `opener:`) is an `openers` name, else a shell command;
`-w NAME` on `create`/`open` overrides the opener's wrapper (`-w ''` for
none). Wrappers are environment-specific, so they belong in the per-machine
user config — a host without your terminal emulator simply has none.
`from` and `wrap` names are checked when the config loads, so `wf config`
reports a misspelled one.

All of them are plain shell commands, run via `$SHELL -c` with one variable
family in the environment — the same family the launched process and every
script receive:

| Variable | Value |
|---|---|
| `WF_MAIN` | main worktree path, `/home/user/Projects/project_name` |
| `WF_NAME` | repo name, `project_name` |
| `WF_WORKTREES_DIR` | resolved worktrees directory |
| `WF_WORKTREE` | this worktree's path |
| `WF_BRANCH` | its branch (empty if detached) |
| `WF_TARGET` | the `-p` argument, default `.` (launch-only) |
| `WF_TITLE` | window label, `project_name: feat-x` (launch-only) |
| `WF_ENV` | all of the above as shell-quoted `NAME=value` assignments (launch-only) |
| `WF_COMMAND` | in a wrapper: the opener command, unexpanded |

Standard shell rules apply — there is no workforest template syntax:
`"$WF_X"` is exactly one argument, bare `$WF_X` word-splits, and `$$`,
braces, pipes, and `&&` mean whatever your shell says they mean (a
misspelled `$WF_VAR` expands to empty, as in any shell). `$WF_COMMAND`
reaches the wrapper unexpanded, so run it through a shell of its own for its
`$WF_*` references to resolve: `$SHELL -c "$WF_COMMAND"`. When that shell
runs somewhere this environment is not inherited — a tmux server, an ssh
host — hand the family over as text: `"export $WF_ENV; $WF_COMMAND"` is
re-parsed on the far side, quoting intact. Background
processes shed activation state inherited from the invoking shell (Python
venv, conda, nvm, rvm) so a new window starts clean instead of carrying an
environment it cannot deactivate.

Fully commented reference configs:
[`config.yaml`](src/workforest/examples/config.yaml) (user/system) and
[`.workforest.yaml`](src/workforest/examples/.workforest.yaml) (project) —
installed to `/usr/share/doc/workforest/examples/` by the Arch package.

### Scripts

`wf run NAME [ARGS...]` runs a `scripts` entry from the root of the current
worktree, with any extra arguments shell-quoted and appended. An entry is a
shell command string, a mapping, or a group of other entries (see below):

```yaml
scripts:
  test: npm test                  # shorthand for {command: npm test}
  frontend:
    command: cd frontend && npm run dev
    exclusive: true               # at most one instance per project: it owns the port
  backend:
    command: docker compose up
    exclusive: true
    background: true              # detached, output in a log file
    cleanup: docker compose down  # runs after the command ends, however it ended
    stop_timeout: 60              # give it a minute to come down before SIGKILL
```

The command runs in a process group of its own, in the foreground of your
terminal, so Ctrl-C and Ctrl-Z reach it directly; a signal sent to `wf`
itself (SIGINT, SIGTERM, SIGHUP) is forwarded to the whole group. `cleanup`
then runs — after a normal exit, a failure, or a kill — in the same
worktree with the same `WF_*` variables. `wf run` fails with the command's
status, or `128+N` when the command was killed by signal N. A command
killed by SIGINT — or exiting 130, as a shell does once its child was — is
reported as interrupted, a warning rather than an error, and ends `wf` the
way Ctrl-C would, so a shell loop around it aborts.

A `background` script (or `wf run -b NAME`) is detached instead: it runs
under a supervisor of its own — the same process-group and cleanup
handling, minus the terminal — with its output in a log file under
`.git/workforest/logs/NAME/` of the main checkout, and `wf` returns as
soon as it is clearly running (a command that dies right away is reported
with the tail of its log; the path is printed when it starts). That way
several long-running scripts can be started from one terminal. Any number
of instances of a script may run at once, in one worktree or across
several: each keeps a record and — in the background — a log of its own,
named after the worktree and the pid of the `wf run` that owns it
(`WORKTREE.PID.log`), and each runs its own `cleanup` when it ends, so a
`cleanup` has to tolerate a sibling instance still running. `wf stop
NAME` stops every instance in this worktree, foreground or background,
started from any terminal; `--all` stops every worktree's. Logs of
instances that are no longer running are removed the next time the script
starts there. Use `exclusive` for a script that must not run twice.

An `exclusive` script runs at most once per project: starting it stops
every running instance in any worktree (this one included), waits for
their cleanup to finish, and only then starts. Stopping — by `wf stop` or
by preemption — is SIGTERM, then SIGKILL after `stop_timeout`
seconds (30 by default; the top-level key changes it for every script, an
entry's own `stop_timeout` for that one). The stopped instance's `wf run`
reports ``killed by SIGTERM (stopped by `wf run backend` in 'feat-x')``
and exits 143. Running instances are recorded under
`.git/workforest/running/` in the main checkout. A record is a hint, not
the truth: pid, process group, and boot are verified before anything is
signalled, so a crash, a reboot, or a `kill -9`'d `wf` leaves nothing to
tidy by hand. A command still running after its `wf run` is gone (an
orphan) is stopped and cleaned up by whoever stops it when `/proc`
confirms the pid was not recycled; elsewhere it is reported and left
alone.

#### Groups

A `bulk` entry runs other entries at once; a `pipeline` entry runs them one
after another. Members are named, not written inline, so each keeps its own
`exclusive`, `cleanup`, and `stop_timeout`, and a group may name another
group (cycles are a config error):

```yaml
scripts:
  migrate:
    command: npm run db:migrate
    hidden: true                  # a group member only: left out of the lists
  dev:
    bulk: [backend, frontend]     # both at once; done when both have ended
  fresh:
    pipeline: [migrate, dev]      # in order; the first failure ends it
    background: true
```

A group is a script like any other: `wf run -b`, `background`,
`exclusive`, and `cleanup` apply to it, and `wf stop GROUP` stops every
member, waits for all their cleanups, then runs the group's own. A group
takes no extra arguments; its `stop_timeout` defaults to the longest of
its members'. Members stay individually visible: `wf stop MEMBER` stops
that member out from under a running group, and a member that is already
running elsewhere is started again — `exclusive` is what makes a member
stop the running one instead. A member that is only ever meant to run as
part of a group can set `hidden: true`: it is then left out of shell
completion and the editor plugins' script lists, while `wf run` and `wf
stop` still take its name.

A pipeline's steps run like consecutive `wf run`s, each with the
terminal; a `background` member is started and left running while the
pipeline moves on. The pipeline ends with the first failing step's status.

A bulk relays its members' output line by line, each line prefixed with
the member's name:

```
backend  | Attaching to db-1, api-1
frontend | VITE v5.4  ready in 312 ms
backend  | api-1 | listening on :8000
```

Members cannot read the terminal, but Ctrl-C reaches them all. A bulk
waits for every member — so `bulk: [lint, test, typecheck]` shows every
failure, not just the first — and fails if any of them did.

### Script environment

`setup_scripts`, `scripts`, and hooks run via `$SHELL -c` with:

| Variable | Value |
|---|---|
| `WF_MAIN` | main worktree path |
| `WF_NAME` | repo name (main checkout directory name) |
| `WF_WORKTREE` | current/new worktree path |
| `WF_WORKTREES_DIR` | resolved worktrees directory |
| `WF_BRANCH` | branch of the current/new worktree |

`worktrees_dir` is a template using the same naming pattern: `$WF_MAIN` and
`$WF_NAME` (plus regular environment variables like `$HOME`) expand there —
the per-worktree variables don't, since no worktree exists yet when the base
directory is resolved.

### Example project config

```yaml
# .workforest.yaml — committed to the repo
symlinks: [node_modules, .env]
setup_scripts:
  - npm install --prefer-offline
scripts:
  test: npm test
  migrate:
    command: npm run db:migrate
    hidden: true
  frontend:
    command: cd frontend && npm run dev
    exclusive: true
  backend:
    command: docker compose up
    exclusive: true
    background: true
    cleanup: docker compose down
    stop_timeout: 60
  dev:
    bulk: [backend, frontend]
  fresh:
    pipeline: [migrate, dev]
```

## Commands

```
workforest create [BRANCH] [-o OPENER] [-w WRAPPER] [-p PATH] [--no-hooks] [--no-open]
workforest open   [NAME]   [-o OPENER] [-w WRAPPER] [-p PATH]
workforest list   [--porcelain | --json]
workforest delete NAME...  [--force] [--delete-branch | --keep-branch]
workforest checkout NAME   [--force]
workforest run    [-b] SCRIPT [ARGS...]
workforest stop   SCRIPT [--all]
workforest tui    [MODE]
workforest init   [--local]
workforest config [--json]
workforest shell-init [bash|zsh]
workforest claude copy-session SESSION_ID   # experimental
```

`open` without NAME opens the worktree you are standing in.

`create` resolves BRANCH in order: existing local branch, then a branch on
exactly one remote (checked out tracking it), then a brand-new branch.
`REMOTE/BRANCH` picks the remote explicitly — needed when several remotes
carry the same branch name; if that local name is already taken, `create`
prompts for a different one.

`workforest claude` (shown only when `~/.claude` exists) copies a Claude
Code session from the main worktree into the current one. It is
**experimental**: it manipulates Claude Code's private on-disk state,
which is not a stable interface, so any Claude Code update may break it.

Exit codes: `0` ok · `1` error · `2` usage · `3` cancelled · `4` config error
· `128+N` the `run` command was killed by signal N.
Human messages go to stderr; stdout carries only machine output (`cd`
directives for the shell function, `--porcelain`/`--json` listings, dumps).
`list --json` describes the whole forest for programs — `main` (the main
checkout, in the same `name`/`branch`/`path`/`dirty`/`running` shape as each
entry of `worktrees`, `running` being the names of the scripts running
there, each once however many instances of it run) and the resolved `worktrees_dir` — and is what the editor extensions
read.

## JetBrains IDE plugin

`editors/idea/` holds a plugin for IntelliJ IDEA, PyCharm, WebStorm, and
the other IntelliJ-based IDEs (2025.2 or later) that puts the forest in the
IDE: a **Workforest** tool window with the project's scripts (badged
where they are running) and the main checkout plus the worktrees (most
recently opened first, dirty markers, the one this window is in), with
tooltips, inline buttons, and context menus; commands to create, open,
delete, and checkout worktrees (the last two on this window's worktree
when nothing is selected),
run and stop `scripts` in the IDE terminal, open a terminal in a worktree,
show the merged configuration, and scaffold the project or the
`.idea/.workforest.yaml` local config; plus a status bar widget. It is a
thin client: every action runs the `workforest` command (`list --json`,
`config --json`, `--complete` lines, and the plain subcommands with
`--force` / `--keep-branch` / `--delete-branch` in place of terminal
prompts, which become IDE dialogs), so the IDE and your shell always
agree. Deleting or checking out the worktree open in the current window
opens the main checkout in its place — the IDE's version of `wf`'s `cd`
back.

Build and install the plugin from a checkout until it is on the Marketplace
(any JDK runs Gradle, even the IDE's bundled one; the JDK 21 it compiles
with is fetched automatically): `# TO DO`

```sh
cd editors/idea
./gradlew buildPlugin      # compile, unit tests → build/distributions/workforest-idea-*.zip
```

then *Settings | Plugins | ⚙ | Install Plugin from Disk* — or unzip it into
the IDE's plugins directory, which is all that dialog does and what `make
idea` (see Development) does from the repository root. `./gradlew runIde`
starts a sandboxed IDE with the plugin for development.
`editors/idea/README.md` is the plugin's own reference (features, settings,
the `.idea/` carry-over recipe, troubleshooting).

## VS Code extension

`editors/vscode/` holds a VS Code extension that puts the forest in the
editor: a **Workforest** sidebar with the JetBrains plugin's toolbar in
its header and two collapsible sections, Scripts (run/stop with one
click, badged where they are running) and Worktrees (main checkout, then
managed worktrees by recency, dirty markers, the worktree this window is
in), commands to create, open, delete, and checkout worktrees (the last
two on this window's worktree when invoked on no row), run and stop `scripts` in
the integrated terminal, show the merged configuration, and scaffold the
project or the `.vscode/.workforest.yaml` local config, plus a status bar
item. It is a thin client: every action runs the `workforest` command
(`list --json`, `config --json`, `--complete branches`, and the plain
subcommands with `--force`/`--keep-branch` in place of terminal prompts),
so the editor and your shell always agree.

Build and install the extension from a checkout until it is on the
Marketplace: `# TO DO`

```sh
cd editors/vscode
npm install
npm run check        # compile, unit tests, package → workforest-*.vsix
code --install-extension workforest-*.vsix
```

`make vscode` (see Development) does the same from the repository root.

`editors/vscode/README.md` is the extension's own reference (features,
settings, troubleshooting).

## Development

```sh
uv sync           # venv + dev dependencies (uv.lock)
make check        # ruff + mypy --strict + pytest (coverage gate ≥ 90%)
make install      # install this checkout as a uv tool (~/.local/bin/workforest)
make uninstall    # remove it again
make vscode       # build the VS Code extension and install it into VS Code
make idea         # build the JetBrains plugin and unpack it into the IDE
make plugins      # both (`-build`, `-install`, `-uninstall` targets exist too)
```

The two plugin targets need machine-specific paths: `IDEA_JAVA_HOME` (the JDK
Gradle runs on; empty means `java` from the `PATH`) and `IDEA_PLUGINS` (the
IDE's plugins directory, `~/.local/share/JetBrains/IdeaIC2025.2` by default).
Set them on the command line, or keep them in an untracked `Makefile.local`,
which the `Makefile` includes when it exists.

Man pages are hand-written roff under `man/` (`workforest.1` and
`workforest.5`, plus `wf.1`/`wf.5` links to them); `tests/test_man.py` fails when they drift from
`cli.py`. They ship as `share/man` data in the wheel, which is how every
package gets them: the Arch package installs the wheel to `/usr`, the
Homebrew formula moves them out of its venv, and `uv tool`/`pipx` keep
them in the venv where `workforest shell-init` points `$MANPATH`.

Packaging templates live under `packaging/` (one directory per package
manager: `packaging/AUR/`, `packaging/homebrew/`); the `@VERSION@` and
`@SHA256@` placeholders are filled in at release time.
`packaging/binary/` builds the self-contained `workforest` executable the
editor plugins ship; each editor's README says how to bundle it.
Release: bump `__version__` and push to main — CI tags the release,
renders the templates, and publishes to PyPI, the AUR, and the
[Homebrew tap](https://github.com/ArkadyBuryakov/homebrew-tap). The
published AUR package and tap are the only places rendered recipes exist.

## License

[MIT](./LICENSE)
