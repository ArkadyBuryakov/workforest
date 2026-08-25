"""Typed subprocess wrappers around git plumbing.

The only module that spawns git. Consumers get typed results;
worktree data comes from `--porcelain -z` output, never from parsing the
human-readable form.
"""

import subprocess
from dataclasses import dataclass
from pathlib import Path

from workforest.errors import GitError, NotARepoError


def run_git(
    args: list[str],
    *,
    cwd: Path | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    """Run git and return the completed process; raise GitError on failure."""
    cmd = ["git", *args]
    result = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, check=False)
    if check and result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "unknown error"
        raise GitError(f"`{' '.join(cmd)}` failed: {detail}")
    return result


def git_output(args: list[str], *, cwd: Path | None = None) -> str:
    return run_git(args, cwd=cwd).stdout.strip()


def repo_root(cwd: Path | None = None) -> Path:
    result = run_git(["rev-parse", "--show-toplevel"], cwd=cwd, check=False)
    if result.returncode != 0:
        raise NotARepoError()
    return Path(result.stdout.strip())


@dataclass(slots=True, frozen=True)
class Worktree:
    path: Path
    head: str
    branch: str | None  # short name; None when detached or bare
    is_main: bool

    @property
    def name(self) -> str:
        return self.path.name


def parse_worktree_porcelain(data: str) -> list[Worktree]:
    """Parse `git worktree list --porcelain -z` output.

    Records are groups of NUL-terminated attribute lines separated by an
    empty entry. Unknown attributes (locked, prunable, future ones) are
    ignored. The first record is the main worktree — a git guarantee.
    """
    worktrees: list[Worktree] = []
    record: dict[str, str] = {}
    for token in data.split("\0"):
        if token == "":
            if record:
                worktrees.append(_record_to_worktree(record, is_main=not worktrees))
                record = {}
            continue
        key, _, value = token.partition(" ")
        record[key] = value
    if record:
        worktrees.append(_record_to_worktree(record, is_main=not worktrees))
    return worktrees


def _record_to_worktree(record: dict[str, str], *, is_main: bool) -> Worktree:
    branch = record.get("branch")
    if branch is not None:
        branch = branch.removeprefix("refs/heads/")
    return Worktree(
        path=Path(record["worktree"]),
        head=record.get("HEAD", ""),
        branch=branch,
        is_main=is_main,
    )


def list_worktrees(cwd: Path | None = None) -> list[Worktree]:
    result = run_git(["worktree", "list", "--porcelain", "-z"], cwd=cwd, check=False)
    if result.returncode != 0:
        raise NotARepoError()
    return parse_worktree_porcelain(result.stdout)


def main_worktree(cwd: Path | None = None) -> Path:
    return list_worktrees(cwd)[0].path


def current_branch(cwd: Path | None = None) -> str:
    """Short branch name, or "HEAD" when detached."""
    return git_output(["rev-parse", "--abbrev-ref", "HEAD"], cwd=cwd)


def local_branches(cwd: Path | None = None) -> list[str]:
    out = git_output(["for-each-ref", "--format=%(refname:short)", "refs/heads"], cwd=cwd)
    return out.splitlines() if out else []


def remotes(cwd: Path | None = None) -> list[str]:
    out = git_output(["remote"], cwd=cwd)
    return out.splitlines() if out else []


def remote_branches(cwd: Path | None = None) -> dict[str, list[str]]:
    """Map of branch name -> remotes that have it, across all remotes."""
    # Longest name first: a remote named "up/stream" must win over "up".
    names = sorted(remotes(cwd), key=len, reverse=True)
    out = git_output(["for-each-ref", "--format=%(refname)", "refs/remotes"], cwd=cwd)
    branches: dict[str, list[str]] = {}
    for ref in out.splitlines():
        ref = ref.removeprefix("refs/remotes/")
        for remote in names:
            name = ref.removeprefix(f"{remote}/")
            if name != ref:
                if name != "HEAD":
                    branches.setdefault(name, []).append(remote)
                break
    return branches


def branch_exists(branch: str, cwd: Path | None = None) -> bool:
    result = run_git(
        ["show-ref", "--verify", "--quiet", f"refs/heads/{branch}"], cwd=cwd, check=False
    )
    return result.returncode == 0


def find_branch_worktree(branch: str, cwd: Path | None = None) -> Worktree | None:
    for worktree in list_worktrees(cwd):
        if worktree.branch == branch:
            return worktree
    return None


def status_porcelain(path: Path) -> str:
    """Empty string means clean."""
    return run_git(["status", "--porcelain"], cwd=path).stdout.rstrip("\n")


def worktree_add(repo: Path, path: Path, branch: str, *, track: str | None = None) -> None:
    """Add a worktree: create `branch` tracking the `track` remote ref, check
    out the existing local branch, or create a brand-new branch."""
    if track is not None:
        run_git(["worktree", "add", "--track", "-b", branch, str(path), track], cwd=repo)
    elif branch_exists(branch, repo):
        run_git(["worktree", "add", str(path), branch], cwd=repo)
    else:
        run_git(["worktree", "add", "-b", branch, str(path)], cwd=repo)


def worktree_remove(repo: Path, path: Path, *, force: bool = False) -> None:
    args = ["worktree", "remove"]
    if force:
        args.append("--force")
    run_git([*args, str(path)], cwd=repo)


def checkout(path: Path, branch: str) -> None:
    run_git(["checkout", branch], cwd=path)


def delete_branch(repo: Path, branch: str) -> None:
    run_git(["branch", "-D", branch], cwd=repo)


def git_dir(worktree: Path) -> Path:
    """Per-worktree git dir (.git/worktrees/<name> for linked worktrees)."""
    return Path(git_output(["rev-parse", "--absolute-git-dir"], cwd=worktree))


def git_common_dir(worktree: Path) -> Path:
    """The repository's shared git dir (.git of the main checkout), the
    same from every worktree."""
    return Path(
        git_output(["rev-parse", "--path-format=absolute", "--git-common-dir"], cwd=worktree)
    )


def set_config(worktree: Path, key: str, value: str, *, per_worktree: bool = False) -> None:
    args = ["config"]
    if per_worktree:
        args.append("--worktree")
    run_git([*args, key, value], cwd=worktree)


def global_excludes_file() -> Path | None:
    """The user's global core.excludesFile, following git's own default."""
    result = run_git(["config", "--global", "--get", "core.excludesFile"], check=False)
    value = result.stdout.strip()
    if value:
        return Path(value).expanduser()
    import os

    xdg = os.environ.get("XDG_CONFIG_HOME")
    base = Path(xdg) if xdg else Path.home() / ".config"
    return base / "git" / "ignore"
