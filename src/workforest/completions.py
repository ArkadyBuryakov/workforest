"""`workforest --complete TOPIC` backend.

Completion must never break the shell: any error yields an empty candidate
list, and everything stays on stdout as plain lines. Most topics emit bare
names; the `commands` topic emits `NAME<TAB>KIND<TAB>DESCRIPTION` and the
`branches` topic `NAME<TAB>LOCATION` so shells that can render descriptions
(zsh) do, while others take field 1.
"""

from workforest import commands, gitutil, launch
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
    config = _config()
    # An opener sharing a subcommand's name is shadowed by it (cli._preprocess
    # dispatches known subcommands first), so don't offer it.
    return [f"{name}\tcommand\t{SUBCOMMAND_HELP[name]}" for name in sorted(known)] + [
        # collapse whitespace: a tab/newline in an opener command would break the line protocol
        f"{name}\topener\t{' '.join(launch.describe_opener(config, name).split())}"
        for name in sorted(config.openers)
        if name not in known
    ]


def _branches() -> list[str]:
    """`NAME<TAB>LOCATION` lines, minus branches already checked out: local
    branches by their bare name (location lists their remotes too), remote-only
    branches remote-qualified — the form `wf create` resolves unambiguously."""
    root = gitutil.repo_root()
    taken = {w.branch for w in gitutil.list_worktrees(root) if w.branch}
    local = gitutil.local_branches(root)
    remote_map = gitutil.remote_branches(root)
    lines = [
        f"{branch}\t{', '.join(['local', *remote_map.get(branch, [])])}"
        for branch in local
        if branch not in taken
    ]
    for branch, remotes in remote_map.items():
        if branch not in taken and branch not in local:
            lines.extend(f"{remote}/{branch}\t{remote}" for remote in remotes)
    return lines


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
    return [s.id for s in claude.list_new_sessions(ctx.main, ctx.cwd_root)]
