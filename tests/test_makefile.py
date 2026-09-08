"""Makefile targets as scripts: what the parser finds, what the `make`
config offers, and how `wf make` / `wf stop --make` behave."""

import shutil
from collections.abc import Callable
from pathlib import Path

import pytest

from workforest import makefile
from workforest.config import Config, MakeSpec, load_config
from workforest.errors import WorkforestError

from .conftest import CliResult, Repo

Run = Callable[..., CliResult]

MAKEFILE = """\
# Dev targets.
-include Makefile.local

VERSION := 1.0
PREFIX ?= /usr/local
FLAGS ::= -g

.PHONY: build test

build: deps
\t@echo building
test: FLAGS = -O0
test:
\t@echo testing $(VERSION)
lint check:
\t@echo both
%.o: %.c
\t@echo pattern
$(PREFIX)/bin/tool:
\t@echo generated
"""


def write_makefile(root: Path, text: str = MAKEFILE, name: str = "Makefile") -> Path:
    path = root / name
    path.write_text(text)
    return path


class TestParsing:
    def test_finds_plain_rules_in_file_order(self, tmp_path: Path) -> None:
        """Rules only: a prerequisite (`build: deps`) is not a definition."""
        write_makefile(tmp_path)
        assert makefile.targets(tmp_path) == ["build", "test", "lint", "check"]

    @pytest.mark.parametrize(
        "line",
        [
            "\t@echo recipe",
            "# comment: not a rule",
            "VERSION := 1.0",
            "VERSION ::= 1.0",
            "VERSION :::= 1.0",
            "PREFIX ?= /usr/local",
            "",
            "no colon here",
            ".PHONY: build",
            "%.o: %.c",
            "$(PREFIX)/bin/tool:",
        ],
    )
    def test_lines_that_define_no_target(self, line: str) -> None:
        assert makefile.rule_targets(line) == []

    def test_double_colon_and_order_only_rules(self) -> None:
        assert makefile.rule_targets("clean:: extra") == ["clean"]
        assert makefile.rule_targets("app: | build-dir") == ["app"]

    def test_follows_includes_relative_to_the_makefile(self, tmp_path: Path) -> None:
        """In make's own order: an included file is read where it is named."""
        write_makefile(tmp_path, "include extra.mk\nlocal:\n\t@echo local\n")
        (tmp_path / "extra.mk").write_text("included:\n\t@echo included\n")
        assert makefile.targets(tmp_path) == ["included", "local"]

    def test_missing_include_and_variable_paths_are_skipped(self, tmp_path: Path) -> None:
        write_makefile(tmp_path, "-include gone.mk $(GENERATED)\nlocal:\n\t@echo local\n")
        assert makefile.targets(tmp_path) == ["local"]

    def test_include_cycles_end(self, tmp_path: Path) -> None:
        write_makefile(tmp_path, "include a.mk\nroot:\n\t@echo root\n")
        (tmp_path / "a.mk").write_text("include Makefile\na:\n\t@echo a\n")
        assert makefile.targets(tmp_path) == ["a", "root"]

    def test_no_makefile_no_targets(self, tmp_path: Path) -> None:
        assert makefile.targets(tmp_path) == []
        assert makefile.makefile_path(tmp_path) is None
        assert not makefile.available(tmp_path)

    def test_gnu_makefile_wins_over_makefile(self, tmp_path: Path) -> None:
        write_makefile(tmp_path, "fallback:\n\t@echo fallback\n")
        write_makefile(tmp_path, "preferred:\n\t@echo preferred\n", name="GNUmakefile")
        assert makefile.targets(tmp_path) == ["preferred"]


class TestNames:
    def test_round_trip(self) -> None:
        assert makefile.script_name("check") == "make:check"
        assert makefile.target_of("make:check") == "check"

    @pytest.mark.parametrize("name", ["check", "make:", "makecheck", ""])
    def test_other_names_are_not_targets(self, name: str) -> None:
        assert makefile.target_of(name) is None


class TestVisibility:
    def config(self, **make: object) -> Config:
        return Config(make=MakeSpec(**make))  # type: ignore[arg-type]

    @pytest.fixture(autouse=True)
    def _makefile(self, tmp_path: Path) -> None:
        write_makefile(tmp_path)

    def test_everything_by_default(self, tmp_path: Path) -> None:
        targets = makefile.visible_targets(self.config(), tmp_path)
        assert targets == ["build", "test", "lint", "check"]

    def test_hide(self, tmp_path: Path) -> None:
        assert makefile.visible_targets(self.config(hidden=True), tmp_path) == []

    def test_hide_scripts(self, tmp_path: Path) -> None:
        config = self.config(hide_scripts=("build", "lint"))
        assert makefile.visible_targets(config, tmp_path) == ["test", "check"]

    def test_show_scripts_wins_over_hide_scripts(self, tmp_path: Path) -> None:
        config = self.config(show_scripts=("check", "test"), hide_scripts=("check",))
        assert makefile.visible_targets(config, tmp_path) == ["test", "check"]

    def test_nothing_where_there_is_no_makefile(self, tmp_path: Path) -> None:
        (tmp_path / "Makefile").unlink()
        assert makefile.visible_targets(self.config(), tmp_path) == []


class TestSpec:
    def test_command_quotes_the_target(self) -> None:
        spec = makefile.spec_for(Config(), "a target")
        assert spec.command == "make 'a target'"
        assert not spec.exclusive

    def test_exclusive_scripts(self) -> None:
        config = Config(make=MakeSpec(exclusive_scripts=("dev",)))
        assert makefile.spec_for(config, "dev").exclusive
        assert not makefile.spec_for(config, "check").exclusive


class TestRequire:
    def test_no_makefile(self, tmp_path: Path) -> None:
        with pytest.raises(WorkforestError, match="no makefile"):
            makefile.require(tmp_path)

    def test_no_make(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setattr(makefile.shutil, "which", lambda _: None)
        with pytest.raises(WorkforestError, match="make is not installed"):
            makefile.require(tmp_path)


@pytest.mark.skipif(shutil.which("make") is None, reason="make not installed")
class TestCommand:
    def test_runs_the_target_at_the_worktree_root(self, repo: Repo, run_cli: Run) -> None:
        write_makefile(repo.path, "hello:\n\t@echo hello-from-make\n")
        result = run_cli("make", "hello", cwd=repo.path)
        assert result.code == 0
        assert "hello-from-make" in result.err
        assert "running 'make:hello'" in result.err

    def test_extra_args_are_appended(self, repo: Repo, run_cli: Run) -> None:
        write_makefile(repo.path, "show:\n\t@echo goal=$(GOAL)\n")
        result = run_cli("make", "show", "GOAL=yes", cwd=repo.path)
        assert result.code == 0
        assert "goal=yes" in result.err

    def test_a_failing_target_fails(self, repo: Repo, run_cli: Run) -> None:
        """Reported exactly as a failing `wf run` command is."""
        write_makefile(repo.path, "boom:\n\t@exit 3\n")
        result = run_cli("make", "boom", cwd=repo.path)
        assert result.code == 1
        assert "script 'make:boom' failed with exit code 2" in result.err

    def test_an_unknown_target_is_make_s_to_reject(self, repo: Repo, run_cli: Run) -> None:
        write_makefile(repo.path, "hello:\n\t@echo hi\n")
        result = run_cli("make", "nope", cwd=repo.path)
        assert result.code != 0

    def test_a_hidden_target_still_runs(self, repo: Repo, run_cli: Run) -> None:
        write_makefile(repo.path, "hello:\n\t@echo hi\n")
        repo.write_project_config("make:\n  hidden: true\n")
        assert run_cli("make", "hello", cwd=repo.path).code == 0
        assert run_cli("--complete", "make", cwd=repo.path).out == ""

    def test_without_a_makefile_it_says_so(self, repo: Repo, run_cli: Run) -> None:
        result = run_cli("make", "hello", cwd=repo.path)
        assert result.code == 1
        assert "no makefile" in result.err

    def test_stop_make_needs_a_running_target(self, repo: Repo, run_cli: Run) -> None:
        write_makefile(repo.path, "hello:\n\t@echo hi\n")
        result = run_cli("stop", "--make", "hello", cwd=repo.path)
        assert result.code == 1
        assert "'make:hello' is not running" in result.err

    def test_completion_lists_the_visible_targets(self, repo: Repo, run_cli: Run) -> None:
        write_makefile(repo.path, "build:\n\t@echo b\ntest:\n\t@echo t\n")
        repo.write_project_config("make:\n  hide_scripts: [test]\n")
        assert run_cli("--complete", "make", cwd=repo.path).out == "build\n"


class TestConfigSection:
    def test_defaults(self, tmp_path: Path) -> None:
        assert load_config(None).make == MakeSpec()

    def test_layers_merge_per_key(self, repo: Repo) -> None:
        repo.write_project_config("make:\n  hidden: true\n  hide_scripts: [a]\n")
        repo.write_project_config("make:\n  hidden: false\n", subdir=".vscode")
        config = load_config(repo.path)
        assert config.make == MakeSpec(hidden=False, hide_scripts=("a",))

    def test_null_restores_the_default(self, repo: Repo) -> None:
        repo.write_project_config("make:\n  hide_scripts: [a]\n")
        repo.write_project_config("make:\n  hide_scripts: null\n", subdir=".vscode")
        assert load_config(repo.path).make.hide_scripts == ()

    def test_it_shows_up_in_the_dump(self, repo: Repo, run_cli: Run) -> None:
        repo.write_project_config("make:\n  exclusive_scripts: [dev]\n")
        result = run_cli("config", cwd=repo.path)
        assert "exclusive_scripts:\n  - dev" in result.out

    @pytest.mark.parametrize(
        ("text", "message"),
        [
            ("make: []\n", "'make' must be a mapping"),
            ("make:\n  nope: true\n", "make.nope: unknown key"),
            ("make:\n  hidden: yes please\n", "make.hidden must be true or false"),
            ("make:\n  hide_scripts: check\n", "make.hide_scripts must be a list of strings"),
            ("make:\n  show_scripts: [1]\n", "make.show_scripts must be a list of strings"),
        ],
    )
    def test_rejects_a_malformed_section(
        self, repo: Repo, run_cli: Run, text: str, message: str
    ) -> None:
        repo.write_project_config(text)
        result = run_cli("config", cwd=repo.path)
        assert result.code == 4
        assert message in result.err
