"""Creation hooks (symlinks, setup scripts) and named-script execution.

Scripts get exactly one environment variable family, WF_* (DESIGN §3.6), and
run via $SHELL -c (sh -c fallback). Their stdout is routed to our stderr so
the cd protocol on stdout stays clean.
"""

import io
import os
import shlex
import subprocess
import sys
from pathlib import Path

from workforest import gitutil, output
from workforest.config import Config
from workforest.errors import WorkforestError

EXCLUDE_FILE_NAME = "workforest.exclude"


def script_env(
    *,
    main: Path,
    worktree: Path,
    worktrees_dir: Path,
    branch: str | None,
) -> dict[str, str]:
    env = os.environ.copy()
    env.update(
        {
            "WF_MAIN": str(main),
            "WF_NAME": main.name,
            "WF_WORKTREE": str(worktree),
            "WF_WORKTREES_DIR": str(worktrees_dir),
            "WF_BRANCH": branch or "",
        }
    )
    return env


def _shell() -> str:
    return os.environ.get("SHELL") or "sh"


def run_snippet(snippet: str, *, cwd: Path, env: dict[str, str]) -> int:
    """Run a config-defined shell snippet with stdout diverted to stderr."""
    argv = [_shell(), "-c", snippet]
    try:
        stderr_fd: int | None = sys.stderr.fileno()
    except io.UnsupportedOperation, AttributeError:
        stderr_fd = None
    if stderr_fd is not None:
        result = subprocess.run(argv, cwd=cwd, env=env, stdout=stderr_fd, check=False)
        return result.returncode
    captured = subprocess.run(argv, cwd=cwd, env=env, capture_output=True, text=True, check=False)
    if captured.stdout:
        sys.stderr.write(captured.stdout)
    if captured.stderr:
        sys.stderr.write(captured.stderr)
    return captured.returncode


def create_symlinks(config: Config, *, main: Path, worktree: Path) -> list[str]:
    """Symlink configured repo-root-relative paths from main into the
    worktree; returns the created relative paths."""
    created: list[str] = []
    for rel in config.symlinks:
        rel = rel.strip("/")
        if not rel:
            continue
        src = main / rel
        dst = worktree / rel
        if not src.exists():
            output.warn(f"symlink source does not exist, skipping: {src}")
            continue
        if dst.exists() and not dst.is_symlink():
            output.warn(f"destination exists and is not a symlink, skipping: {dst}")
            continue
        dst.parent.mkdir(parents=True, exist_ok=True)
        if dst.is_symlink():
            dst.unlink()
        dst.symlink_to(src)
        output.success(f"symlinked {rel} -> {src}")
        created.append(rel)
    if created:
        exclude_from_git(worktree, created)
    return created


def exclude_from_git(worktree: Path, rel_paths: list[str]) -> None:
    """Hide the given root-relative paths from git status in this worktree
    only, via a per-worktree core.excludesFile seeded with the user's global
    excludes (so overriding the file loses nothing)."""
    git_dir = gitutil.git_dir(worktree)
    exclude_file = git_dir / EXCLUDE_FILE_NAME

    gitutil.set_config(worktree, "extensions.worktreeConfig", "true")
    gitutil.set_config(worktree, "core.excludesFile", str(exclude_file), per_worktree=True)

    lines = ["# Managed by workforest: symlinks from the `symlinks` config key"]
    global_excludes = gitutil.global_excludes_file()
    if global_excludes is not None and global_excludes.is_file():
        lines.append(f"# --- inherited from global core.excludesFile: {global_excludes} ---")
        lines.append(global_excludes.read_text().rstrip("\n"))
        lines.append("# --- workforest symlinks ---")
    lines.extend(f"/{rel}" for rel in rel_paths)
    exclude_file.write_text("\n".join(lines) + "\n")
    output.success(f"excluded {len(rel_paths)} symlink(s) from git in this worktree")


def run_setup_scripts(config: Config, *, worktree: Path, env: dict[str, str]) -> int:
    """Run setup_scripts in order; failures warn but do not abort. Returns
    the number of failed scripts."""
    failures = 0
    for snippet in config.setup_scripts:
        output.success(f"running setup script: {snippet}")
        if run_snippet(snippet, cwd=worktree, env=env) != 0:
            output.warn(f"setup script failed: {snippet}")
            failures += 1
    return failures


def run_named_script(
    config: Config,
    name: str,
    *,
    cwd: Path,
    env: dict[str, str],
    extra_args: list[str] | None = None,
) -> None:
    """Run a `scripts` entry from the merged config; raise on failure.

    extra_args are shell-quoted and appended to the snippet, so
    `wf run make check` runs `make check` for a script defined as `make`.
    """
    snippet = config.scripts.get(name)
    if snippet is None:
        available = ", ".join(sorted(config.scripts)) or "none defined"
        raise WorkforestError(f"no script named {name!r} (available: {available})")
    if extra_args:
        snippet = f"{snippet} {' '.join(shlex.quote(arg) for arg in extra_args)}"
    output.success(f"running {name!r} in {cwd}: {snippet}")
    code = run_snippet(snippet, cwd=cwd, env=env)
    if code != 0:
        raise WorkforestError(f"script {name!r} failed with exit code {code}")
