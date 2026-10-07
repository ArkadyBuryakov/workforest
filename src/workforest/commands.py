"""Core commands. Each returns a ShellAction (stdout directive), a str
(stdout text), or None — cli.py is the sole stdout writer."""

import importlib.resources
import json
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import yaml

from workforest import gitutil, hooks, launch, makefile, output
from workforest.config import (
    PROJECT_LOCAL_DIRS,
    Config,
    load_config,
    resolve_worktrees_dir,
)
from workforest.errors import (
    CancelledError,
    GitError,
    NotARepoError,
    UsageError,
    WorkforestError,
)
from workforest.launch import ShellAction

type CommandResult = ShellAction | str | None


@dataclass(slots=True)
class Context:
    cwd_root: Path  # repo root of the invocation directory
    main: Path  # main worktree ($WF_MAIN)
    config: Config
    worktrees_dir: Path

    @property
    def repo_name(self) -> str:
        return self.main.name


def build_context(cwd: Path | None = None) -> Context:
    cwd_root = gitutil.repo_root(cwd)
    main = gitutil.main_worktree(cwd_root)
    config = load_config(main)
    worktrees_dir = resolve_worktrees_dir(config, main)
    return Context(cwd_root=cwd_root, main=main, config=config, worktrees_dir=worktrees_dir)


def _is_managed(ctx: Context, worktree: gitutil.Worktree) -> bool:
    return not worktree.is_main and worktree.path.parent == ctx.worktrees_dir


def managed_worktrees(ctx: Context) -> list[gitutil.Worktree]:
    """Worktrees located directly inside the resolved worktrees dir —
    the only ones we list, complete, or delete."""
    return [w for w in gitutil.list_worktrees(ctx.main) if _is_managed(ctx, w)]


def find_managed(ctx: Context, name: str) -> gitutil.Worktree:
    for worktree in managed_worktrees(ctx):
        if worktree.name == name:
            return worktree
    raise WorkforestError(f"worktree {name!r} not found in {ctx.worktrees_dir}")


def one_line(text: str) -> str:
    """Whitespace runs collapsed to one space: a lock reason may hold tabs
    and newlines, and every line-oriented output is one record per line."""
    return " ".join(text.split())


def _locked_error(worktree: gitutil.Worktree) -> WorkforestError:
    reason = f" ({one_line(worktree.locked)})" if worktree.locked else ""
    name = worktree.name
    return WorkforestError(f"worktree {name!r} is locked{reason} — run: wf unlock {name}")


def _stale_error(worktree: gitutil.Worktree, then: str = "") -> WorkforestError:
    name = worktree.name
    if worktree.locked is not None:
        # `prune` skips a locked record, so naming it alone would be a dead end.
        return WorkforestError(
            f"worktree {name!r} is stale and locked — run: wf unlock {name}, then wf prune"
        )
    return WorkforestError(f"worktree {name!r} is stale — run: wf prune{then}")


def _registered(ctx: Context, worktree: gitutil.Worktree) -> gitutil.Worktree | None:
    """The record as git has it now — it may have been locked, unlocked or
    pruned by someone else since we listed it."""
    return next((w for w in gitutil.list_worktrees(ctx.main) if w.path == worktree.path), None)


def _remove(ctx: Context, worktree: gitutil.Worktree) -> None:
    try:
        gitutil.worktree_remove(ctx.main, worktree.path, force=True)
    except GitError:
        # Locked since the check: say so in our words, not git's `fatal:`.
        current = _registered(ctx, worktree)
        if current is not None and current.locked is not None:
            raise _locked_error(current) from None
        raise


def _prune(ctx: Context) -> list[gitutil.Worktree]:
    """`git worktree prune`, returning the records it dropped."""
    before = gitutil.list_worktrees(ctx.main)
    gitutil.worktree_prune(ctx.main)
    left = {w.path for w in gitutil.list_worktrees(ctx.main)}
    return [w for w in before if w.path not in left]


def _label(ctx: Context, worktree: gitutil.Worktree) -> str:
    """A worktree by name when it is one of ours, by path otherwise."""
    return worktree.name if _is_managed(ctx, worktree) else str(worktree.path)


def _warn_left_behind(worktree: gitutil.Worktree) -> None:
    output.warn(
        f"left {worktree.path} in place: it is no longer a worktree, "
        "and whatever is in it is yours to keep or remove"
    )


def _forget(ctx: Context, worktree: gitutil.Worktree) -> None:
    """Drop one stale, unlocked record, never touching files.

    With the directory gone git removes just that record. With the
    directory still there (its `.git` file is what went missing) git
    refuses, and only a repository-wide prune clears it — so say what else
    went, and that the directory stays."""
    if _registered(ctx, worktree) is None:
        return  # already gone, e.g. pruned along with an earlier one
    if not worktree.path.exists():
        _remove(ctx, worktree)
        return
    pruned = _prune(ctx)
    others = [w for w in pruned if w.path != worktree.path]
    if len(others) == len(pruned):
        # Still there: locked since the check is the one way prune skips it.
        raise _locked_error(_registered(ctx, worktree) or worktree)
    _warn_left_behind(worktree)
    if others:
        names = ", ".join(_label(ctx, w) for w in others)
        output.warn(f"also pruned the other stale worktree records: {names}")


def _clear_stale(ctx: Context, worktree: gitutil.Worktree) -> None:
    """`create` found a stale record in its way: git does not prune on
    `worktree add`, so do it — unless a lock says to keep the record."""
    if worktree.locked is not None:
        if _is_managed(ctx, worktree):
            raise _stale_error(worktree)
        raise WorkforestError(
            f"a stale, locked worktree record at {worktree.path} is in the way — "
            f"run: git worktree unlock {worktree.path}, then wf prune"
        )
    _forget(ctx, worktree)
    output.warn(f"pruned the stale worktree record at {worktree.path}")


def short_branch_name(branch: str) -> str:
    return branch.rsplit("/", 1)[-1]


@dataclass(slots=True, frozen=True)
class ResolvedBranch:
    branch: str  # local branch name
    track: str | None  # remote ref the branch should track, if any


def _resolve_branch(ctx: Context, spec: str) -> ResolvedBranch:
    """Resolve BRANCH or REMOTE/BRANCH to a local branch and its remote ref.

    Precedence: exact local branch, explicit REMOTE/BRANCH, a branch known to
    exactly one remote; anything else names a new local branch.
    """
    if gitutil.branch_exists(spec, ctx.main):
        return ResolvedBranch(spec, track=None)
    remote_map = gitutil.remote_branches(ctx.main)
    for remote in sorted(gitutil.remotes(ctx.main), key=len, reverse=True):
        branch = spec.removeprefix(f"{remote}/")
        if branch == spec:
            continue
        if remote not in remote_map.get(branch, ()):
            raise WorkforestError(f"branch {branch!r} not found on remote {remote!r}")
        while gitutil.branch_exists(branch, ctx.main):
            # The obvious local name is taken (possibly tracking a different
            # remote), so the new branch needs a name of its own.
            if not output.interactive():
                raise WorkforestError(
                    f"branch {branch!r} already exists locally; `wf create {branch}` to use it"
                )
            branch = output.ask(f"branch {branch!r} already exists locally; local name for {spec}:")
            if not branch:
                raise CancelledError("cancelled")
        return ResolvedBranch(branch, track=spec)
    carriers = remote_map.get(spec, [])
    if len(carriers) > 1:
        raise WorkforestError(
            f"branch {spec!r} exists on multiple remotes ({', '.join(carriers)}); "
            f"pick one, e.g. `wf create {carriers[0]}/{spec}`"
        )
    return ResolvedBranch(spec, track=f"{carriers[0]}/{spec}" if carriers else None)


def _script_env(ctx: Context, worktree: Path, branch: str | None) -> dict[str, str]:
    return hooks.script_env(
        main=ctx.main,
        worktree=worktree,
        worktrees_dir=ctx.worktrees_dir,
        branch=branch,
    )


def cmd_create(
    ctx: Context,
    branch: str | None,
    *,
    opener: str | None = None,
    wrap: str | None = None,
    path_arg: str | None = None,
    no_hooks: bool = False,
    no_open: bool = False,
) -> CommandResult:
    if not branch:
        branch = gitutil.current_branch(ctx.cwd_root)
        if branch == "HEAD":
            raise WorkforestError("detached HEAD: specify a branch name")
    resolved = _resolve_branch(ctx, branch)
    branch = resolved.branch

    existing = gitutil.find_branch_worktree(branch, ctx.main)
    if existing is not None and gitutil.is_stale(existing):
        _clear_stale(ctx, existing)
        existing = None
    if existing is not None:
        output.warn(f"branch {branch!r} already checked out at {existing.path}")
        worktree_path = existing.path
    else:
        worktree_path = ctx.worktrees_dir / short_branch_name(branch)
        occupant = next(
            (w for w in gitutil.list_worktrees(ctx.main) if w.path == worktree_path), None
        )
        if occupant is not None and gitutil.is_stale(occupant):
            _clear_stale(ctx, occupant)
            occupant = None
        if occupant is not None:
            # Same directory name, different branch (feat/x vs fix/x): never
            # silently reuse another branch's worktree.
            raise WorkforestError(
                f"{worktree_path} already holds a different branch; "
                f"remove it first or use a different branch name"
            )
        if worktree_path.exists():
            raise WorkforestError(f"directory exists but is not a worktree: {worktree_path}")
        ctx.worktrees_dir.mkdir(parents=True, exist_ok=True)
        gitutil.worktree_add(ctx.main, worktree_path, branch, track=resolved.track)
        output.success(f"created worktree for {branch!r} at {worktree_path}")
        if not no_hooks:
            env = _script_env(ctx, worktree_path, branch)
            hooks.create_symlinks(ctx.config, main=ctx.main, worktree=worktree_path)
            failures = hooks.run_setup_scripts(ctx.config, worktree=worktree_path, env=env)
            if failures:
                output.warn(f"{failures} setup script(s) failed")

    if no_open:
        return None
    return launch.launch(
        ctx.config,
        main=ctx.main,
        worktree=worktree_path,
        worktrees_dir=ctx.worktrees_dir,
        branch=branch,
        opener_arg=opener,
        wrap_arg=wrap,
        path_arg=path_arg,
    )


def cmd_open(
    ctx: Context,
    name: str | None,
    *,
    opener: str | None = None,
    wrap: str | None = None,
    path_arg: str | None = None,
) -> CommandResult:
    if name:
        worktree = find_managed(ctx, name)
    else:
        # No name, but standing in a managed worktree: that's the one.
        current = next((w for w in managed_worktrees(ctx) if w.path == ctx.cwd_root), None)
        if current is None:
            raise UsageError("worktree name required (or run inside a managed worktree)")
        worktree = current
    if gitutil.is_stale(worktree):
        # A lock never blocks opening; a missing directory has to.
        again = f", then wf create {worktree.branch}" if worktree.branch else ""
        raise _stale_error(worktree, again)
    return launch.launch(
        ctx.config,
        main=ctx.main,
        worktree=worktree.path,
        worktrees_dir=ctx.worktrees_dir,
        branch=worktree.branch,
        opener_arg=opener,
        wrap_arg=wrap,
        path_arg=path_arg,
    )


@dataclass(slots=True, frozen=True)
class WorktreeState:
    dirty: bool | None  # None when stale: never asked
    locked: str | None  # the lock reason, "" when none was given
    stale: bool

    @property
    def label(self) -> str:
        """`clean`, `dirty` or `stale`, plus `locked`."""
        word = "stale" if self.stale else "dirty" if self.dirty else "clean"
        return f"{word} locked" if self.locked is not None else word


@dataclass(slots=True, frozen=True)
class ListedWorktree:
    worktree: gitutil.Worktree
    state: WorktreeState

    @property
    def prunable(self) -> str | None:
        """Why the record is stale, None when it is not. Git gives no
        reason for a stale record that is locked, so there is a stock one."""
        if not self.state.stale:
            return None
        return self.worktree.prunable or "the working tree is missing or is no longer a worktree"


def _inspect_one(worktree: gitutil.Worktree) -> ListedWorktree:
    stale = gitutil.is_stale(worktree)
    dirty: bool | None = None
    if not stale:
        # Only ever in a directory that is a worktree: anywhere else git
        # would fail, or answer for the repository enclosing it.
        try:
            dirty = bool(gitutil.status_porcelain(worktree.path))
        except GitError, OSError:
            stale = True  # went away, or broke, since the listing
    return ListedWorktree(worktree, WorktreeState(dirty, worktree.locked, stale))


def _inspect(worktrees: list[gitutil.Worktree]) -> list[ListedWorktree]:
    """One `git status` per live worktree; subprocess-bound, so run them
    together."""
    if not worktrees:
        return []
    with ThreadPoolExecutor(max_workers=min(8, len(worktrees))) as pool:
        return list(pool.map(_inspect_one, worktrees))


def _worktree_json(listed: ListedWorktree, running: dict[Path, dict[str, int]]) -> dict[str, Any]:
    worktree = listed.worktree
    return {
        "name": worktree.name,
        "branch": worktree.branch,  # null when detached
        "path": str(worktree.path),
        "dirty": listed.state.dirty,  # null when stale: never asked
        "locked": listed.state.locked,  # the reason ("" for none); null when not locked
        "prunable": listed.prunable,  # why it is stale; null when it is not
        # scripts running there: name → live instances
        "running": running.get(worktree.path, {}),
    }


def _list_json(ctx: Context) -> str:
    """The whole forest for programs (editor integrations): the main
    checkout in the same shape as the worktrees, plus where they live and
    what is running in each."""
    everything = gitutil.list_worktrees(ctx.main)
    main, *worktrees = _inspect([everything[0], *(w for w in everything if _is_managed(ctx, w))])
    running = hooks.running_scripts(ctx.main)
    data = {
        "main": _worktree_json(main, running),
        "worktrees_dir": str(ctx.worktrees_dir),
        "worktrees": [_worktree_json(listed, running) for listed in worktrees],
    }
    return json.dumps(data, indent=2)


def _flag_column(flag: str, reason: str | None) -> str:
    """A git flag as one TSV field: empty when absent, else the flag's own
    name and, after a space, its reason on one line — never empty for a
    flag that is set, whatever the reason."""
    if reason is None:
        return ""
    return f"{flag} {one_line(reason)}".rstrip()


def _porcelain_row(listed: ListedWorktree) -> str:
    worktree, state = listed.worktree, listed.state
    dirty = "" if state.dirty is None else "1" if state.dirty else "0"
    return "\t".join(
        (
            worktree.name,
            worktree.branch or "",
            str(worktree.path),
            dirty,
            _flag_column("locked", state.locked),
            _flag_column("prunable", listed.prunable),
        )
    )


def cmd_list(ctx: Context, *, porcelain: bool = False, as_json: bool = False) -> CommandResult:
    if as_json:
        return _list_json(ctx)
    listing = _inspect(managed_worktrees(ctx))
    if not listing:
        if porcelain:
            return ""
        output.info(f"no worktrees in {ctx.worktrees_dir} (create one with: wf create BRANCH)")
        return None
    if porcelain:
        return "\n".join(_porcelain_row(listed) for listed in listing)
    name_width = max(len(listed.worktree.name) for listed in listing)
    branch_width = max(len(listed.worktree.branch or "(detached)") for listed in listing)
    state_width = max(len(listed.state.label) for listed in listing)
    return "\n".join(
        f"{listed.worktree.name:<{name_width}}  "
        f"{listed.worktree.branch or '(detached)':<{branch_width}}  "
        f"{listed.state.label:<{state_width}}  {listed.worktree.path}"
        for listed in listing
    )


def _confirm_dirty(worktree: gitutil.Worktree, question: str, *, force: bool) -> None:
    """Shared delete/checkout guard for uncommitted changes."""
    if force:
        return
    changes = gitutil.status_porcelain(worktree.path)
    if not changes:
        return
    lines = changes.splitlines()
    output.warn(f"worktree {worktree.name!r} has uncommitted changes:")
    for line in lines[:10]:
        output.info(f"  {line}")
    if len(lines) > 10:
        output.info("  ...")
    if not output.confirm(question):
        raise CancelledError("cancelled") from None


def cmd_delete(
    ctx: Context,
    names: list[str],
    *,
    force: bool = False,
    delete_branch: bool | None = None,
) -> CommandResult:
    result: CommandResult = None
    # Resolve every name first: a typo must fail the batch before anything
    # is deleted, not strand it half-done.
    worktrees = [find_managed(ctx, name) for name in names]
    # The lock check belongs to the same pre-pass. --force never overrides a
    # lock: it answers "uncommitted changes", and one flag for both is how
    # the worktree locked against deletion gets deleted.
    for worktree in worktrees:
        if worktree.locked is not None:
            raise _locked_error(worktree)
    for worktree in worktrees:
        branch = worktree.branch
        if gitutil.is_stale(worktree):
            # Nothing to ask about and no files of ours to remove: only
            # the record goes.
            _forget(ctx, worktree)
            output.success(f"pruned stale worktree {worktree.name!r}")
        else:
            _confirm_dirty(worktree, "Delete anyway?", force=force)
            _remove(ctx, worktree)
            output.success(f"deleted worktree {worktree.name!r}")
        if worktree.path == ctx.cwd_root:
            # The shell is standing in the directory we just removed —
            # move it back to the main checkout.
            result = launch.cd_action(ctx.main)
        if branch is None:
            continue
        decision = delete_branch
        if decision is None and output.interactive():
            decision = output.confirm(f"Also delete branch {branch!r}?")
        if decision:
            try:
                gitutil.delete_branch(ctx.main, branch)
                output.success(f"deleted branch {branch!r}")
            except WorkforestError as exc:
                output.warn(str(exc))
    return result


def cmd_checkout(ctx: Context, name: str, *, force: bool = False) -> CommandResult:
    worktree = find_managed(ctx, name)
    if gitutil.is_stale(worktree):
        raise _stale_error(worktree)
    if worktree.locked is not None:
        raise _locked_error(worktree)
    branch = worktree.branch
    if branch is None:
        raise WorkforestError(f"cannot determine branch for worktree {name!r} (detached HEAD)")
    _confirm_dirty(
        worktree,
        "Delete worktree and checkout its branch in the main repo anyway?",
        force=force,
    )
    _remove(ctx, worktree)
    output.success(f"deleted worktree {name!r}")
    gitutil.checkout(ctx.main, branch)
    output.success(f"checked out {branch!r} in {ctx.main}")
    return launch.cd_action(ctx.main)


def cmd_lock(ctx: Context, name: str, *, reason: str | None = None) -> CommandResult:
    """Lock a worktree against `delete`, `checkout` and `prune`. A second
    lock is an error, as in git: it would silently replace the reason."""
    worktree = find_managed(ctx, name)
    current: gitutil.Worktree | None = worktree
    if worktree.locked is None:
        try:
            gitutil.worktree_lock(ctx.main, worktree.path, reason)
        except GitError:
            current = _registered(ctx, worktree)  # locked by someone else meanwhile?
            if current is None or current.locked is None:
                raise
        else:
            output.success(f"locked worktree {name!r}")
            return None
    held = f" ({one_line(current.locked)})" if current and current.locked else ""
    raise WorkforestError(
        f"worktree {name!r} is already locked{held} — run: wf unlock {name} to lock it anew"
    )


def cmd_unlock(ctx: Context, name: str) -> CommandResult:
    worktree = find_managed(ctx, name)
    if worktree.locked is not None:
        try:
            gitutil.worktree_unlock(ctx.main, worktree.path)
        except GitError:
            current = _registered(ctx, worktree)  # unlocked by someone else meanwhile?
            if current is None or current.locked is not None:
                raise
        else:
            output.success(f"unlocked worktree {name!r}")
            return None
    raise WorkforestError(f"worktree {name!r} is not locked")


def _records(count: int) -> str:
    return f"{count} stale worktree record{'' if count == 1 else 's'}"


def cmd_prune(ctx: Context, *, dry_run: bool = False) -> CommandResult:
    """Drop the records of worktrees that are gone. Repository-wide — git
    cannot prune one record — so every record removed is named, ours or
    not; and a locked one is kept and named too, rather than reporting
    nothing to do while a broken row sits in the listing."""
    stale = [w for w in gitutil.list_worktrees(ctx.main) if gitutil.is_stale(w)]
    held = [w for w in stale if w.locked is not None]
    gone = [w for w in stale if w.locked is None] if dry_run else _prune(ctx)
    if gone:
        names = ", ".join(_label(ctx, w) for w in gone)
        if dry_run:
            output.info(f"would prune {_records(len(gone))}: {names}")
        else:
            output.success(f"pruned {_records(len(gone))}: {names}")
            for worktree in gone:
                if worktree.path.exists():
                    _warn_left_behind(worktree)
    elif not held:
        output.info("no stale worktree records")
    if held:
        names = ", ".join(_label(ctx, w) for w in held)
        verb = "is" if len(held) == 1 else "are"
        output.warn(f"{_records(len(held))} {verb} locked: {names} — unlock to prune")
    return None


def _current_script_env(ctx: Context) -> dict[str, str]:
    branch = gitutil.current_branch(ctx.cwd_root)
    return _script_env(ctx, ctx.cwd_root, None if branch == "HEAD" else branch)


def cmd_run(
    ctx: Context,
    name: str,
    extra_args: list[str] | None = None,
    *,
    background: bool | None = None,
) -> CommandResult:
    hooks.run_named_script(
        ctx.config,
        name,
        cwd=ctx.cwd_root,
        env=_current_script_env(ctx),
        extra_args=extra_args,
        background=background,
    )
    return None


def cmd_make(
    ctx: Context,
    target: str,
    extra_args: list[str] | None = None,
    *,
    background: bool | None = None,
) -> CommandResult:
    """`wf make TARGET`: `make TARGET` at the worktree root, run as the
    script `make:TARGET` so it is recorded, stoppable and counted like any
    other. The target is make's to accept or reject — a hidden one still
    runs, and one this build generates is never in our list."""
    makefile.require(ctx.cwd_root)
    hooks.run_named_script(
        ctx.config,
        makefile.script_name(target),
        cwd=ctx.cwd_root,
        env=_current_script_env(ctx),
        extra_args=extra_args,
        background=background,
    )
    return None


def cmd_stop(
    ctx: Context, name: str, *, everywhere: bool = False, make: bool = False
) -> CommandResult:
    hooks.stop_script(
        ctx.config,
        makefile.script_name(name) if make else name,
        cwd=ctx.cwd_root,
        env=_current_script_env(ctx),
        everywhere=everywhere,
    )
    return None


def _scaffold_template() -> str:
    resource = importlib.resources.files("workforest") / "templates" / "project.yaml"
    return resource.read_text()


def cmd_init(ctx: Context, *, local: bool = False) -> CommandResult:
    if local:
        for candidate in PROJECT_LOCAL_DIRS:
            directory = ctx.main / candidate
            if directory.is_dir():
                break
        else:
            dirs = " or ".join(f"{d}/" for d in PROJECT_LOCAL_DIRS)
            raise WorkforestError(f"--local needs an IDE settings folder ({dirs}) in {ctx.main}")
    else:
        directory = ctx.main
    target = directory / ".workforest.yaml"
    if target.exists():
        raise WorkforestError(f"{target} already exists")
    target.write_text(_scaffold_template())
    output.success(f"scaffolded {target} (all keys commented out; see man 5 workforest)")
    return None


def cmd_config_show(*, as_json: bool = False) -> CommandResult:
    try:
        ctx = build_context()
        config = ctx.config
    except NotARepoError:
        config = load_config(None)
    if as_json:
        sources = [{"layer": s.layer, "path": str(s.path)} for s in config.sources]
        return json.dumps({"config": config.as_dict(), "sources": sources}, indent=2)
    dump = yaml.safe_dump(config.as_dict(), sort_keys=False).rstrip("\n")
    lines = [dump, "", "# sources (low -> high):"]
    if config.sources:
        lines.extend(f"#   {s.layer}: {s.path}" for s in config.sources)
    else:
        lines.append("#   (built-in defaults only)")
    return "\n".join(lines)
