# Workforest for JetBrains IDEs

Git worktree forest management inside IntelliJ IDEA, PyCharm, WebStorm,
GoLand, RustRover, and the other IntelliJ-based IDEs (2025.2 or later),
driven by the [`workforest`](../../README.md) command line tool. The plugin
is a thin client: everything it does is a `workforest` command, so the IDE
and your shell always agree about the forest.

## Features

**The Workforest tool window** (the tree icon on the left) has two
collapsible sections:

- **Scripts**: the `scripts` of this window's repository — a command, a
  `bulk`, or a `pipeline` (each with its own icon), `background` /
  `exclusive` flagged, the command in the tooltip. ▶ runs one in this
  window's worktree (`workforest run NAME` in a new terminal tab), ■ stops
  it; double-click runs. The section follows `.workforest.yaml` edits.
- **Worktrees**: the main checkout first, then every managed worktree,
  most recently opened in this IDE first (never-opened ones by creation
  time), each with its branch, a `●` when it has uncommitted changes, and
  which one this window is in (bold). Hovering a row shows its details
  and inline buttons: open in a new window, open a terminal there, delete.
  Double-click opens it (as the *Open worktrees in* setting says).

Every row has a **context menu** with the rest: worktrees — open in a new
or this window, open in terminal, run / stop a script *in that worktree*,
checkout, delete, copy path; scripts — run, stop, show the configuration;
the section headers — create a worktree, show / initialize the
configuration, refresh. The **header buttons** never act on the selection:
each asks with a popup you can type into to filter.

**Actions** (also under *Tools | Workforest* and in *Find Action*):

| Action | What it runs |
|---|---|
| Create Worktree… | `workforest create BRANCH --no-open`, then opens the worktree. The branch field completes the branches not yet checked out (local and remote, like the CLI's completion); a name that matches none creates a new branch. |
| Open Worktree… | opens the main checkout or a worktree in a new window, this window, or asks — see the *Open worktrees in* setting. |
| Delete Worktree… | `workforest delete NAME --force` after its own confirmation for uncommitted changes, and asks whether to delete the branch. Deleting the worktree this window shows replaces the window with the main checkout. |
| Checkout into Main Checkout… | `workforest checkout NAME --force`: fold a worktree back into the main checkout; offers to open it when no window shows it. |
| Run Script… | `workforest run NAME` in a new terminal tab in the chosen worktree (this window's by default; from a worktree's context menu, that worktree), so Ctrl-C, colors, and background scripts behave exactly as in your shell. Needs the bundled Terminal plugin. |
| Stop Script… | `workforest stop NAME` in the chosen worktree. |
| Open in Terminal | a terminal tab in the worktree's directory. |
| Show Merged Configuration | `workforest config` in a read-only editor tab. |
| Initialize Project Config | `workforest init`: scaffolds `.workforest.yaml` and opens it. |
| Initialize Local Config | `workforest init --local`: scaffolds `.idea/.workforest.yaml`, the untracked per-developer override layer, and opens it. |
| Refresh | re-reads the forest. It also refreshes when the window comes back to the front, when worktrees are added or removed from any terminal, and when a `.workforest.yaml` changes. |

**Status bar**: the worktree this window is in (`name (main)` for the main
checkout; hover for branch and path); click it to open another. Hide it
from the status bar's context menu like any widget.

The uncommitted-changes and branch-deletion questions the CLI asks on a
terminal become IDE dialogs; the CLI itself runs without a terminal and is
never prompted. A failed action shows the CLI's last stderr line as a
notification with *Show Output* for the whole of it; a broken
configuration file (`workforest` exits 4) is a warning. Any worktree of
the same repo works as the project: the CLI finds the main checkout from
it.

## Settings

*Settings | Tools | Workforest*:

- **Executable** — the `workforest` command. Empty (the default) finds it
  on the `PATH` the IDE sees, then in `~/.local/bin` (`uv tool`, pipx),
  `/opt/homebrew/bin`, `/usr/local/bin`, and `/usr/bin`. Set it when the
  IDE was launched without your shell's `PATH` (macOS dock, a desktop
  launcher).
- **Open worktrees in** — where Create and Open put the worktree: a new
  window (default), this window, or ask every time. Opening always keeps
  to that choice: the plugin opens projects with explicit options rather
  than the IDE's *Open project in* preference, which the platform applies
  inconsistently to directories that have no `.idea/` yet.

## Carrying `.idea/` into a worktree

JetBrains IDEs keep per-project state in `.idea/`; a fresh worktree has
only the tracked part, and run configurations or the chosen SDK usually are
not. Carry them over with a setup script that copies what the worktree
lacks. Never symlink `.idea/` (a `symlinks` entry): two windows must not
share one `workspace.xml`.

```yaml
# .workforest.yaml
setup_scripts:
  - '[ -d "$WF_MAIN/.idea" ] && mkdir -p "$WF_WORKTREE/.idea" && cp -Rn "$WF_MAIN/.idea/." "$WF_WORKTREE/.idea/" || true'
```

`.idea/.workforest.yaml` is also where a personal, untracked config layer
lives (`wf init --local`).

## Install

Until the plugin is on the JetBrains Marketplace, build it and drop it into
the IDE's plugins directory:

```sh
cd editors/idea
./gradlew buildPlugin      # compile, unit tests → build/distributions/workforest-idea-<version>.zip
```

Any JDK 17+ runs Gradle (an IDE's bundled JBR does: `JAVA_HOME=/opt/<ide>/jbr
./gradlew …`); the JDK 21 the build compiles with is fetched automatically
by Gradle's toolchain resolver. Then either *Settings | Plugins | ⚙ |
Install Plugin from Disk* with the zip, or from a shell — the plugins
directory is `~/.local/share/JetBrains/<Product><Version>/` on Linux (a
vendor-customized IDE uses its own vendor and data-directory names, as in
`product-info.json`), `~/Library/Application Support/JetBrains/<Product><Version>/plugins/` on macOS:

```sh
unzip -o build/distributions/workforest-idea-*.zip -d "$PLUGINS_DIR"   # install (what the dialog does)
rm -rf "$PLUGINS_DIR/workforest-idea"                                   # uninstall
```

Both take effect on the next IDE start. (The IDE's headless `installPlugins`
launcher command only takes Marketplace ids and repository URLs — it ends in
the same unpack.) As `scripts` in a personal `.workforest.yaml`, so that
`wf run plugin` rebuilds and reinstalls from any worktree:

```yaml
scripts:
  plugin-build: cd editors/idea && JAVA_HOME="${JAVA_HOME:-/opt/idea/jbr}" ./gradlew --quiet buildPlugin
  plugin-install:
    command: >-
      dir=~/.local/share/JetBrains/IdeaIC2025.2 &&
      rm -rf "$dir/workforest-idea" &&
      unzip -qo editors/idea/build/distributions/workforest-idea-*.zip -d "$dir"
  plugin-uninstall: rm -rf ~/.local/share/JetBrains/IdeaIC2025.2/workforest-idea
  plugin:
    pipeline: [plugin-build, plugin-install]
```

`workforest` itself must be installed too (see the project README).

## Troubleshooting

- *workforest not found* — install it, or set **Executable** (above). The
  tool window shows a *Configure…* link in that state.
- *unrecognized arguments: --json* or *cannot prompt … not a terminal* —
  a `workforest` older than the plugin expects (it needs `list --json`).
  Update it.
- A failed action shows the CLI's last stderr line as a notification. For
  the full output (a failing setup script, say), run the same command in a
  terminal: `wf create BRANCH --no-open`.
- Nothing listed but the forest is not empty: the project directory must be
  the main checkout or one of its worktrees; *Refresh* after `wf` changes
  made in a shell.

## Development

```sh
./gradlew build buildPlugin   # compile, unit tests, plugin zip (what CI runs)
./gradlew runIde              # a sandboxed IDE with the plugin installed
./gradlew verifyPlugin        # IntelliJ Plugin Verifier against recommended IDEs (downloads them)
./gradlew verifyPlugin -PlocalIde=/opt/idea   # ... and against an installed IDE (a newer one, say)
```

The plugin is compiled against 2025.2 and runs on later platforms, so
platform objects are only ever built through their methods — never through
Kotlin data-class `copy` or inline DSL builders such as `OpenProjectTask {}`,
which compile to constructor calls and throw `NoSuchMethodError` on a
platform that added a field. Verifying against the IDE you actually run
catches that — when the verifier can read it: it does not understand the
module layout of 2026.2+ snapshot builds (`The 'Core' plugin … was not
found`), so for those, run the plugin.

- `settings.gradle.kts` enables the foojay toolchain resolver: the build
  needs a JDK 21 toolchain and fetches one when the JDK running Gradle is
  another version.
- `Protocol.kt` — the only parser of the CLI's machine interface: `list
  --json` (main checkout, worktrees dir, worktrees), `--complete` lines,
  the last stderr line as the error message, and shell quoting. Pure and
  unit-tested (`ProtocolTest.kt`); JSON via the platform's bundled Gson.
- `WorkforestCli.kt` — the only place a process is spawned: finds the
  executable and runs it with `NO_COLOR` and no terminal. The plugin never
  runs git itself.
- `Actions.kt` — every action; a tree selection (`WORKTREE_KEY` /
  `SCRIPT_KEY`) is the target from the tree's context menus and inline
  buttons (`TREE_PLACE`), never from the toolbar (`TOOLBAR_PLACE`), which
  asks with a searchable popup. `terminal/` (Run Script, Open in Terminal)
  is registered from `workforest-terminal.xml`, loaded only with the
  Terminal plugin; the tree finds those actions by id and reports when
  they are absent.
- `WorkforestToolWindow.kt` — the tree (scripts, then worktrees) with
  tooltips, inline buttons (painted as an overlay at the right edge of the
  visible area for the hovered and the selected row — never part of the
  row, so a long path cannot push them out of reach — and hit-tested on
  the same rectangles), and context menus, fed by `WorktreeService`, which re-reads the forest
  after every action, on window activation, and on VFS changes under the
  main checkout's `.git/worktrees`, `.git/HEAD`, and the config files
  (watched even when outside the project), and orders it with `Recency`
  (last opened in this IDE — recorded by `RecordProjectOpenActivity`, as
  `RecentProjectsManager` is internal API — else creation time).
- `WorkforestStatusBar.kt` — the status-bar widget.
- `Projects.kt` — opening a worktree as a project, always in a new window:
  `ProjectUtil.openOrImport` drops `forceOpenInNewFrame` for directories
  without `.idea/`, so that directory is created first (the IDE would
  create it on opening anyway).
- `WorkforestSettings.kt` — the executable path and its settings page.
- `META-INF/pluginIcon.svg` (+ `_dark`, 40 px) and `icons/toolWindow.svg`
  (+ `_dark`, 13 px, the tool-window greys) are the project logo,
  `assets/logo.svg`, with a square padded viewBox — regenerate them from it
  when it changes.

The plugin is versioned on its own (`pluginVersion` in `gradle.properties`)
and depends only on the CLI's machine outputs (`list --json`, `--complete`),
not on a particular CLI version beyond the one that introduced them.
