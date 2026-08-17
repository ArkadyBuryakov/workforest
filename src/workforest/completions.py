"""`workforest --complete TOPIC` backend.

Completion must never break the shell: any error yields an empty candidate
list, and everything stays on stdout as plain lines. Most topics emit bare
names; the `commands` topic emits `NAME<TAB>KIND<TAB>DESCRIPTION` so shells
that can render descriptions (zsh) do, while others take field 1.
"""

from workforest import commands, gitutil
from workforest.config import Config, load_config
from workforest.errors import WorkforestError

TOPICS = ("commands", "branches", "worktrees", "scripts", "openers", "claude-sessions")


def complete(topic: str) -> list[str]:
    try:
        return _complete(topic)
    except WorkforestError, OSError:
        return []


def _complete(topic: str) -> list[str]:
    match topic:
        case "commands":
            return _commands()
        case "branches":
            return _branches()
        case "worktrees":
            return _worktrees()
        case "scripts":
            return sorted(_config().scripts)
        case "openers":
            return sorted(_config().openers)
        case "claude-sessions":
            return _claude_sessions()
        case _:
            return []


def _config() -> Config:
    try:
        return commands.build_context().config
    except WorkforestError:
        return load_config(None)


def _commands() -> list[str]:
    from workforest.cli import SUBCOMMAND_HELP, _known_subcommands

    known = _known_subcommands()
    openers = _config().openers
    # An opener sharing a subcommand's name is shadowed by it (cli._preprocess
    # dispatches known subcommands first), so don't offer it.
    return [f"{name}\tcommand\t{SUBCOMMAND_HELP[name]}" for name in sorted(known)] + [
        # collapse whitespace: a tab/newline in a template would break the line protocol
        f"{name}\topener\t{' '.join(openers[name].split())}"
        for name in sorted(openers)
        if name not in known
    ]


def _branches() -> list[str]:
    """Local then remote branches, minus those already checked out."""
    root = gitutil.repo_root()
    taken = {w.branch for w in gitutil.list_worktrees(root) if w.branch}
    candidates: dict[str, None] = {}
    for branch in (*gitutil.local_branches(root), *gitutil.remote_branches(root)):
        if branch not in taken:
            candidates.setdefault(branch)
    return list(candidates)


def _worktrees() -> list[str]:
    ctx = commands.build_context()
    return [w.name for w in commands.managed_worktrees(ctx)]


def _claude_sessions() -> list[str]:
    from workforest.integrations import claude

    if not claude.available():
        return []
    ctx = commands.build_context()
    if ctx.cwd_root == ctx.main:
        return []
    return [sid for sid, _ in claude.list_new_sessions(ctx.main, ctx.cwd_root)]
