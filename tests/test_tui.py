"""TUI: pure helper wiring + the fzf-missing error path. The fzf subprocess
itself is the only untested seam (manual checklist)."""

from pathlib import Path

import pytest

from workforest import commands, tui
from workforest.config import Config
from workforest.errors import WorkforestError

from .conftest import Repo


class TestPureHelpers:
    def test_header_marks_current_mode(self) -> None:
        header = tui.build_header(("create", "open"), "open")
        assert header == " CREATE  │ [OPEN]"

    def test_opener_line_marks_current(self) -> None:
        line = tui.build_opener_line(["edit", "shell"], 0)
        assert line == "  [edit] |  shell "

    def test_cycle_wraps_both_directions(self) -> None:
        assert tui.cycle(3, 0, -1) == 2
        assert tui.cycle(3, 2, +1) == 0
        assert tui.cycle(3, 1, +1) == 2

    def test_mode_has_opener(self) -> None:
        assert tui.mode_has_opener("create")
        assert tui.mode_has_opener("open")
        assert not tui.mode_has_opener("delete")
        assert not tui.mode_has_opener("checkout")

    def test_parse_fzf_output(self) -> None:
        result = tui.parse_fzf_output("query\nalt-l\nselection")
        assert (result.query, result.key, result.selection) == ("query", "alt-l", "selection")
        result = tui.parse_fzf_output("typed\n\n")
        assert (result.query, result.key, result.selection) == ("typed", "", "")

    def test_fzf_args_create_accepts_query(self) -> None:
        args = tui._fzf_args("create", None)
        assert "--bind=enter:accept-or-print-query" in args
        args = tui._fzf_args("open", "  [edit]")
        assert "--bind=enter:accept" in args
        assert any(a.startswith("--preview=") for a in args)


class TestCarousel:
    def test_config_openers_win(self, repo: Repo) -> None:
        ctx = commands.build_context(repo.path)
        ctx.config = Config(openers={"edit": "$EDITOR {target}", "git": "lazygit"})
        assert tui.opener_carousel(ctx) == [("edit", "edit"), ("git", "git")]

    def test_derived_fallback_pair(self, repo: Repo) -> None:
        ctx = commands.build_context(repo.path)
        # SHELL is pinned to /bin/sh by the isolation fixture
        assert tui.opener_carousel(ctx) == [("edit", None), ("shell", "/bin/sh")]

    def test_fallback_without_shell(self, repo: Repo, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.delenv("SHELL")
        ctx = commands.build_context(repo.path)
        assert tui.opener_carousel(ctx) == [("edit", None)]


class TestModesAndCandidates:
    def test_base_modes_without_claude(self, repo: Repo) -> None:
        ctx = commands.build_context(repo.path)
        assert tui.available_modes(ctx) == ("create", "open", "checkout", "delete")

    def test_claude_mode_only_in_non_main_worktree(self, repo: Repo) -> None:
        (Path.home() / ".claude").mkdir()
        ctx = commands.build_context(repo.path)
        assert "claude" not in tui.available_modes(ctx)  # main worktree
        commands.cmd_create(ctx, "feat", no_open=True)
        inner = commands.build_context(ctx.worktrees_dir / "feat")
        assert tui.available_modes(inner) == ("create", "open", "checkout", "delete", "claude")

    def test_create_candidates_are_branches(
        self, repo: Repo, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        repo.add_branch("free-branch")
        monkeypatch.chdir(repo.path)
        ctx = commands.build_context(repo.path)
        assert "free-branch" in tui.candidates(ctx, "create")

    def test_other_candidates_are_worktrees(self, repo: Repo) -> None:
        ctx = commands.build_context(repo.path)
        commands.cmd_create(ctx, "feat", no_open=True)
        for mode in ("open", "checkout", "delete"):
            assert tui.candidates(ctx, mode) == ["feat"]


class TestExecute:
    def test_create_and_open_pass_opener(self, repo: Repo) -> None:
        ctx = commands.build_context(repo.path)
        action = tui._execute(ctx, "create", "feat", "my-opener")
        assert action is not None
        assert "my-opener" in action.script  # type: ignore[union-attr]
        action = tui._execute(ctx, "open", "feat", None)
        assert action is not None

    def test_delete_and_checkout(self, repo: Repo) -> None:
        ctx = commands.build_context(repo.path)
        tui._execute(ctx, "create", "feat", "true")
        assert tui._execute(ctx, "delete", "feat", None) is None
        tui._execute(ctx, "create", "feat2", "true")
        action = tui._execute(ctx, "checkout", "feat2", None)
        assert action is not None
        assert action.script == f"cd {repo.path}"  # type: ignore[union-attr]


class TestFzfGate:
    def test_missing_fzf_is_actionable(self, repo: Repo, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.chdir(repo.path)
        monkeypatch.setattr("shutil.which", lambda _: None)
        with pytest.raises(WorkforestError, match="requires fzf"):
            tui.run(None)
