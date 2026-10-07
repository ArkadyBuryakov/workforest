"""gitutil: porcelain parsing (pure, recorded samples) + real-repo queries."""

from collections.abc import Callable
from pathlib import Path

import pytest

from workforest import gitutil
from workforest.errors import GitError, NotARepoError

from .conftest import Repo

NUL = "\0"

# Recorded `git worktree list --porcelain -z` shapes (unit layer: no git).
MAIN_ONLY = "worktree /dev/api\0HEAD abc123\0branch refs/heads/main\0\0"
WITH_LINKED = (
    "worktree /dev/api\0HEAD abc123\0branch refs/heads/main\0\0"
    "worktree /dev/worktrees/api/feat\0HEAD def456\0branch refs/heads/feature/feat\0\0"
)
DETACHED = "worktree /dev/api\0HEAD abc123\0detached\0\0"
BARE = "worktree /dev/api.git\0bare\0\0"
LOCKED_AND_UNKNOWN = (
    "worktree /dev/api\0HEAD abc123\0branch refs/heads/main\0\0"
    "worktree /dev/worktrees/api/x\0HEAD def456\0branch refs/heads/x\0"
    "locked because reasons\0prunable gone\0future-attribute value\0\0"
)


class TestPorcelainParsing:
    def test_main_only(self) -> None:
        worktrees = gitutil.parse_worktree_porcelain(MAIN_ONLY)
        assert len(worktrees) == 1
        assert worktrees[0] == gitutil.Worktree(
            path=Path("/dev/api"), head="abc123", branch="main", is_main=True
        )

    def test_linked_worktree_and_main_flag(self) -> None:
        worktrees = gitutil.parse_worktree_porcelain(WITH_LINKED)
        assert [w.is_main for w in worktrees] == [True, False]
        assert worktrees[1].branch == "feature/feat"
        assert worktrees[1].name == "feat"

    def test_detached_head(self) -> None:
        (worktree,) = gitutil.parse_worktree_porcelain(DETACHED)
        assert worktree.branch is None

    def test_bare(self) -> None:
        (worktree,) = gitutil.parse_worktree_porcelain(BARE)
        assert worktree.branch is None
        assert worktree.head == ""

    def test_unknown_attributes_ignored(self) -> None:
        worktrees = gitutil.parse_worktree_porcelain(LOCKED_AND_UNKNOWN)
        assert len(worktrees) == 2
        assert worktrees[1].branch == "x"

    def test_lock_and_prunable_keep_their_reasons(self) -> None:
        main, linked = gitutil.parse_worktree_porcelain(LOCKED_AND_UNKNOWN)
        assert (main.locked, main.prunable) == (None, None)
        assert (linked.locked, linked.prunable) == ("because reasons", "gone")

    def test_bare_lock_is_an_empty_reason_not_an_absent_lock(self) -> None:
        data = MAIN_ONLY + "worktree /dev/worktrees/api/x\0HEAD def456\0locked\0\0"
        assert gitutil.parse_worktree_porcelain(data)[1].locked == ""

    def test_lock_reason_may_span_lines(self) -> None:
        data = MAIN_ONLY + "worktree /dev/worktrees/api/x\0HEAD d\0locked one\ntwo\0\0"
        assert gitutil.parse_worktree_porcelain(data)[1].locked == "one\ntwo"


def _record(path: Path, **flags: str | None) -> gitutil.Worktree:
    return gitutil.Worktree(path=path, head="abc", branch="x", is_main=False, **flags)


class TestIsStale:
    """One stat on the admin link; no git involved."""

    def linked(self, tmp_path: Path) -> Path:
        path = tmp_path / "x"
        path.mkdir()
        (path / ".git").write_text("gitdir: elsewhere\n")
        return path

    def test_healthy(self, tmp_path: Path) -> None:
        assert not gitutil.is_stale(_record(self.linked(tmp_path)))

    def test_directory_gone(self, tmp_path: Path) -> None:
        assert gitutil.is_stale(_record(tmp_path / "x"))

    def test_git_file_removed_directory_kept(self, tmp_path: Path) -> None:
        path = self.linked(tmp_path)
        (path / ".git").unlink()
        assert gitutil.is_stale(_record(path))

    def test_locked_and_gone_is_stale_though_git_does_not_say_prunable(
        self, tmp_path: Path
    ) -> None:
        assert gitutil.is_stale(_record(tmp_path / "x", locked="usb"))

    def test_locked_and_healthy_is_not_stale(self, tmp_path: Path) -> None:
        assert not gitutil.is_stale(_record(self.linked(tmp_path), locked=""))

    def test_gits_own_flag_is_enough(self, tmp_path: Path) -> None:
        # whatever the reason says: only its presence counts
        assert gitutil.is_stale(_record(self.linked(tmp_path), prunable=""))

    def test_main_is_never_stale(self, tmp_path: Path) -> None:
        main = gitutil.Worktree(path=tmp_path / "bare.git", head="", branch=None, is_main=True)
        assert not gitutil.is_stale(main)

    def test_empty(self) -> None:
        assert gitutil.parse_worktree_porcelain("") == []


class TestRepoQueries:
    def test_repo_root(self, repo: Repo) -> None:
        assert gitutil.repo_root(repo.path) == repo.path
        sub = repo.path / "src"
        sub.mkdir()
        assert gitutil.repo_root(sub) == repo.path

    def test_repo_root_outside(self, tmp_path: Path) -> None:
        outside = tmp_path / "not-a-repo"
        outside.mkdir()
        with pytest.raises(NotARepoError):
            gitutil.repo_root(outside)

    def test_list_worktrees_main(self, repo: Repo) -> None:
        (worktree,) = gitutil.list_worktrees(repo.path)
        assert worktree.is_main
        assert worktree.path == repo.path
        assert worktree.branch == "main"
        assert gitutil.main_worktree(repo.path) == repo.path

    def test_current_branch(self, repo: Repo) -> None:
        assert gitutil.current_branch(repo.path) == "main"

    def test_branches(self, make_repo: Callable[..., Repo]) -> None:
        repo = make_repo(origin=True)
        repo.add_branch("feature/local")
        repo.add_branch("remote-only", remote_only=True)
        assert set(gitutil.local_branches(repo.path)) == {"main", "feature/local"}
        assert gitutil.remotes(repo.path) == ["origin"]
        assert gitutil.remote_branches(repo.path) == {
            "main": ["origin"],
            "feature/local": ["origin"],
            "remote-only": ["origin"],
        }
        assert gitutil.branch_exists("feature/local", repo.path)
        assert not gitutil.branch_exists("remote-only", repo.path)

    def test_remote_branches_cover_all_remotes(self, make_repo: Callable[..., Repo]) -> None:
        repo = make_repo(origin=True)
        repo.add_remote("upstream")
        repo.add_branch("shared")
        repo.git("push", "-q", "upstream", "shared")
        repo.add_branch("upstream-only", remote_only=True, remote="upstream")
        branches = gitutil.remote_branches(repo.path)
        assert branches["shared"] == ["origin", "upstream"]
        assert branches["upstream-only"] == ["upstream"]

    def test_status_porcelain(self, repo: Repo) -> None:
        assert gitutil.status_porcelain(repo.path) == ""
        repo.make_dirty()
        assert "dirty.txt" in gitutil.status_porcelain(repo.path)


class TestMutations:
    def test_worktree_add_new_branch(self, repo: Repo, tmp_path: Path) -> None:
        target = tmp_path / "wt" / "feat"
        gitutil.worktree_add(repo.path, target, "feat")
        assert target.is_dir()
        found = gitutil.find_branch_worktree("feat", repo.path)
        assert found is not None and found.path == target
        assert gitutil.branch_exists("feat", repo.path)

    def test_worktree_add_local_branch(self, repo: Repo, tmp_path: Path) -> None:
        repo.add_branch("existing")
        target = tmp_path / "wt" / "existing"
        gitutil.worktree_add(repo.path, target, "existing")
        assert gitutil.current_branch(target) == "existing"

    def test_worktree_add_tracking_remote(
        self, make_repo: Callable[..., Repo], tmp_path: Path
    ) -> None:
        repo = make_repo(origin=True)
        repo.add_branch("remote-feat", remote_only=True)
        target = tmp_path / "wt" / "remote-feat"
        gitutil.worktree_add(repo.path, target, "remote-feat", track="origin/remote-feat")
        assert gitutil.current_branch(target) == "remote-feat"
        upstream = repo.git("rev-parse", "--abbrev-ref", "remote-feat@{upstream}")
        assert upstream == "origin/remote-feat"

    def test_worktree_remove_and_delete_branch(self, repo: Repo, tmp_path: Path) -> None:
        target = tmp_path / "wt" / "gone"
        gitutil.worktree_add(repo.path, target, "gone")
        gitutil.worktree_remove(repo.path, target)
        assert not target.exists()
        assert gitutil.find_branch_worktree("gone", repo.path) is None
        gitutil.delete_branch(repo.path, "gone")
        assert not gitutil.branch_exists("gone", repo.path)

    def test_worktree_remove_dirty_needs_force(self, repo: Repo, tmp_path: Path) -> None:
        target = tmp_path / "wt" / "dirty"
        gitutil.worktree_add(repo.path, target, "dirty")
        repo.make_dirty(worktree=target)
        with pytest.raises(GitError):
            gitutil.worktree_remove(repo.path, target)
        gitutil.worktree_remove(repo.path, target, force=True)
        assert not target.exists()

    def test_lock_unlock_prune(self, repo: Repo, tmp_path: Path) -> None:
        import shutil

        kept, gone = tmp_path / "wt" / "kept", tmp_path / "wt" / "gone"
        gitutil.worktree_add(repo.path, kept, "kept")
        gitutil.worktree_add(repo.path, gone, "gone")

        def flags() -> dict[str, tuple[str | None, bool]]:
            return {
                w.name: (w.locked, w.prunable is not None)
                for w in gitutil.list_worktrees(repo.path)
                if not w.is_main
            }

        gitutil.worktree_lock(repo.path, kept, "on a\nusb drive")
        assert flags()["kept"] == ("on a\nusb drive", False)
        with pytest.raises(GitError):
            gitutil.worktree_lock(repo.path, kept)
        gitutil.worktree_unlock(repo.path, kept)
        gitutil.worktree_lock(repo.path, kept)
        assert flags()["kept"] == ("", False)
        gitutil.worktree_unlock(repo.path, kept)
        with pytest.raises(GitError):
            gitutil.worktree_unlock(repo.path, kept)

        shutil.rmtree(gone)
        assert flags() == {"kept": (None, False), "gone": (None, True)}
        gitutil.worktree_prune(repo.path)
        assert flags() == {"kept": (None, False)}

    def test_checkout(self, repo: Repo) -> None:
        repo.add_branch("other")
        gitutil.checkout(repo.path, "other")
        assert gitutil.current_branch(repo.path) == "other"

    def test_git_error_carries_command_and_stderr(self, repo: Repo) -> None:
        with pytest.raises(GitError, match="git checkout no-such-branch"):
            gitutil.checkout(repo.path, "no-such-branch")
