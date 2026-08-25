"""argparse front-end: error → exit-code mapping and the sole writer to
stdout."""

import argparse
import os
import signal
import sys
from pathlib import Path
from typing import Any

from workforest import __version__, commands, output
from workforest.commands import CommandResult
from workforest.errors import EXIT_OK, WorkforestError
from workforest.launch import ShellAction

# Single source for subcommand names and their one-line help: build_parser()
# and the `commands` completion topic both read from here.
SUBCOMMAND_HELP: dict[str, str] = {
    "create": "create (or reuse) a worktree for a branch and open it",
    "open": "open an existing worktree",
    "list": "list managed worktrees",
    "delete": "delete worktree(s)",
    "checkout": "delete a worktree and check its branch out in main",
    "run": "run a named script from the merged config",
    "tui": "interactive mode (requires fzf)",
    "init": "scaffold a .workforest.yaml project config",
    "config": "show the merged configuration and its sources",
    "shell-init": "print the wf shell wrapper (eval in your shell rc)",
    "claude": "Claude Code integration (experimental: may break on any Claude Code update)",
}

SUBCOMMANDS = frozenset(SUBCOMMAND_HELP) - {"claude"}  # claude is feature-gated

# Marks a stdout line as a directive for the wf shell wrapper to eval. The
# unit-separator control byte cannot appear in data output (listings, dumps),
# so the wrapper never mistakes data for something to execute.
SHELL_DIRECTIVE_PREFIX = "\x1f"


def _claude_available() -> bool:
    """Feature gate: the integration is invisible without
    ~/.claude. Filesystem check only — core never imports the integration."""
    return (Path.home() / ".claude").is_dir()


def _known_subcommands() -> frozenset[str]:
    if _claude_available():
        return SUBCOMMANDS | {"claude"}
    return SUBCOMMANDS


def _emit(result: CommandResult) -> None:
    match result:
        case ShellAction(script=script):
            print(f"{SHELL_DIRECTIVE_PREFIX}{script}")
        case str() as text if text:
            print(text)
        case _:
            pass


# --- handlers -----------------------------------------------------------


def _handle_create(ns: argparse.Namespace) -> CommandResult:
    ctx = commands.build_context()
    return commands.cmd_create(
        ctx,
        ns.branch,
        opener=ns.opener,
        wrap=ns.wrap,
        path_arg=ns.path,
        no_hooks=ns.no_hooks,
        no_open=ns.no_open,
    )


def _handle_open(ns: argparse.Namespace) -> CommandResult:
    ctx = commands.build_context()
    return commands.cmd_open(ctx, ns.name, opener=ns.opener, wrap=ns.wrap, path_arg=ns.path)


def _handle_list(ns: argparse.Namespace) -> CommandResult:
    ctx = commands.build_context()
    return commands.cmd_list(ctx, porcelain=ns.porcelain)


def _handle_delete(ns: argparse.Namespace) -> CommandResult:
    ctx = commands.build_context()
    delete_branch: bool | None = None
    if ns.delete_branch:
        delete_branch = True
    elif ns.keep_branch:
        delete_branch = False
    return commands.cmd_delete(ctx, ns.names, force=ns.force, delete_branch=delete_branch)


def _handle_checkout(ns: argparse.Namespace) -> CommandResult:
    ctx = commands.build_context()
    return commands.cmd_checkout(ctx, ns.name, force=ns.force)


def _handle_run(ns: argparse.Namespace) -> CommandResult:
    ctx = commands.build_context()
    return commands.cmd_run(ctx, ns.script, ns.args)


def _handle_init(ns: argparse.Namespace) -> CommandResult:
    ctx = commands.build_context()
    return commands.cmd_init(ctx, local=ns.local)


def _handle_config(ns: argparse.Namespace) -> CommandResult:
    return commands.cmd_config_show(as_json=ns.json)


def _handle_shell_init(ns: argparse.Namespace) -> CommandResult:
    from workforest import shellinit

    return shellinit.shell_init(ns.shell)


def _handle_tui(ns: argparse.Namespace) -> CommandResult:
    from workforest import tui

    return tui.run(ns.mode)


def _handle_claude(ns: argparse.Namespace) -> CommandResult:
    from workforest.integrations import claude

    claude.cmd_copy_session(ns.session_id)
    return None


# --- parser -------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="workforest",
        description="Git worktree forest management",
    )
    parser.add_argument("--version", action="version", version=f"workforest {__version__}")
    sub = parser.add_subparsers(dest="command", required=True)

    def opener_args(p: argparse.ArgumentParser) -> None:
        p.add_argument("-o", "--opener", help="opener name or shell command")
        p.add_argument(
            "-w",
            "--wrap",
            metavar="WRAPPER",
            help="run the opener through this `wrappers` entry ('' for none)",
        )
        p.add_argument(
            "-p", "--path", help="path inside the worktree, passed to the opener as $WF_TARGET"
        )

    p = sub.add_parser("create", help=SUBCOMMAND_HELP["create"])
    p.add_argument(
        "branch", nargs="?", help="branch name or REMOTE/BRANCH (default: current branch)"
    )
    opener_args(p)
    p.add_argument("--no-hooks", action="store_true", help="skip symlinks and setup scripts")
    p.add_argument("--no-open", action="store_true", help="create only, do not open")
    p.set_defaults(func=_handle_create)

    p = sub.add_parser("open", help=SUBCOMMAND_HELP["open"])
    p.add_argument("name", nargs="?", help="worktree directory name")
    opener_args(p)
    p.set_defaults(func=_handle_open)

    p = sub.add_parser("list", help=SUBCOMMAND_HELP["list"])
    p.add_argument("--porcelain", action="store_true", help="stable tab-separated output")
    p.set_defaults(func=_handle_list)

    p = sub.add_parser("delete", help=SUBCOMMAND_HELP["delete"])
    p.add_argument("names", nargs="+", metavar="NAME")
    p.add_argument("--force", action="store_true", help="skip the dirty-worktree confirmation")
    group = p.add_mutually_exclusive_group()
    group.add_argument("--delete-branch", action="store_true", help="also delete the branch")
    group.add_argument("--keep-branch", action="store_true", help="never delete the branch")
    p.set_defaults(func=_handle_delete)

    p = sub.add_parser("checkout", help=SUBCOMMAND_HELP["checkout"])
    p.add_argument("name", metavar="NAME")
    p.add_argument("--force", action="store_true", help="skip the dirty-worktree confirmation")
    p.set_defaults(func=_handle_checkout)

    p = sub.add_parser("run", help=SUBCOMMAND_HELP["run"])
    p.add_argument("script", metavar="SCRIPT")
    p.add_argument(
        "args",
        nargs=argparse.REMAINDER,
        metavar="ARGS",
        help="appended (shell-quoted) to the script command",
    )
    p.set_defaults(func=_handle_run)

    p = sub.add_parser("init", help=SUBCOMMAND_HELP["init"])
    p.add_argument(
        "--local",
        action="store_true",
        help="place it in .vscode/ or .idea/ (untracked per-developer override)",
    )
    p.set_defaults(func=_handle_init)

    p = sub.add_parser("config", help=SUBCOMMAND_HELP["config"])
    p.add_argument("--json", action="store_true")
    p.set_defaults(func=_handle_config)

    p = sub.add_parser("shell-init", help=SUBCOMMAND_HELP["shell-init"])
    p.add_argument("shell", nargs="?", choices=("bash", "zsh"), help="default: from $SHELL")
    p.set_defaults(func=_handle_shell_init)

    p = sub.add_parser("tui", help=SUBCOMMAND_HELP["tui"])
    p.add_argument("mode", nargs="?", help="initial mode (create/open/checkout/delete)")
    p.set_defaults(func=_handle_tui)

    if _claude_available():
        p = sub.add_parser("claude", help=SUBCOMMAND_HELP["claude"])
        claude_sub = p.add_subparsers(dest="claude_command", required=True)
        cp = claude_sub.add_parser(
            "copy-session", help="copy a session from the main worktree into this one"
        )
        cp.add_argument("session_id", metavar="SESSION_ID")
        cp.set_defaults(func=_handle_claude)

    return parser


def _preprocess(argv: list[str]) -> list[str]:
    """No args → TUI."""
    return argv or ["tui"]


def main(argv: list[str] | None = None) -> int:
    args = _preprocess(list(sys.argv[1:] if argv is None else argv))

    if args and args[0] == "--complete":
        from workforest import completions

        topic = args[1] if len(args) > 1 else ""
        for line in completions.complete(topic):
            print(line)
        return EXIT_OK

    parser = build_parser()
    try:
        ns = parser.parse_args(args)
    except SystemExit as exc:  # argparse exits for --help/--version/usage errors
        return int(exc.code) if isinstance(exc.code, int) else EXIT_OK

    try:
        handler: Any = ns.func
        _emit(handler(ns))
        return EXIT_OK
    except WorkforestError as exc:
        if os.environ.get("WORKFOREST_DEBUG"):
            raise
        output.error(str(exc))
        return exc.exit_code
    except KeyboardInterrupt:  # pragma: no cover - terminates the process
        # Die by SIGINT instead of exiting normally: a parent shell decides
        # whether to abort a loop by how the child died (WIFSIGNALED), not by
        # its exit code. Explicit prompt cancels still exit EXIT_CANCELLED.
        output.info("")
        sys.stderr.flush()
        signal.signal(signal.SIGINT, signal.SIG_DFL)
        os.kill(os.getpid(), signal.SIGINT)
        return 128 + signal.SIGINT  # not reached
