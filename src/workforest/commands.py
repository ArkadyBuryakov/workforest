"""Core commands. Each returns a ShellAction (stdout directive), a str
(stdout text), or None — cli.py is the sole stdout writer (DESIGN §5)."""

import importlib.resources
import json
from dataclasses import dataclass
from pathlib import Path

import yaml

from workforest import gitutil, hooks, launch, output
from workforest.config import (
    PROJECT_LOCAL_DIRS,
    Config,
    load_config,
    resolve_worktrees_dir,
)
from workforest.errors import CancelledError, NotARepoError, UsageError, WorkforestError
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


def managed_worktrees(ctx: Context) -> list[gitutil.Worktree]:
    """Worktrees located directly inside the resolved worktrees dir
    (DESIGN §3.5) — the only ones we list, complete, or delete."""
    return [
        worktree
        for worktree in gitutil.list_worktrees(ctx.main)
        if not worktree.is_main and worktree.path.parent == ctx.worktrees_dir
    ]


def find_managed(ctx: Context, name: str) -> gitutil.Worktree:
    for worktree in managed_worktrees(ctx):
        if worktree.name == name:
            return worktree
    raise WorkforestError(f"worktree {name!r} not found in {ctx.worktrees_dir}")


def short_branch_name(branch: str) -> str:
    return branch.rsplit("/", 1)[-1]


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
    path_arg: str | None = None,
    no_hooks: bool = False,
    no_open: bool = False,
) -> CommandResult:
    if not branch:
        branch = gitutil.current_branch(ctx.cwd_root)
        if branch == "HEAD":
            raise WorkforestError("detached HEAD: specify a branch name")

    existing = gitutil.find_branch_worktree(branch, ctx.main)
    if existing is not None:
        output.warn(f"branch {branch!r} already checked out at {existing.path}")
        worktree_path = existing.path
    else:
        worktree_path = ctx.worktrees_dir / short_branch_name(branch)
        registered = {w.path for w in gitutil.list_worktrees(ctx.main)}
        if worktree_path in registered:
            # Same directory name, different branch (feat/x vs fix/x): never
            # silently reuse another branch's worktree.
            raise WorkforestError(
                f"{worktree_path} already holds a different branch; "
                f"remove it first or use a different branch name"
            )
        if worktree_path.exists():
            raise WorkforestError(f"directory exists but is not a worktree: {worktree_path}")
        ctx.worktrees_dir.mkdir(parents=True, exist_ok=True)
        gitutil.worktree_add(ctx.main, worktree_path, branch)
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
        path_arg=path_arg,
    )


def cmd_open(
    ctx: Context,
    name: str | None,
    *,
    opener: str | None = None,
    path_arg: str | None = None,
) -> CommandResult:
    if not name:
        raise UsageError("worktree name required")
    worktree = find_managed(ctx, name)
    return launch.launch(
        ctx.config,
        main=ctx.main,
        worktree=worktree.path,
        worktrees_dir=ctx.worktrees_dir,
        branch=worktree.branch,
        opener_arg=opener,
        path_arg=path_arg,
    )


def cmd_list(ctx: Context, *, porcelain: bool = False) -> CommandResult:
    worktrees = managed_worktrees(ctx)
    if porcelain:
        lines = [
            "\t".join(
                (
                    w.name,
                    w.branch or "",
                    str(w.path),
                    "1" if gitutil.status_porcelain(w.path) else "0",
                )
            )
            for w in worktrees
        ]
        return "\n".join(lines)
    if not worktrees:
        output.info(f"no worktrees in {ctx.worktrees_dir} (create one with: wf create BRANCH)")
        return None
    name_width = max(len(w.name) for w in worktrees)
    branch_width = max(len(w.branch or "(detached)") for w in worktrees)
    rows = [
        f"{w.name:<{name_width}}  {w.branch or '(detached)':<{branch_width}}  "
        f"{'dirty' if gitutil.status_porcelain(w.path) else 'clean'}  {w.path}"
        for w in worktrees
    ]
    return "\n".join(rows)


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
    for name in names:
        worktree = find_managed(ctx, name)
        _confirm_dirty(worktree, "Delete anyway?", force=force)
        branch = worktree.branch
        gitutil.worktree_remove(ctx.main, worktree.path, force=True)
        output.success(f"deleted worktree {name!r}")
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
    branch = worktree.branch
    if branch is None:
        raise WorkforestError(f"cannot determine branch for worktree {name!r} (detached HEAD)")
    _confirm_dirty(
        worktree,
        "Delete worktree and checkout its branch in the main repo anyway?",
        force=force,
    )
    gitutil.worktree_remove(ctx.main, worktree.path, force=True)
    output.success(f"deleted worktree {name!r}")
    gitutil.checkout(ctx.main, branch)
    output.success(f"checked out {branch!r} in {ctx.main}")
    return launch.cd_action(ctx.main)


def cmd_run(ctx: Context, name: str, extra_args: list[str] | None = None) -> CommandResult:
    branch = gitutil.current_branch(ctx.cwd_root)
    env = _script_env(ctx, ctx.cwd_root, None if branch == "HEAD" else branch)
    hooks.run_named_script(ctx.config, name, cwd=ctx.cwd_root, env=env, extra_args=extra_args)
    return None


def _scaffold_template() -> str:
    resource = importlib.resources.files("workforest") / "examples" / ".workforest.yaml"
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
    output.success(f"scaffolded {target}")
    return None


def cmd_config_show(*, as_json: bool = False) -> CommandResult:
    try:
        ctx = build_context()
        config = ctx.config
    except NotARepoError:
        config = load_config(None)
    sources = [(layer, str(path)) for layer, path in config.sources]
    if as_json:
        return json.dumps({"config": config.as_dict(), "sources": sources}, indent=2)
    dump = yaml.safe_dump(config.as_dict(), sort_keys=False).rstrip("\n")
    lines = [dump, "", "# sources (low -> high):"]
    if sources:
        lines.extend(f"#   {layer}: {path}" for layer, path in sources)
    else:
        lines.append("#   (built-in defaults only)")
    return "\n".join(lines)
