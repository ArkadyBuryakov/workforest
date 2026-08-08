"""Claude integration against a fabricated ~/.claude tree (HOME is
redirected by the isolation fixture)."""

import json
from collections.abc import Callable
from pathlib import Path

import pytest

from workforest import commands
from workforest.errors import WorkforestError
from workforest.integrations import claude

from .conftest import CliResult, Repo

Run = Callable[..., CliResult]


def seed_session(
    main: Path,
    session_id: str = "abc-123",
    *,
    display: str | None = "fix the login bug",
    spaced_json: bool = False,
) -> Path:
    """Create a session file (and history entry) for the main worktree."""
    project = claude.project_dir(main)
    project.mkdir(parents=True, exist_ok=True)
    if spaced_json:
        lines = [
            f'{{"cwd": "{main}", "type": "user", "text": "hello"}}',
            '{"type": "meta"}',
        ]
    else:
        lines = [
            json.dumps({"cwd": str(main), "type": "user", "text": "hello"}),
            json.dumps({"type": "meta"}),
            "not-json-at-all",
        ]
    session = project / f"{session_id}.jsonl"
    session.write_text("\n".join(lines) + "\n")
    if display is not None:
        history = claude.claude_dir() / "history.jsonl"
        entry = {"sessionId": session_id, "project": str(main), "display": display}
        with history.open("a") as fh:
            fh.write(json.dumps(entry) + "\n")
    return session


class TestAvailability:
    def test_gated_on_home_dir(self) -> None:
        assert not claude.available()
        claude.claude_dir().mkdir()
        assert claude.available()

    def test_project_dir_encoding(self) -> None:
        assert claude.project_dir(Path("/home/u/dev/api.v2")).name == "-home-u-dev-api-v2"


class TestListing:
    def test_lists_sessions_with_descriptions(self, repo: Repo) -> None:
        seed_session(repo.path, "s1", display="short one")
        seed_session(repo.path, "s2", display="x" * 100)
        seed_session(repo.path, "s3", display=None)
        sessions = dict(claude.list_sessions(repo.path))
        assert sessions["s1"] == "short one"
        assert len(sessions["s2"]) == 80 and sessions["s2"].endswith("...")
        assert sessions["s3"] == "(no description)"

    def test_empty_without_project_dir(self, repo: Repo) -> None:
        claude.claude_dir().mkdir()
        assert claude.list_sessions(repo.path) == []

    def test_new_sessions_filter_already_copied(self, repo: Repo) -> None:
        ctx = commands.build_context(repo.path)
        commands.cmd_create(ctx, "feat", no_open=True)
        worktree = ctx.worktrees_dir / "feat"
        seed_session(repo.path, "old")
        seed_session(repo.path, "new")
        copied = claude.project_dir(worktree)
        copied.mkdir(parents=True)
        (copied / "old.jsonl").write_text("{}\n")
        assert [sid for sid, _ in claude.list_new_sessions(repo.path, worktree)] == ["new"]


class TestCopySession:
    def make_worktree(self, repo: Repo) -> Path:
        ctx = commands.build_context(repo.path)
        commands.cmd_create(ctx, "feat", no_open=True)
        return ctx.worktrees_dir / "feat"

    def test_rewrites_compact_cwd(self, repo: Repo) -> None:
        worktree = self.make_worktree(repo)
        seed_session(repo.path)
        claude.copy_session("abc-123", main=repo.path, current=worktree)
        copied = claude.project_dir(worktree) / "abc-123.jsonl"
        lines = copied.read_text().splitlines()
        assert json.loads(lines[0])["cwd"] == str(worktree)
        assert json.loads(lines[1]) == {"type": "meta"}  # untouched
        assert lines[2] == "not-json-at-all"  # non-JSON copied verbatim

    def test_rewrites_spaced_json(self, repo: Repo) -> None:
        worktree = self.make_worktree(repo)
        seed_session(repo.path, "sp", spaced_json=True)
        claude.copy_session("sp", main=repo.path, current=worktree)
        copied = claude.project_dir(worktree) / "sp.jsonl"
        assert json.loads(copied.read_text().splitlines()[0])["cwd"] == str(worktree)

    def test_appends_rewritten_history_entry(self, repo: Repo) -> None:
        worktree = self.make_worktree(repo)
        seed_session(repo.path)
        claude.copy_session("abc-123", main=repo.path, current=worktree)
        entries = [
            json.loads(line)
            for line in (claude.claude_dir() / "history.jsonl").read_text().splitlines()
        ]
        assert entries[-1]["project"] == str(worktree)
        assert entries[-1]["sessionId"] == "abc-123"
        assert entries[0]["project"] == str(repo.path)  # original kept

    def test_copies_session_assets_dir(self, repo: Repo) -> None:
        worktree = self.make_worktree(repo)
        seed_session(repo.path)
        assets = claude.project_dir(repo.path) / "abc-123"
        assets.mkdir()
        (assets / "note.txt").write_text("asset\n")
        claude.copy_session("abc-123", main=repo.path, current=worktree)
        assert (claude.project_dir(worktree) / "abc-123" / "note.txt").read_text() == "asset\n"

    def test_already_copied_warns_not_errors(self, repo: Repo) -> None:
        worktree = self.make_worktree(repo)
        seed_session(repo.path)
        claude.copy_session("abc-123", main=repo.path, current=worktree)
        claude.copy_session("abc-123", main=repo.path, current=worktree)  # no raise

    def test_unknown_session_errors(self, repo: Repo) -> None:
        worktree = self.make_worktree(repo)
        claude.claude_dir().mkdir(exist_ok=True)
        with pytest.raises(WorkforestError, match="not found"):
            claude.copy_session("ghost", main=repo.path, current=worktree)


class TestCliWiring:
    def test_copy_session_via_cli(self, run_cli: Run, repo: Repo) -> None:
        ctx = commands.build_context(repo.path)
        commands.cmd_create(ctx, "feat", no_open=True)
        worktree = ctx.worktrees_dir / "feat"
        seed_session(repo.path)
        result = run_cli("claude", "copy-session", "abc-123", cwd=worktree)
        assert result.code == 0, result.err
        assert (claude.project_dir(worktree) / "abc-123.jsonl").is_file()

    def test_refused_from_main_worktree(self, run_cli: Run, repo: Repo) -> None:
        seed_session(repo.path)
        result = run_cli("claude", "copy-session", "abc-123", cwd=repo.path)
        assert result.code == 1
        assert "non-main worktree" in result.err

    def test_session_completion_topic(self, run_cli: Run, repo: Repo) -> None:
        ctx = commands.build_context(repo.path)
        commands.cmd_create(ctx, "feat", no_open=True)
        seed_session(repo.path)
        result = run_cli("--complete", "claude-sessions", cwd=ctx.worktrees_dir / "feat")
        assert result.out.splitlines() == ["abc-123"]
        # empty from the main worktree
        result = run_cli("--complete", "claude-sessions", cwd=repo.path)
        assert result.out == ""
