# Changelog

## 0.7.0

Makefile targets are scripts without being configured.

- `wf make TARGET` runs any target of the worktree's `GNUmakefile`,
  `makefile`, or `Makefile` with everything `wf run` gives a configured
  script: extra arguments, `-b` and a log file, the process group and
  terminal handling, exit status, records, `exclusive` preemption, and
  cleanup. A target runs under the name `make:TARGET`, so it never
  shadows the `scripts` map; `wf stop --make TARGET` stops it again.
- The new `make` config section says which targets are offered by name —
  in shell completion and in the editors' script lists — through
  `hidden`, `hide_scripts`, `show_scripts`, and `exclusive_scripts`.
  Hiding is about what is offered, not what may run: a hidden target
  still runs when named. Targets are read out of the makefile rather
  than from `make`, so listing them never evaluates a `$(shell ...)`.
- Both editor clients list the makefile targets alongside the scripts,
  badged `make`, and run and stop them the same way.

## 0.6.1

- The VS Code extension is published to
  [Open VSX](https://open-vsx.org/extension/ArkadyBuryakov/workforest) as
  well as the Visual Studio Marketplace, so VSCodium, Cursor, Windsurf and
  the other forks install it from the registry they use.
- On the Visual Studio Marketplace the extension is `workforest-vscode`,
  shown as **Workforest for VS Code** — the plain name is held there by
  another publisher. On Open VSX it stays `workforest`.

## 0.6.0

The first release with editor clients: a VS Code extension and a JetBrains
plugin, each on its marketplace.

Both ship the `workforest` CLI. The package for each platform
(`linux-x64`, `linux-arm64`, `darwin-x64`, `darwin-arm64`) carries a
self-contained executable, and that is the one the client runs — it was
built with the package, so the two always match. Only where the package
has none does a client fall back to `PATH` and the usual install
directories; neither offers a setting for the path.

- VS Code: the Workforest sidebar — a header toolbar (create, open, run
  script, checkout, delete, refresh, and more under `…`) over two
  collapsible sections, Scripts (run and stop with one click) and
  Worktrees (the main checkout, then the managed worktrees by recency,
  with dirty markers and the worktree this window is in) — the same
  commands in the Command Palette, the merged configuration, config
  scaffolding, and a status bar item.
- JetBrains: the Workforest tool window over the same two sections, with
  tooltips, inline buttons and context menus; create, open, checkout and
  delete worktrees; run and stop scripts in a terminal tab; the current
  worktree on the status bar.
- Scripts are marked where they run: the row's icon turns light blue in
  this window's worktree and orange in the others, with the instance
  counts next to the name. The marks are part of the row, so the run and
  stop buttons no longer shift as a script starts or stops.
- Checkout and Delete from the header or the Command Palette act on the
  worktree this window is in, after confirming it, instead of asking
  which; from the main checkout they still ask.
- Any number of instances of a script may run at once, in one worktree or
  across several: each keeps a record and a log of its own
  (`WORKTREE.PID.log`) and runs its own `cleanup`, and `wf stop NAME`
  stops every instance in the worktree. `exclusive` is what holds a script
  to one.
- A group member can set `hidden: true`: it is then left out of shell
  completion and the clients' script lists, while `wf run` and `wf stop`
  still take its name.

## 0.5.2

- `wf list --json` describes the whole forest for programs.
- The project logo.
- Fixes: `wf init`, and foreground openers run from `workforest` rather
  than the `wf` shell function.

## 0.5.1

- Script groups: a `bulk` runs its members at once and relays their output
  line by line; a `pipeline` runs them in turn and stops at the first
  failure. Both are scripts like any other — `background`, `exclusive`,
  `cleanup` and `wf stop` apply to the group.
- Packaging fixes for the AUR package and the Homebrew formula.

## 0.5.0

- Scripts grew up: `background` (or `wf run -b`) detaches a script under a
  supervisor of its own with its output in a log, `exclusive` holds one to
  a single instance per project, `cleanup` runs when it ends, and
  `stop_timeout` bounds SIGTERM before SIGKILL.
- Man pages: `workforest(1)` and `workforest(5)`, installed with the
  package.
- Opener shortcuts are gone; openers are named in full.

## 0.4.0

- Openers reworked: one config shape for every kind of opener, with the
  window command alongside.

## 0.3.0

- Openers and `window_command` are shell-native — they are the command
  line you would type, not a template language.
- Better opener completion: an opener whose name overlaps a command is
  left out rather than shadowing it.
- Remote branch resolution across several remotes.
- The Claude Code integration is marked experimental.

## 0.2.3

- Virtual environments are scrubbed from a new worktree instead of being
  copied into it.
- Packaging moved to templates rendered at release time, so a release no
  longer commits back to the repository.

## 0.2.2

- macOS support, via a Homebrew tap.

## 0.2.0

- Opener variables and placeholders reworked.

## 0.1.1

- The AUR package.

## 0.1.0

First release: worktrees created, opened, and deleted in a predictable
place, with the project's own symlinks and setup scripts run for each; the
`wf` shell function and its alias; `wf run` for the project's `scripts`;
shell completion; PyPI packaging.
