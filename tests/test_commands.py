"""commands: end-to-end flows against real throwaway repos."""

from collections.abc import Callable
from pathlib import Path

import pytest

from workforest import commands, gitutil
from workforest.errors import CancelledError, UsageError, WorkforestError
from workforest.launch import ShellAction

from .conftest import Repo


def ctx_for(repo: Repo, cwd: Path | None = None) -> commands.Context:
    return commands.build_context(cwd or repo.path)


class TestContext:
    def test_context_from_main(self, repo: Repo, tmp_path: Path) -> None:
        ctx = ctx_for(repo)
        assert ctx.main == repo.path
        assert ctx.repo_name == repo.path.name
        assert ctx.worktrees_dir == tmp_path / "dev" / "worktrees" / repo.path.name

    def test_context_from_inside_worktree(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        result = commands.cmd_create(ctx, "feat", no_open=True)
        assert result is None
        worktree_path = ctx.worktrees_dir / "feat"
        inner = commands.build_context(worktree_path)
        assert inner.main == repo.path
        assert inner.cwd_root == worktree_path

    def test_project_config_read_from_main_inside_worktree(self, repo: Repo) -> None:
        repo.write_project_config("opener: from-project\n")
        repo.commit("config")
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        inner = commands.build_context(ctx.worktrees_dir / "feat")
        assert inner.config.opener == "from-project"


class TestCreate:
    def test_new_branch(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        action = commands.cmd_create(ctx, "feature/cool-thing")
        worktree_path = ctx.worktrees_dir / "cool-thing"  # short name after last /
        assert worktree_path.is_dir()
        assert gitutil.current_branch(worktree_path) == "feature/cool-thing"
        assert isinstance(action, ShellAction)
        assert action.script == f"cd {worktree_path} && stub-editor"

    def test_existing_local_branch(self, repo: Repo) -> None:
        repo.add_branch("existing")
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "existing", no_open=True)
        assert gitutil.current_branch(ctx.worktrees_dir / "existing") == "existing"

    def test_remote_branch(self, make_repo: Callable[..., Repo]) -> None:
        repo = make_repo(origin=True)
        repo.add_branch("remote-feat", remote_only=True)
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "remote-feat", no_open=True)
        assert gitutil.current_branch(ctx.worktrees_dir / "remote-feat") == "remote-feat"

    def test_branch_already_in_worktree_reuses_it(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        action = commands.cmd_create(ctx, "feat")
        assert isinstance(action, ShellAction)
        assert str(ctx.worktrees_dir / "feat") in action.script

    def test_default_branch_is_current(self, repo: Repo) -> None:
        # current branch (main) is checked out in the main worktree → reuse
        ctx = ctx_for(repo)
        action = commands.cmd_create(ctx, None)
        assert isinstance(action, ShellAction)
        assert action.script == f"cd {repo.path} && stub-editor"

    def test_short_name_collision_errors(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat/x", no_open=True)
        with pytest.raises(WorkforestError, match="different branch"):
            commands.cmd_create(ctx, "fix/x", no_open=True)

    def test_existing_non_worktree_directory_errors(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        blocker = ctx.worktrees_dir / "feat"
        blocker.mkdir(parents=True)
        with pytest.raises(WorkforestError, match="not a worktree"):
            commands.cmd_create(ctx, "feat", no_open=True)

    def test_hooks_run_on_create(self, repo: Repo, tmp_path: Path) -> None:
        out = tmp_path / "hook-ran.txt"
        repo.write_project_config(
            f"symlinks: ['.env']\nsetup_scripts: ['echo $WF_BRANCH > {out}']\n"
        )
        repo.commit("config")
        (repo.path / ".env").write_text("X=1\n")  # untracked, like a real .env
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        worktree_path = ctx.worktrees_dir / "feat"
        assert (worktree_path / ".env").is_symlink()
        assert out.read_text() == "feat\n"
        assert gitutil.status_porcelain(worktree_path) == ""

    def test_no_hooks_skips_them(self, repo: Repo) -> None:
        repo.write_project_config("symlinks: ['.env']\n")
        repo.commit("config")
        (repo.path / ".env").write_text("X=1\n")
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True, no_hooks=True)
        assert not (ctx.worktrees_dir / "feat" / ".env").exists()


class TestOpen:
    def test_open_worktree_root(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        action = commands.cmd_open(ctx, "feat")
        assert isinstance(action, ShellAction)
        assert action.script == f"cd {ctx.worktrees_dir / 'feat'} && stub-editor"

    def test_open_subdirectory(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        (ctx.worktrees_dir / "feat" / "src").mkdir()
        action = commands.cmd_open(ctx, "feat", path_arg="src")
        assert isinstance(action, ShellAction)
        assert action.script == f"cd {ctx.worktrees_dir / 'feat' / 'src'} && stub-editor"

    def test_open_file_with_path_template(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        action = commands.cmd_open(ctx, "feat", opener="tool {path}", path_arg="README.md")
        assert isinstance(action, ShellAction)
        assert action.script == f"cd {ctx.worktrees_dir / 'feat'} && tool README.md"

    def test_open_unknown_errors(self, repo: Repo) -> None:
        with pytest.raises(WorkforestError, match="'nope' not found"):
            commands.cmd_open(ctx_for(repo), "nope")

    def test_open_without_name_is_usage_error(self, repo: Repo) -> None:
        with pytest.raises(UsageError, match="name required"):
            commands.cmd_open(ctx_for(repo), None)

    def test_main_worktree_is_not_openable_by_name(self, repo: Repo) -> None:
        # main is not managed; only worktrees inside worktrees_dir resolve
        with pytest.raises(WorkforestError, match="not found"):
            commands.cmd_open(ctx_for(repo), repo.path.name)


class TestList:
    def test_porcelain_format_is_stable(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feature/one", no_open=True)
        commands.cmd_create(ctx, "two", no_open=True)
        repo.make_dirty(worktree=ctx.worktrees_dir / "two")
        out = commands.cmd_list(ctx, porcelain=True)
        assert out == (
            f"one\tfeature/one\t{ctx.worktrees_dir / 'one'}\t0\n"
            f"two\ttwo\t{ctx.worktrees_dir / 'two'}\t1"
        )

    def test_human_listing(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        out = commands.cmd_list(ctx)
        assert isinstance(out, str)
        assert "feat" in out and "clean" in out

    def test_empty_forest(self, repo: Repo) -> None:
        assert commands.cmd_list(ctx_for(repo)) is None
        assert commands.cmd_list(ctx_for(repo), porcelain=True) == ""


class TestDelete:
    def test_clean_delete_keeps_branch_off_tty(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        commands.cmd_delete(ctx, ["feat"])
        assert not (ctx.worktrees_dir / "feat").exists()
        assert gitutil.branch_exists("feat", repo.path)  # kept by default

    def test_delete_branch_flag(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        commands.cmd_delete(ctx, ["feat"], delete_branch=True)
        assert not gitutil.branch_exists("feat", repo.path)

    def test_dirty_refused_off_tty_without_force(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        repo.make_dirty(worktree=ctx.worktrees_dir / "feat")
        with pytest.raises(CancelledError):
            commands.cmd_delete(ctx, ["feat"])
        assert (ctx.worktrees_dir / "feat").exists()

    def test_dirty_force(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        repo.make_dirty(worktree=ctx.worktrees_dir / "feat")
        commands.cmd_delete(ctx, ["feat"], force=True)
        assert not (ctx.worktrees_dir / "feat").exists()

    def test_dirty_confirmed_on_tty(self, repo: Repo, tty: Callable[[list[str]], None]) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        repo.make_dirty(worktree=ctx.worktrees_dir / "feat")
        tty(["y", "n"])  # yes delete dirty worktree, no keep the branch
        commands.cmd_delete(ctx, ["feat"])
        assert not (ctx.worktrees_dir / "feat").exists()
        assert gitutil.branch_exists("feat", repo.path)

    def test_dirty_declined_on_tty_cancels(
        self, repo: Repo, tty: Callable[[list[str]], None]
    ) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        repo.make_dirty(worktree=ctx.worktrees_dir / "feat")
        tty(["n"])
        with pytest.raises(CancelledError):
            commands.cmd_delete(ctx, ["feat"])

    def test_multiple_names(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "one", no_open=True)
        commands.cmd_create(ctx, "two", no_open=True)
        commands.cmd_delete(ctx, ["one", "two"])
        assert commands.managed_worktrees(ctx) == []

    def test_unknown_name(self, repo: Repo) -> None:
        with pytest.raises(WorkforestError, match="not found"):
            commands.cmd_delete(ctx_for(repo), ["ghost"])

    def test_delete_from_inside_returns_cd_to_main(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        inside = commands.build_context(ctx.worktrees_dir / "feat")
        action = commands.cmd_delete(inside, ["feat"])
        assert isinstance(action, ShellAction)
        assert action.script == f"cd {repo.path}"

    def test_delete_elsewhere_returns_nothing(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        assert commands.cmd_delete(ctx, ["feat"]) is None


class TestCheckout:
    def test_collapse_into_main(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        action = commands.cmd_checkout(ctx, "feat")
        assert not (ctx.worktrees_dir / "feat").exists()
        assert gitutil.current_branch(repo.path) == "feat"
        assert isinstance(action, ShellAction)
        assert action.script == f"cd {repo.path}"

    def test_dirty_needs_force_off_tty(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        repo.make_dirty(worktree=ctx.worktrees_dir / "feat")
        with pytest.raises(CancelledError):
            commands.cmd_checkout(ctx, "feat")
        commands.cmd_checkout(ctx, "feat", force=True)
        assert gitutil.current_branch(repo.path) == "feat"


class TestRun:
    def test_merged_scripts_and_env(self, repo: Repo, tmp_path: Path) -> None:
        out = tmp_path / "run.txt"
        repo.write_project_config(f'scripts:\n  probe: echo "$WF_MAIN|$WF_BRANCH" > {out}\n')
        repo.commit("config")
        ctx = ctx_for(repo)
        commands.cmd_run(ctx, "probe")
        assert out.read_text() == f"{repo.path}|main\n"

    def test_runs_from_worktree_root(self, repo: Repo, tmp_path: Path) -> None:
        out = tmp_path / "cwd.txt"
        repo.write_project_config(f"scripts:\n  where: pwd > {out}\n")
        repo.commit("config")
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        worktree_ctx = commands.build_context(ctx.worktrees_dir / "feat")
        commands.cmd_run(worktree_ctx, "where")
        assert out.read_text() == f"{ctx.worktrees_dir / 'feat'}\n"


class TestInit:
    def test_scaffolds_project_config(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        commands.cmd_init(ctx)
        scaffold = repo.path / ".workforest.yaml"
        assert scaffold.is_file()
        # the scaffold must itself be a valid config
        commands.build_context(repo.path)

    def test_refuses_overwrite(self, repo: Repo) -> None:
        ctx = ctx_for(repo)
        commands.cmd_init(ctx)
        with pytest.raises(WorkforestError, match="already exists"):
            commands.cmd_init(ctx)

    def test_local_into_vscode(self, repo: Repo) -> None:
        (repo.path / ".vscode").mkdir()
        commands.cmd_init(ctx_for(repo), local=True)
        assert (repo.path / ".vscode" / ".workforest.yaml").is_file()

    def test_local_without_settings_folder_errors(self, repo: Repo) -> None:
        with pytest.raises(WorkforestError, match="--local needs"):
            commands.cmd_init(ctx_for(repo), local=True)


class TestConfigShow:
    def test_shows_defaults_outside_repo(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        outside = tmp_path / "nowhere"
        outside.mkdir()
        monkeypatch.chdir(outside)
        out = commands.cmd_config_show()
        assert isinstance(out, str)
        assert "worktrees_dir: $WF_MAIN/../worktrees/$WF_NAME" in out
        assert "built-in defaults only" in out

    def test_json_with_sources(self, repo: Repo, monkeypatch: pytest.MonkeyPatch) -> None:
        import json

        repo.write_project_config("opener: proj\n")
        monkeypatch.chdir(repo.path)
        out = commands.cmd_config_show(as_json=True)
        assert isinstance(out, str)
        data = json.loads(out)
        assert data["config"]["opener"] == "proj"
        assert data["sources"][0][0] == "project"
