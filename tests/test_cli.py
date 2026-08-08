"""cli: stdout/stderr contract, exit codes, shortcut dispatch."""

from collections.abc import Callable
from pathlib import Path

from workforest import __version__

from .conftest import CliResult, Repo

Run = Callable[..., CliResult]


class TestBasics:
    def test_version(self, run_cli: Run) -> None:
        result = run_cli("--version")
        assert result.code == 0
        assert result.out.strip() == f"workforest {__version__}"

    def test_help_exits_zero(self, run_cli: Run) -> None:
        assert run_cli("--help").code == 0

    def test_usage_error_is_exit_2(self, run_cli: Run, repo: Repo) -> None:
        result = run_cli("delete", cwd=repo.path)  # missing NAME
        assert result.code == 2
        assert result.out == ""

    def test_outside_repo_is_operational_error(self, run_cli: Run, tmp_path: Path) -> None:
        outside = tmp_path / "outside"
        outside.mkdir()
        result = run_cli("list", cwd=outside)
        assert result.code == 1
        assert "Error: Not inside a git repository" in result.err
        assert result.out == ""

    def test_config_error_is_exit_4(self, run_cli: Run, repo: Repo) -> None:
        repo.write_project_config("bogus_key: 1\n")
        result = run_cli("list", cwd=repo.path)
        assert result.code == 4
        assert "unknown key" in result.err


class TestStdoutContract:
    def test_create_emits_only_cd_line(self, run_cli: Run, repo: Repo) -> None:
        result = run_cli("create", "feat", cwd=repo.path)
        assert result.code == 0
        lines = result.out.splitlines()
        assert len(lines) == 1
        assert lines[0].startswith("cd ")
        assert lines[0].endswith("&& stub-editor")
        assert "created worktree" in result.err  # human messages on stderr

    def test_no_open_emits_nothing(self, run_cli: Run, repo: Repo) -> None:
        result = run_cli("create", "feat", "--no-open", cwd=repo.path)
        assert result.code == 0
        assert result.out == ""

    def test_list_porcelain_on_stdout(self, run_cli: Run, repo: Repo) -> None:
        run_cli("create", "feat", "--no-open", cwd=repo.path)
        result = run_cli("list", "--porcelain", cwd=repo.path)
        assert result.code == 0
        name, branch, path, dirty = result.out.rstrip("\n").split("\t")
        assert (name, branch, dirty) == ("feat", "feat", "0")
        assert path.endswith("worktrees/api/feat")

    def test_checkout_emits_cd_to_main(self, run_cli: Run, repo: Repo) -> None:
        run_cli("create", "feat", "--no-open", cwd=repo.path)
        result = run_cli("checkout", "feat", cwd=repo.path)
        assert result.code == 0
        assert result.out.strip() == f"cd {repo.path}"

    def test_config_dump_on_stdout(self, run_cli: Run, repo: Repo) -> None:
        result = run_cli("config", cwd=repo.path)
        assert result.code == 0
        assert "worktrees_dir:" in result.out


class TestShortcut:
    def test_unknown_first_word_opens_with_opener(self, run_cli: Run, repo: Repo) -> None:
        run_cli("create", "feat", "--no-open", cwd=repo.path)
        result = run_cli("mytool", "feat", cwd=repo.path)
        assert result.code == 0
        assert result.out.strip().endswith("&& mytool")

    def test_shortcut_passes_path(self, run_cli: Run, repo: Repo) -> None:
        run_cli("create", "feat", "--no-open", cwd=repo.path)
        result = run_cli("mytool {path}", "feat", "-p", "README.md", cwd=repo.path)
        assert result.code == 0
        assert result.out.strip().endswith("&& mytool README.md")


class TestExitCodes:
    def test_cancelled_off_tty_is_3(self, run_cli: Run, repo: Repo) -> None:
        run_cli("create", "feat", "--no-open", cwd=repo.path)
        repo.make_dirty(worktree=repo.path.parent.parent / "dev" / "worktrees" / "api" / "feat")
        result = run_cli("delete", "feat", cwd=repo.path)
        assert result.code == 3
        assert "use --force" in result.err

    def test_missing_worktree_is_1(self, run_cli: Run, repo: Repo) -> None:
        result = run_cli("open", "ghost", cwd=repo.path)
        assert result.code == 1

    def test_failing_script_is_1(self, run_cli: Run, repo: Repo) -> None:
        repo.write_project_config("scripts:\n  boom: exit 9\n")
        result = run_cli("run", "boom", cwd=repo.path)
        assert result.code == 1
        assert "exit code 9" in result.err


class TestClaudeGate:
    def test_hidden_without_claude_dir(self, run_cli: Run, repo: Repo) -> None:
        # "claude" is not a subcommand without ~/.claude: it falls through to
        # the opener shortcut, and the extra argument is a usage error
        result = run_cli("claude", "copy-session", "x", cwd=repo.path)
        assert result.code == 2

    def test_visible_with_claude_dir(self, run_cli: Run) -> None:
        (Path.home() / ".claude").mkdir(parents=True)
        result = run_cli("--help")
        assert result.code == 0
        assert "claude" in result.out
