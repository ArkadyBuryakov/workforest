<p align="center">
  <img src="https://raw.githubusercontent.com/ArkadyBuryakov/workforest/HEAD/editors/vscode/media/icon.png" alt="Workforest logo" width="96">
</p>

# Workforest for VS Code

Your repo has one working directory; your AI agents want five.
[Workforest](https://github.com/ArkadyBuryakov/workforest) gives every
feature — and every agent — its own disposable git worktree. This
extension puts the forest in VS Code: see it, create and open worktrees,
run the project's scripts in them, and clean them up, without leaving the
editor.

Everything goes through the `workforest` command-line tool, so the
extension and your shell agree on where worktrees live, which setup runs in
a fresh one, and what `wf run NAME` does — all of it from the same
`.workforest.yaml`.

## Requirements

- Linux or macOS, and git. The extension ships the `workforest` CLI it
  drives — a self-contained executable in `bin/`, one per platform — so
  nothing else has to be installed.
- The bundled copy is the one the extension uses: it was built together
  with this .vsix, so the two always match. Installing `workforest`
  separately (`uv tool install workforest`, `brew install
  arkadyburyakov/tap/workforest`, `yay -S workforest`; see the [install
  instructions](https://github.com/ArkadyBuryakov/workforest#install)) is
  what gets you `wf` in your shell, the man pages, and `wf open`'s `cd` —
  and is required on a platform this build carries no executable for, where
  the extension falls back to `PATH`, `~/.local/bin` (`uv tool`, pipx),
  `/opt/homebrew/bin`, `/usr/local/bin`, and `/usr/bin`. An installed CLI
  has to be 0.6 or later: the view reads `workforest list --json`, which
  older releases lack.
- The extension runs where the repository is (it is a workspace extension),
  so it works over Remote-SSH, containers, and WSL — the bundled CLI is the
  remote's, since the remote installs its own platform's build.

## Features

**The Workforest sidebar** (the tree icon in the Activity Bar) has the
same toolbar as the JetBrains plugin in its header — Create, Open, Run
Script, Checkout, Delete, Refresh, and under `…` Stop Script, Show Merged
Configuration, the two Init commands, and Settings — each asking what to
act on, except Checkout and Delete, which act on the worktree this window
is in (after confirming it) and only ask from the main checkout. Below
it, two collapsible sections:

- **Scripts**: the `scripts` of this window's repository, with their
  command in the tooltip. ▶ on a row runs it in this window's worktree
  (`workforest run NAME` in a new integrated terminal), ■ stops it. A
  running script's icon turns light blue while it runs in this window's
  worktree and orange while it runs only in others, with every instance
  counted next to the name (`1 here`, `2 here, 3 elsewhere`) and spelled
  out in the tooltip. The marks sit at the name, never at the buttons, so ▶ and ■
  stay put as a script starts or stops. The section follows
  `.workforest.yaml` edits and the running scripts of every worktree.
- **Worktrees**: the main checkout first, then every managed worktree,
  most recently opened first (worktrees this VS Code has never opened
  sort by creation time), each with its branch, a `●` when it has
  uncommitted changes, and which one this window is in. Inline buttons
  on a row open that worktree in a new window, open a terminal there, or
  delete it; the context menu has the rest. With a multi-root workspace
  spanning several repositories, each forest is a node of its own.

**Commands** (all under `Workforest:` in the Command Palette):

| Command | What it runs |
|---|---|
| Create Worktree… | `workforest create BRANCH --no-open`, then opens the worktree. The picker lists branches not yet checked out (local and remote, like the CLI's completion); typing a name that matches none creates a new branch. |
| Open Worktree… | opens the main checkout or a worktree in a new window, this window, or asks — see `workforest.openIn`. |
| Delete Worktree… | `workforest delete NAME... --force` after its own confirmation for uncommitted changes, and asks whether to delete the branch. Invoked from the header or the palette it targets the worktree this window is in; from the tree, the row it was invoked on. Deleting the worktree this window shows moves the window to the main checkout. |
| Checkout into Main Checkout… | `workforest checkout NAME --force`: fold a worktree back into the main checkout — this window's, unless invoked on a row. |
| Run Script… | `workforest run NAME` in a new integrated terminal in the chosen worktree (this window's by default; from a worktree's context menu, that worktree), so Ctrl-C, colors, and background scripts behave exactly as in your shell. |
| Stop Script… | `workforest stop NAME` in the chosen worktree. |
| Open in Integrated Terminal | a terminal in the worktree's directory. |
| Show Merged Configuration | `workforest config` in an editor tab (also in the Scripts view's `…` menu). |
| Initialize Project Config | `workforest init`: scaffolds `.workforest.yaml` and opens it. |
| Initialize Local Config | `workforest init --local`: scaffolds `.vscode/.workforest.yaml`, the untracked per-developer override layer, and opens it. |
| Settings… | opens this extension's settings. |
| Refresh | re-reads the forest. It also refreshes when the window regains focus, when worktrees are added or removed from any terminal, and when a script starts or stops anywhere in the project. |

**Status bar**: the worktree this window is in (`workforest.statusBar`);
click it to switch.

## Settings

| Setting | Default | Meaning |
|---|---|---|
| `workforest.openIn` | `newWindow` | where created/opened worktrees open: `newWindow`, `currentWindow`, or `ask` |
| `workforest.statusBar` | `true` | show the current worktree on the status bar |

The extension has no configuration of its own beyond these: openers,
symlinks, setup scripts, and scripts all live in the
[Workforest configuration](https://github.com/ArkadyBuryakov/workforest#configuration),
which you can also point at VS Code from the terminal (`wf create feat -o code`).

## Troubleshooting

The **Workforest** output channel (View → Output) logs every `workforest`
invocation with the executable it resolved to, and its stderr. A configuration error (`workforest` exits 4)
is shown as a warning; fix the file it names and refresh.

## Development

```sh
npm install
npm run check    # compile, unit tests (node:test, no VS Code needed), .vsix
npm run smoke    # the extension inside a real VS Code against a scratch forest
```

`media/workforest.svg`, `media/icon.svg` and `media/icon.png` are generated
from the project logo by `assets/generate` in the repository root — edit
`assets/src/logo-icon.svg` and re-run that, never these files.

`npm run smoke` needs `code` on the PATH, a display, and this repository's
`.venv` (`uv sync` in the repository root): it builds a throwaway
repository with one worktree, starts an isolated VS Code (its own
user-data and extensions directories) with the extension in development
mode, and runs `src/smoke/index.ts` inside the extension host.

`npm run check` builds a `.vsix` with no CLI in it — during development the
installed `workforest` is used anyway. The published packages are one per
platform, each with the executable that platform's runner froze:

```sh
../../packaging/binary/build.sh bin       # the CLI for this machine, into bin/
npm run package -- --target linux-x64     # .vsix for one platform, carrying it
```
