"""Locked and stale worktrees: no command may crash on one, run git inside
one that is not really a worktree, or pass git's own `fatal:` on. Real
throwaway repositories throughout — the behaviour under test is git's."""

import json
import shutil
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import pytest

from workforest import commands, completions, gitutil
from workforest.errors import GitError, WorkforestError

from .conftest import CliResult, Repo

type Run = Callable[..., CliResult]


def ctx_for(repo: Repo) -> commands.Context:
    return commands.build_context(repo.path)


@dataclass(slots=True, frozen=True)
class Forest:
    """One worktree per state, named for it."""

    repo: Repo
    ctx: commands.Context

    def path(self, name: str) -> Path:
        return self.ctx.worktrees_dir / name

    def names(self) -> list[str]:
        return [w.name for w in commands.managed_worktrees(self.ctx)]


def make_stale(path: Path, *, keep_directory: bool = False) -> None:
    if keep_directory:
        (path / ".git").unlink()
    else:
        shutil.rmtree(path)


@pytest.fixture
def forest(repo: Repo) -> Forest:
    """live · held (locked, no reason) · gone (directory removed) · nogit
    (directory kept, `.git` removed) · zombie (locked, then removed)."""
    ctx = ctx_for(repo)
    for name in ("live", "held", "gone", "nogit", "zombie"):
        commands.cmd_create(ctx, name, no_open=True)
    forest = Forest(repo, ctx)
    commands.cmd_lock(ctx, "held")
    commands.cmd_lock(ctx, "zombie", reason="on the\nusb\tdrive")
    make_stale(forest.path("gone"))
    make_stale(forest.path("nogit"), keep_directory=True)
    make_stale(forest.path("zombie"))
    return forest


def message(excinfo: pytest.ExceptionInfo[WorkforestError]) -> str:
    text = str(excinfo.value)
    assert "fatal:" not in text and "git worktree" not in text
    return text


class TestList:
    def test_human_listing_names_every_state(self, forest: Forest) -> None:
        out = commands.cmd_list(forest.ctx)
        assert isinstance(out, str)
        states = {line.split()[0]: " ".join(line.split()[2:-1]) for line in out.splitlines()}
        assert states == {
            "live": "clean",
            "held": "clean locked",
            "gone": "stale",
            "nogit": "stale",
            "zombie": "stale locked",
        }

    def test_dirty_and_locked(self, forest: Forest) -> None:
        forest.repo.make_dirty(worktree=forest.path("held"))
        out = commands.cmd_list(forest.ctx)
        assert isinstance(out, str)
        assert "dirty locked" in out

    def test_porcelain_is_six_columns_one_record_per_line(self, forest: Forest) -> None:
        out = commands.cmd_list(forest.ctx, porcelain=True)
        assert isinstance(out, str)
        rows = {line.split("\t")[0]: line.split("\t") for line in out.split("\n")}
        assert set(rows) == {"live", "held", "gone", "nogit", "zombie"}
        assert all(len(row) == 6 for row in rows.values())
        assert rows["live"][3:] == ["0", "", ""]
        assert rows["held"][3:] == ["0", "locked", ""]
        # stale: dirty was never asked; the reason's wording is git's own
        assert rows["gone"][3:5] == ["", ""]
        assert rows["gone"][5].startswith("prunable")
        assert rows["nogit"][5].startswith("prunable")
        # a reason with a newline and a tab is flattened, not split
        assert rows["zombie"][3:5] == ["", "locked on the usb drive"]
        assert rows["zombie"][5].startswith("prunable ")

    def test_json_carries_both_flags_and_never_fails(self, forest: Forest) -> None:
        data = json.loads(commands.cmd_list(forest.ctx, as_json=True) or "")
        assert (data["main"]["locked"], data["main"]["prunable"]) == (None, None)
        rows: dict[str, dict[str, Any]] = {w["name"]: w for w in data["worktrees"]}
        assert (rows["live"]["dirty"], rows["live"]["locked"], rows["live"]["prunable"]) == (
            False,
            None,
            None,
        )
        assert (rows["held"]["dirty"], rows["held"]["locked"]) == (False, "")
        for name in ("gone", "nogit", "zombie"):
            assert rows[name]["dirty"] is None
            assert isinstance(rows[name]["prunable"], str)
        # the raw reason: JSON has no line protocol to protect
        assert rows["zombie"]["locked"] == "on the\nusb\tdrive"

    def test_nested_stale_directory_never_reports_the_main_checkout(self, repo: Repo) -> None:
        """With the worktrees inside the main checkout, `git status` in a
        directory that lost its `.git` file answers for the main checkout —
        silently wrong, not a crash."""
        repo.write_project_config("worktrees_dir: wt\n")
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        make_stale(ctx.worktrees_dir / "feat", keep_directory=True)
        repo.make_dirty()  # the main checkout
        row = json.loads(commands.cmd_list(ctx, as_json=True) or "")["worktrees"][0]
        assert row["dirty"] is None and row["prunable"] is not None
        out = commands.cmd_list(ctx)
        assert isinstance(out, str) and "stale" in out and "dirty" not in out

    def test_status_is_never_asked_of_a_stale_worktree(
        self, forest: Forest, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        asked: list[str] = []
        real = gitutil.status_porcelain

        def spy(path: Path) -> str:
            asked.append(path.name)
            return real(path)

        monkeypatch.setattr(gitutil, "status_porcelain", spy)
        commands.cmd_list(forest.ctx)
        assert sorted(asked) == ["held", "live"]

    def test_worktree_vanishing_mid_listing_reads_as_stale(
        self, forest: Forest, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        def gone(path: Path) -> str:
            raise FileNotFoundError(str(path))

        monkeypatch.setattr(gitutil, "status_porcelain", gone)
        data = json.loads(commands.cmd_list(forest.ctx, as_json=True) or "")
        assert all(w["dirty"] is None and w["prunable"] for w in data["worktrees"])

    def test_cli_exits_zero(self, forest: Forest, run_cli: Run) -> None:
        for flags in ((), ("--porcelain",), ("--json",)):
            result = run_cli("list", *flags, cwd=forest.repo.path)
            assert result.code == 0, result.err
            assert "Traceback" not in result.err


class TestOpen:
    def test_stale_is_refused_with_the_fix(self, forest: Forest) -> None:
        with pytest.raises(WorkforestError) as excinfo:
            commands.cmd_open(forest.ctx, "gone")
        assert message(excinfo) == ("worktree 'gone' is stale — run: wf prune, then wf create gone")

    def test_directory_without_git_file_is_stale_too(self, forest: Forest) -> None:
        with pytest.raises(WorkforestError, match="is stale"):
            commands.cmd_open(forest.ctx, "nogit")

    def test_stale_and_locked_names_unlock_first(self, forest: Forest) -> None:
        with pytest.raises(WorkforestError) as excinfo:
            commands.cmd_open(forest.ctx, "zombie")
        assert message(excinfo) == (
            "worktree 'zombie' is stale and locked — run: wf unlock zombie, then wf prune"
        )

    def test_lock_never_blocks_opening(self, forest: Forest) -> None:
        action = commands.cmd_open(forest.ctx, "held")
        assert action is not None

    def test_cli_exit_code_and_no_directive(self, forest: Forest, run_cli: Run) -> None:
        result = run_cli("open", "gone", cwd=forest.repo.path)
        assert result.code == 1
        assert result.out == ""
        assert result.err.startswith("Error: worktree 'gone' is stale")


class TestDelete:
    def test_locked_is_refused_even_with_force(self, forest: Forest) -> None:
        with pytest.raises(WorkforestError) as excinfo:
            commands.cmd_delete(forest.ctx, ["held"], force=True)
        assert message(excinfo) == "worktree 'held' is locked — run: wf unlock held"
        assert forest.path("held").is_dir()

    def test_locked_message_carries_the_reason_on_one_line(self, forest: Forest) -> None:
        with pytest.raises(WorkforestError) as excinfo:
            commands.cmd_delete(forest.ctx, ["zombie"])
        assert message(excinfo) == (
            "worktree 'zombie' is locked (on the usb drive) — run: wf unlock zombie"
        )

    def test_a_lock_anywhere_in_the_batch_deletes_nothing(self, forest: Forest) -> None:
        with pytest.raises(WorkforestError, match="'held' is locked"):
            commands.cmd_delete(forest.ctx, ["live", "held"], force=True)
        assert forest.path("live").is_dir()
        assert "live" in forest.names()

    def test_stale_prunes_only_that_record(
        self, forest: Forest, capsys: pytest.CaptureFixture[str]
    ) -> None:
        capsys.readouterr()
        assert commands.cmd_delete(forest.ctx, ["gone"], delete_branch=False) is None
        assert capsys.readouterr().err == "pruned stale worktree 'gone'\n"
        # the other stale record is not swept along
        assert forest.names() == ["held", "live", "nogit", "zombie"]

    def test_stale_can_take_the_branch_along(self, forest: Forest) -> None:
        commands.cmd_delete(forest.ctx, ["gone"], delete_branch=True)
        assert not gitutil.branch_exists("gone", forest.repo.path)

    def test_stale_with_directory_left_keeps_the_files_and_says_so(
        self, forest: Forest, capsys: pytest.CaptureFixture[str]
    ) -> None:
        (forest.path("nogit") / "work.txt").write_text("mine\n")
        capsys.readouterr()
        commands.cmd_delete(forest.ctx, ["nogit"], delete_branch=False)
        err = capsys.readouterr().err
        assert (forest.path("nogit") / "work.txt").read_text() == "mine\n"
        assert f"left {forest.path('nogit')} in place" in err
        # git can only clear this one repository-wide: what else went is named
        assert "also pruned the other stale worktree records: gone" in err
        assert forest.names() == ["held", "live", "zombie"]

    def test_batch_survives_a_record_pruned_along_the_way(self, forest: Forest) -> None:
        commands.cmd_delete(forest.ctx, ["nogit", "gone", "live"], delete_branch=False)
        assert forest.names() == ["held", "zombie"]

    def test_locked_behind_our_back_is_still_our_message(
        self, forest: Forest, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """Locked between the check and the removal: git refuses, and its
        text must not be what the user reads."""
        real = gitutil.worktree_remove

        def lock_first(repo: Path, path: Path, *, force: bool = False) -> None:
            gitutil.worktree_lock(repo, path, "late")
            real(repo, path, force=force)

        monkeypatch.setattr(gitutil, "worktree_remove", lock_first)
        with pytest.raises(WorkforestError) as excinfo:
            commands.cmd_delete(forest.ctx, ["live"])
        assert message(excinfo) == "worktree 'live' is locked (late) — run: wf unlock live"

    def test_stale_one_locked_behind_our_back_is_not_reported_pruned(
        self, forest: Forest, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        real = gitutil.worktree_prune

        def lock_first(repo: Path) -> None:
            gitutil.worktree_lock(repo, forest.path("nogit"), "late")
            real(repo)

        monkeypatch.setattr(gitutil, "worktree_prune", lock_first)
        with pytest.raises(WorkforestError) as excinfo:
            commands.cmd_delete(forest.ctx, ["nogit"])
        assert message(excinfo) == "worktree 'nogit' is locked (late) — run: wf unlock nogit"
        assert "nogit" in forest.names()

    def test_other_git_failures_are_not_swallowed(
        self, forest: Forest, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        def fail(repo: Path, path: Path, *, force: bool = False) -> None:
            raise GitError("boom")

        monkeypatch.setattr(gitutil, "worktree_remove", fail)
        with pytest.raises(GitError, match="boom"):
            commands.cmd_delete(forest.ctx, ["live"])


class TestCheckout:
    def test_stale_is_refused(self, forest: Forest) -> None:
        with pytest.raises(WorkforestError) as excinfo:
            commands.cmd_checkout(forest.ctx, "gone")
        assert message(excinfo) == "worktree 'gone' is stale — run: wf prune"

    def test_locked_is_refused_even_with_force(self, forest: Forest) -> None:
        with pytest.raises(WorkforestError) as excinfo:
            commands.cmd_checkout(forest.ctx, "held", force=True)
        assert message(excinfo) == "worktree 'held' is locked — run: wf unlock held"
        assert forest.path("held").is_dir()

    def test_stale_and_locked(self, forest: Forest) -> None:
        with pytest.raises(WorkforestError, match="stale and locked — run: wf unlock zombie"):
            commands.cmd_checkout(forest.ctx, "zombie")


class TestCreate:
    def test_stale_record_is_pruned_and_the_worktree_recreated(
        self, forest: Forest, capsys: pytest.CaptureFixture[str]
    ) -> None:
        capsys.readouterr()
        commands.cmd_create(forest.ctx, "gone", no_open=True)
        err = capsys.readouterr().err
        assert f"pruned the stale worktree record at {forest.path('gone')}" in err
        assert "fatal:" not in err
        assert (forest.path("gone") / ".git").exists()

    def test_stale_and_locked_record_is_kept(self, forest: Forest) -> None:
        with pytest.raises(WorkforestError) as excinfo:
            commands.cmd_create(forest.ctx, "zombie", no_open=True)
        assert "stale and locked — run: wf unlock zombie, then wf prune" in message(excinfo)
        assert "zombie" in forest.names()

    def test_directory_left_behind_is_not_overwritten(self, forest: Forest) -> None:
        with pytest.raises(WorkforestError, match="directory exists but is not a worktree"):
            commands.cmd_create(forest.ctx, "nogit", no_open=True)
        assert forest.path("nogit").is_dir()

    def test_stale_record_of_another_branch_at_the_same_path(self, forest: Forest) -> None:
        # fix/gone would land in the directory the stale `gone` record names
        commands.cmd_create(forest.ctx, "fix/gone", no_open=True)
        branches = {w.name: w.branch for w in commands.managed_worktrees(forest.ctx)}
        assert branches["gone"] == "fix/gone"

    def test_locked_stale_record_outside_the_worktrees_dir(
        self, repo: Repo, tmp_path: Path
    ) -> None:
        elsewhere = tmp_path / "elsewhere" / "feat"
        gitutil.worktree_add(repo.path, elsewhere, "feat")
        gitutil.worktree_lock(repo.path, elsewhere)
        shutil.rmtree(elsewhere)
        with pytest.raises(WorkforestError, match=f"record at {elsewhere} is in the way"):
            commands.cmd_create(ctx_for(repo), "feat", no_open=True)


class TestLockUnlock:
    def test_round_trip(self, repo: Repo, capsys: pytest.CaptureFixture[str]) -> None:
        ctx = ctx_for(repo)
        commands.cmd_create(ctx, "feat", no_open=True)
        capsys.readouterr()
        assert commands.cmd_lock(ctx, "feat", reason="keep") is None
        assert capsys.readouterr().err == "locked worktree 'feat'\n"
        assert commands.find_managed(ctx, "feat").locked == "keep"
        assert commands.cmd_unlock(ctx, "feat") is None
        assert capsys.readouterr().err == "unlocked worktree 'feat'\n"
        assert commands.find_managed(ctx, "feat").locked is None

    def test_locking_twice_is_an_error_and_keeps_the_reason(self, forest: Forest) -> None:
        with pytest.raises(WorkforestError) as excinfo:
            commands.cmd_lock(forest.ctx, "zombie", reason="another")
        assert message(excinfo).startswith("worktree 'zombie' is already locked (on the usb drive)")
        assert commands.find_managed(forest.ctx, "zombie").locked == "on the\nusb\tdrive"

    def test_unlocking_an_unlocked_one_is_an_error(self, forest: Forest) -> None:
        with pytest.raises(WorkforestError) as excinfo:
            commands.cmd_unlock(forest.ctx, "live")
        assert message(excinfo) == "worktree 'live' is not locked"

    def test_main_checkout_cannot_be_locked(self, forest: Forest) -> None:
        with pytest.raises(WorkforestError) as excinfo:
            commands.cmd_lock(forest.ctx, "api")
        assert "not found" in message(excinfo)

    def test_a_stale_record_can_be_locked_and_unlocked(self, forest: Forest) -> None:
        commands.cmd_lock(forest.ctx, "gone", reason="drive is away")
        assert commands.find_managed(forest.ctx, "gone").locked == "drive is away"
        commands.cmd_unlock(forest.ctx, "zombie")
        assert commands.find_managed(forest.ctx, "zombie").locked is None

    def test_lock_raced_by_another_lock(
        self, forest: Forest, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        real = gitutil.worktree_lock

        def twice(repo: Path, path: Path, reason: str | None = None) -> None:
            real(repo, path, "theirs")
            real(repo, path, reason)

        monkeypatch.setattr(gitutil, "worktree_lock", twice)
        with pytest.raises(WorkforestError) as excinfo:
            commands.cmd_lock(forest.ctx, "live", reason="ours")
        assert message(excinfo).startswith("worktree 'live' is already locked (theirs)")

    def test_unlock_raced_by_another_unlock(
        self, forest: Forest, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        real = gitutil.worktree_unlock

        def twice(repo: Path, path: Path) -> None:
            real(repo, path)
            real(repo, path)

        monkeypatch.setattr(gitutil, "worktree_unlock", twice)
        with pytest.raises(WorkforestError) as excinfo:
            commands.cmd_unlock(forest.ctx, "held")
        assert message(excinfo) == "worktree 'held' is not locked"

    def test_unrelated_git_failures_pass_through(
        self, forest: Forest, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        def fail(*args: object, **kwargs: object) -> None:
            raise GitError("boom")

        monkeypatch.setattr(gitutil, "worktree_lock", fail)
        monkeypatch.setattr(gitutil, "worktree_unlock", fail)
        with pytest.raises(GitError, match="boom"):
            commands.cmd_lock(forest.ctx, "live")
        with pytest.raises(GitError, match="boom"):
            commands.cmd_unlock(forest.ctx, "held")

    def test_cli(self, forest: Forest, run_cli: Run) -> None:
        result = run_cli("lock", "live", "--reason", "mine", cwd=forest.repo.path)
        assert (result.code, result.out, result.err) == (0, "", "locked worktree 'live'\n")
        result = run_cli("lock", "live", cwd=forest.repo.path)
        assert result.code == 1 and "already locked (mine)" in result.err
        result = run_cli("unlock", "live", cwd=forest.repo.path)
        assert (result.code, result.out, result.err) == (0, "", "unlocked worktree 'live'\n")


class TestPrune:
    def test_dry_run_changes_nothing(
        self, forest: Forest, capsys: pytest.CaptureFixture[str]
    ) -> None:
        before = forest.names()
        capsys.readouterr()
        assert commands.cmd_prune(forest.ctx, dry_run=True) is None
        assert capsys.readouterr().err == (
            "would prune 2 stale worktree records: gone, nogit\n"
            "1 stale worktree record is locked: zombie — unlock to prune\n"
        )
        assert forest.names() == before

    def test_prunes_and_names_everything_it_removed_or_kept(
        self, forest: Forest, capsys: pytest.CaptureFixture[str]
    ) -> None:
        capsys.readouterr()
        commands.cmd_prune(forest.ctx)
        err = capsys.readouterr().err.splitlines()
        assert err[0] == "pruned 2 stale worktree records: gone, nogit"
        assert err[1].startswith(f"left {forest.path('nogit')} in place")
        assert err[2] == "1 stale worktree record is locked: zombie — unlock to prune"
        assert forest.names() == ["held", "live", "zombie"]
        assert forest.path("nogit").is_dir()

    def test_locked_stale_record_goes_once_unlocked(
        self, forest: Forest, capsys: pytest.CaptureFixture[str]
    ) -> None:
        commands.cmd_prune(forest.ctx)
        commands.cmd_unlock(forest.ctx, "zombie")
        capsys.readouterr()
        commands.cmd_prune(forest.ctx)
        assert capsys.readouterr().err == "pruned 1 stale worktree record: zombie\n"
        assert forest.names() == ["held", "live"]

    def test_only_locked_ones_left_is_not_nothing_to_prune(
        self, forest: Forest, capsys: pytest.CaptureFixture[str]
    ) -> None:
        commands.cmd_prune(forest.ctx)
        capsys.readouterr()
        commands.cmd_prune(forest.ctx)
        assert capsys.readouterr().err == (
            "1 stale worktree record is locked: zombie — unlock to prune\n"
        )

    def test_nothing_to_prune_is_not_an_error(self, repo: Repo, run_cli: Run) -> None:
        for flags in ((), ("-n",), ("--dry-run",)):
            result = run_cli("prune", *flags, cwd=repo.path)
            assert (result.code, result.out, result.err) == (0, "", "no stale worktree records\n")

    def test_records_outside_the_worktrees_dir_are_named_by_path(
        self, repo: Repo, tmp_path: Path, capsys: pytest.CaptureFixture[str]
    ) -> None:
        elsewhere = tmp_path / "elsewhere" / "feat"
        gitutil.worktree_add(repo.path, elsewhere, "feat")
        shutil.rmtree(elsewhere)
        capsys.readouterr()
        commands.cmd_prune(ctx_for(repo))
        assert capsys.readouterr().err == f"pruned 1 stale worktree record: {elsewhere}\n"


class TestCompletions:
    def complete(self, forest: Forest, topic: str, monkeypatch: pytest.MonkeyPatch) -> list[str]:
        monkeypatch.chdir(forest.repo.path)
        return completions.complete(topic)

    def test_stale_records_do_not_empty_the_candidates(
        self, forest: Forest, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        assert self.complete(forest, "worktrees", monkeypatch) == [
            "gone",
            "held",
            "live",
            "nogit",
            "zombie",
        ]

    def test_lock_offers_only_the_unlocked(
        self, forest: Forest, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        assert self.complete(forest, "lockable", monkeypatch) == ["gone", "live", "nogit"]

    def test_unlock_offers_only_the_locked_one_line_each(
        self, forest: Forest, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        assert self.complete(forest, "unlockable", monkeypatch) == [
            "held\tlocked",
            "zombie\ton the usb drive",
        ]

    def test_new_commands_are_listed(self, forest: Forest, monkeypatch: pytest.MonkeyPatch) -> None:
        names = [line.split("\t")[0] for line in self.complete(forest, "commands", monkeypatch)]
        assert {"lock", "unlock", "prune"} <= set(names)
