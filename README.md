# Workforest

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
- **Open** it in your editor — in the current shell, or in a new terminal
  window via a configurable command template.
- **Run** named project scripts with well-known `WF_*` environment variables.
- **Delete** worktrees safely, or **checkout**: collapse one back into the
  main checkout.
- Drive everything from an interactive fzf **TUI** (`wf` with no arguments).

## Install

```sh
# Arch Linux
yay -S workforest        # AUR

# anywhere else
uv tool install workforest   # or: pipx install workforest
```

Then add one line to your `~/.bashrc` / `~/.zshrc`:

```sh
eval "$(workforest shell-init)"
```

This defines the `wf` function (needed so `wf open` can change your shell's
directory) and registers completions.

Requirements: Linux, git ≥ 2.36, Python ≥ 3.14. Optional: `fzf` for the TUI.

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

Scalars and lists replace; the `scripts`/`openers` mappings merge per key
(`null` removes an entry). `workforest config` shows the merged result and
where each layer came from; `workforest init` scaffolds a project file
(`--local` for a personal one).

All keys, with defaults:

```yaml
worktrees_dir: "$WF_MAIN/../worktrees/$WF_NAME"  # where the forest lives
opener: ""              # default opener; "" → $VISUAL → $EDITOR
openers: {}             # name -> command template, e.g. edit: "$EDITOR {path}"
window_command: ""      # "" → current shell; or e.g.
                        # "kitty --title {title} --directory {path} {command}"
symlinks: []            # untracked assets linked from main into new worktrees
setup_scripts: []       # shell snippets run in a fresh worktree
scripts: {}             # name -> snippet for `wf run NAME`
```

Openers are command templates: `{path}` is replaced with the target path; a
template without `{path}` just runs with the worktree as working directory.
Environment variables expand in both openers and `window_command`.

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

The same `$WF_*` variables work inside `worktrees_dir`.

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
workforest create [BRANCH] [-o OPENER] [-p PATH] [--no-hooks] [--no-open]
workforest open   [NAME]   [-o OPENER] [-p PATH]
workforest list   [--porcelain]
workforest delete NAME...  [--force] [--delete-branch | --keep-branch]
workforest checkout NAME   [--force]
workforest run    SCRIPT [ARGS...]
workforest tui    [MODE]
workforest init   [--local]
workforest config [--json]
workforest shell-init [bash|zsh]
```

Exit codes: `0` ok · `1` error · `2` usage · `3` cancelled · `4` config error.
Human messages go to stderr; stdout carries only machine output (`cd`
directives for the `wf` wrapper, `--porcelain` listings, dumps).

## Migrating from the bash MVP

- `.vscode/worktrees.json` → `.workforest.yaml` (same keys: `symlinks`,
  `setup_scripts`, `scripts`).
- Script env vars renamed: `ROOT_TREE_PATH` → `WF_MAIN`, `WORK_TREE_PATH` →
  `WF_WORKTREE`, `WORKTREES_DIR` → `WF_WORKTREES_DIR` (no aliases).
- The default layout gained a per-repo level: set
  `worktrees_dir: "$WF_MAIN/../worktrees"` to keep an existing flat forest.
- Kitty windows are one line of user config now:
  `window_command: "kitty --title {title} --directory {path} {command}"`.

## Development

```sh
uv sync           # venv + dev dependencies (uv.lock)
make check        # ruff + mypy --strict + pytest (coverage gate ≥ 90%)
make install      # install this checkout as a uv tool (~/.local/bin/workforest)
make uninstall    # remove it again
```

Design docs live in `.design_docs/`. Packaging recipes live under
`packaging/` (one directory per package manager; currently `packaging/AUR/`).
Release: bump `__version__`, tag, update `sha256sums` in
`packaging/AUR/PKGBUILD`, regenerate `.SRCINFO`
(`makepkg --printsrcinfo > .SRCINFO` inside `packaging/AUR/`), push to AUR.

## License

MIT
