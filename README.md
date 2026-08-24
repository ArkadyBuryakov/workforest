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

This upgrades `wf` to a shell function (needed so `wf open` can change your
shell's directory — a plain binary cannot) and registers completions. Without
it everything still works, but "open in current shell" prints the `cd`
command instead of performing it.

Requirements: Linux or macOS, git ≥ 2.36, Python ≥ 3.14 (the AUR and
Homebrew packages bring their own). Optional: `fzf` for the TUI.

## Quick start

```sh
wf create feature/login     # create worktree + run hooks + open in $EDITOR
wf list                     # what's in the forest
wf open login -o 'lazygit'  # open with any command instead
wf run test                 # run a named script from config
wf run make check -j2       # extra args are appended to the script command
wf checkout login           # fold the branch back into the main checkout
wf delete fix-y             # remove a worktree (asks about dirty changes)
wf                          # interactive TUI (fzf)
```

Any unknown first word is an opener shortcut: `wf edit api` ≡
`wf open api -o edit`.

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
merged result and where each layer came from; `workforest init` scaffolds a
project file (`--local` for a personal one). Nothing in the environment
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
scripts: {}             # name -> snippet for `wf run NAME`
```

### Openers

An opener is a shell command that runs with the worktree root as working
directory, either **in your terminal** (the default: the `wf` wrapper does
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
  migrate: npm run db:migrate
```

## Commands

```
workforest create [BRANCH] [-o OPENER] [-w WRAPPER] [-p PATH] [--no-hooks] [--no-open]
workforest open   [NAME]   [-o OPENER] [-w WRAPPER] [-p PATH]
workforest list   [--porcelain]
workforest delete NAME...  [--force] [--delete-branch | --keep-branch]
workforest checkout NAME   [--force]
workforest run    SCRIPT [ARGS...]
workforest tui    [MODE]
workforest init   [--local]
workforest config [--json]
workforest shell-init [bash|zsh]
workforest claude copy-session SESSION_ID   # experimental
```

`open` (and the opener shortcut, e.g. `wf edit`) without NAME opens the
worktree you are standing in.

`create` resolves BRANCH in order: existing local branch, then a branch on
exactly one remote (checked out tracking it), then a brand-new branch.
`REMOTE/BRANCH` picks the remote explicitly — needed when several remotes
carry the same branch name; if that local name is already taken, `create`
prompts for a different one.

`workforest claude` (shown only when `~/.claude` exists) copies a Claude
Code session from the main worktree into the current one. It is
**experimental**: it manipulates Claude Code's private on-disk state,
which is not a stable interface, so any Claude Code update may break it.

Exit codes: `0` ok · `1` error · `2` usage · `3` cancelled · `4` config error.
Human messages go to stderr; stdout carries only machine output (`cd`
directives for the `wf` wrapper, `--porcelain` listings, dumps).

## Development

```sh
uv sync           # venv + dev dependencies (uv.lock)
make check        # ruff + mypy --strict + pytest (coverage gate ≥ 90%)
make install      # install this checkout as a uv tool (~/.local/bin/workforest)
make uninstall    # remove it again
```

Packaging templates live under `packaging/` (one directory per package
manager: `packaging/AUR/`, `packaging/homebrew/`); the `@VERSION@` and
`@SHA256@` placeholders are filled in at release time.
Release: bump `__version__` and push to main — CI tags the release,
renders the templates, and publishes to PyPI, the AUR, and the
[Homebrew tap](https://github.com/ArkadyBuryakov/homebrew-tap). The
published AUR package and tap are the only places rendered recipes exist.

## License

[MIT](./LICENSE)
