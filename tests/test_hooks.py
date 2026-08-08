"""hooks: symlinks + git-status invisibility, setup scripts, named scripts."""

import subprocess
from pathlib import Path

import pytest

from workforest import gitutil, hooks
from workforest.config import Config
from workforest.errors import WorkforestError

from .conftest import Repo


def make_worktree(repo: Repo, name: str = "feat") -> Path:
    target = repo.path.parent / "worktrees" / repo.path.name / name
    gitutil.worktree_add(repo.path, target, name)
    return target


class TestScriptEnv:
    def test_wf_family_only(self, tmp_path: Path) -> None:
        env = hooks.script_env(
            main=tmp_path / "api",
            worktree=tmp_path / "wt" / "feat",
            worktrees_dir=tmp_path / "wt",
            branch="feature/x",
        )
        assert env["WF_MAIN"] == str(tmp_path / "api")
        assert env["WF_NAME"] == "api"
        assert env["WF_WORKTREE"] == str(tmp_path / "wt" / "feat")
        assert env["WF_WORKTREES_DIR"] == str(tmp_path / "wt")
        assert env["WF_BRANCH"] == "feature/x"
        # no MVP compat aliases
        assert "ROOT_TREE_PATH" not in env
        assert "WORK_TREE_PATH" not in env
        assert "WORKTREES_DIR" not in env

    def test_detached_branch_is_empty(self, tmp_path: Path) -> None:
        env = hooks.script_env(
            main=tmp_path, worktree=tmp_path, worktrees_dir=tmp_path, branch=None
        )
        assert env["WF_BRANCH"] == ""


class TestSymlinks:
    def test_creates_links_and_hides_them_from_git(self, repo: Repo) -> None:
        (repo.path / "node_modules").mkdir()
        (repo.path / ".env").write_text("SECRET=1\n")
        worktree = make_worktree(repo)
        cfg = Config(symlinks=["node_modules", ".env"])

        created = hooks.create_symlinks(cfg, main=repo.path, worktree=worktree)

        assert created == ["node_modules", ".env"]
        assert (worktree / "node_modules").is_symlink()
        assert (worktree / "node_modules").resolve() == repo.path / "node_modules"
        assert (worktree / ".env").read_text() == "SECRET=1\n"
        # invisible to git status in the worktree...
        assert gitutil.status_porcelain(worktree) == ""
        # ...but a plain untracked file still shows up
        repo.make_dirty(worktree=worktree)
        assert "dirty.txt" in gitutil.status_porcelain(worktree)

    def test_main_repo_status_unaffected(self, repo: Repo) -> None:
        (repo.path / ".env").write_text("x\n")
        worktree = make_worktree(repo)
        hooks.create_symlinks(Config(symlinks=[".env"]), main=repo.path, worktree=worktree)
        # .env is untracked in main and must stay visible there
        assert ".env" in gitutil.status_porcelain(repo.path)

    def test_missing_source_skipped(self, repo: Repo) -> None:
        worktree = make_worktree(repo)
        cfg = Config(symlinks=["does-not-exist"])
        assert hooks.create_symlinks(cfg, main=repo.path, worktree=worktree) == []

    def test_existing_file_not_clobbered(self, repo: Repo) -> None:
        (repo.path / ".env").write_text("main\n")
        worktree = make_worktree(repo)
        (worktree / ".env").write_text("precious\n")
        cfg = Config(symlinks=[".env"])
        assert hooks.create_symlinks(cfg, main=repo.path, worktree=worktree) == []
        assert (worktree / ".env").read_text() == "precious\n"

    def test_existing_symlink_replaced(self, repo: Repo) -> None:
        (repo.path / ".env").write_text("x\n")
        worktree = make_worktree(repo)
        (worktree / ".env").symlink_to(repo.path / "README.md")
        cfg = Config(symlinks=[".env"])
        assert hooks.create_symlinks(cfg, main=repo.path, worktree=worktree) == [".env"]
        assert (worktree / ".env").resolve() == repo.path / ".env"

    def test_nested_path_creates_parents(self, repo: Repo) -> None:
        (repo.path / ".vscode").mkdir()
        (repo.path / ".vscode" / "settings.json").write_text("{}\n")
        worktree = make_worktree(repo)
        cfg = Config(symlinks=[".vscode/settings.json"])
        created = hooks.create_symlinks(cfg, main=repo.path, worktree=worktree)
        assert created == [".vscode/settings.json"]
        assert (worktree / ".vscode" / "settings.json").is_symlink()
        assert gitutil.status_porcelain(worktree) == ""

    def test_global_excludes_seeded(self, repo: Repo, tmp_path: Path) -> None:
        global_ignore = tmp_path / "global-ignore"
        global_ignore.write_text("*.log\n")
        subprocess.run(
            ["git", "config", "--global", "core.excludesFile", str(global_ignore)],
            check=True,
        )
        (repo.path / ".env").write_text("x\n")
        worktree = make_worktree(repo)
        hooks.create_symlinks(Config(symlinks=[".env"]), main=repo.path, worktree=worktree)
        # the user's global ignores still apply inside the worktree
        (worktree / "noise.log").write_text("x\n")
        assert gitutil.status_porcelain(worktree) == ""


class TestSetupScripts:
    def test_scripts_run_in_worktree_with_env(self, repo: Repo, tmp_path: Path) -> None:
        worktree = make_worktree(repo)
        out = tmp_path / "hook-out.txt"
        cfg = Config(setup_scripts=[f'echo "$WF_BRANCH in $PWD" > {out}'])
        env = hooks.script_env(
            main=repo.path, worktree=worktree, worktrees_dir=worktree.parent, branch="feat"
        )
        failures = hooks.run_setup_scripts(cfg, worktree=worktree, env=env)
        assert failures == 0
        assert out.read_text() == f"feat in {worktree}\n"

    def test_failure_warns_but_continues(self, repo: Repo, tmp_path: Path) -> None:
        worktree = make_worktree(repo)
        out = tmp_path / "second.txt"
        cfg = Config(setup_scripts=["exit 7", f"touch {out}"])
        env = hooks.script_env(
            main=repo.path, worktree=worktree, worktrees_dir=worktree.parent, branch="feat"
        )
        failures = hooks.run_setup_scripts(cfg, worktree=worktree, env=env)
        assert failures == 1
        assert out.exists()  # the second script still ran


class TestNamedScripts:
    def test_runs_from_cwd_with_env(self, repo: Repo, tmp_path: Path) -> None:
        out = tmp_path / "run-out.txt"
        cfg = Config(scripts={"record": f'echo "$WF_MAIN|$WF_NAME|$PWD" > {out}'})
        env = hooks.script_env(
            main=repo.path, worktree=repo.path, worktrees_dir=tmp_path, branch="main"
        )
        hooks.run_named_script(cfg, "record", cwd=repo.path, env=env)
        assert out.read_text() == f"{repo.path}|{repo.path.name}|{repo.path}\n"

    def test_unknown_name_lists_available(self, repo: Repo) -> None:
        cfg = Config(scripts={"a": "true", "b": "true"})
        with pytest.raises(WorkforestError, match=r"no script named 'nope' \(available: a, b\)"):
            hooks.run_named_script(cfg, "nope", cwd=repo.path, env={})

    def test_failing_script_raises_with_code(self, repo: Repo) -> None:
        cfg = Config(scripts={"boom": "exit 3"})
        env = hooks.script_env(
            main=repo.path, worktree=repo.path, worktrees_dir=repo.path, branch=None
        )
        with pytest.raises(WorkforestError, match="exit code 3"):
            hooks.run_named_script(cfg, "boom", cwd=repo.path, env=env)
