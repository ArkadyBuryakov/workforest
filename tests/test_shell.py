"""Shell integration: shell-init output driven in real bash/zsh sessions,
plus the --complete backend."""

import os
import shutil
import subprocess
import sys
from collections.abc import Callable
from pathlib import Path

import pytest

from workforest import commands, completions

from .conftest import CliResult, Repo

Run = Callable[..., CliResult]


def run_shell(shell: str, script: str, cwd: Path) -> subprocess.CompletedProcess[str]:
    """Run a script in a real shell with the venv's workforest on PATH."""
    env = os.environ.copy()
    venv_bin = Path(sys.executable).parent
    env["PATH"] = f"{venv_bin}{os.pathsep}{env['PATH']}"
    return subprocess.run(
        [shell, "-c", script], cwd=cwd, env=env, capture_output=True, text=True, check=False
    )


class TestShellInit:
    def test_bash_output_is_valid_syntax(self, run_cli: Run, tmp_path: Path) -> None:
        result = run_cli("shell-init", "bash")
        assert result.code == 0
        script = tmp_path / "init.bash"
        script.write_text(result.out)
        check = subprocess.run(["bash", "-n", str(script)], capture_output=True, check=False)
        assert check.returncode == 0, check.stderr

    @pytest.mark.skipif(shutil.which("zsh") is None, reason="zsh not installed")
    def test_zsh_output_is_valid_syntax(self, run_cli: Run, tmp_path: Path) -> None:
        result = run_cli("shell-init", "zsh")
        assert result.code == 0
        script = tmp_path / "init.zsh"
        script.write_text(result.out)
        check = subprocess.run(["zsh", "-n", str(script)], capture_output=True, check=False)
        assert check.returncode == 0, check.stderr

    def test_static_completion_files_are_valid_syntax(self) -> None:
        root = Path(__file__).parent.parent
        bash_file = root / "src" / "workforest" / "shell" / "completion.bash"
        check = subprocess.run(["bash", "-n", str(bash_file)], capture_output=True, check=False)
        assert check.returncode == 0, check.stderr
        if shutil.which("zsh"):
            zsh_file = root / "completions" / "_workforest"
            check = subprocess.run(
                ["zsh", "-c", f"autoload -Uz compinit; compinit -u; source {zsh_file}"],
                capture_output=True,
                check=False,
            )
            # sourcing outside completion context must at least parse;
            # compadd errors are acceptable, syntax errors are not
            assert b"parse error" not in check.stderr

    @pytest.mark.skipif(shutil.which("zsh") is None, reason="zsh not installed")
    @pytest.mark.parametrize("init_first", [True, False])
    def test_zsh_registration_survives_compinit_ordering(
        self, init_first: bool, repo: Repo
    ) -> None:
        """Regression: eval'ing shell-init before compinit must still register
        completions (deferred via a one-shot precmd hook)."""
        init = 'eval "$(workforest shell-init zsh)"'
        compinit = "autoload -Uz compinit && compinit -u"
        script = "\n".join(
            [
                *((init, compinit) if init_first else (compinit, init)),
                "for f in $precmd_functions; do $f; done",  # simulate first prompt
                'print -r -- "${_comps[wf]:-NOTHING}:${_comps[workforest]:-NOTHING}"',
                'print -r -- "hooks:${#precmd_functions}"',
            ]
        )
        result = run_shell("zsh", script, cwd=repo.path)
        assert result.returncode == 0, result.stderr
        assert "_workforest_complete:_workforest_complete" in result.stdout
        assert "hooks:0" in result.stdout  # the one-shot hook removed itself

    def test_detects_shell_from_env(self, run_cli: Run, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("SHELL", "/usr/bin/zsh")
        result = run_cli("shell-init")
        assert result.code == 0
        assert "compdef" in result.out

    def test_unknown_shell_errors(self, run_cli: Run, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("SHELL", "/usr/bin/fish")
        result = run_cli("shell-init")
        assert result.code == 1
        assert "fish" in result.err


@pytest.mark.parametrize(
    "shell",
    [
        "bash",
        pytest.param(
            "zsh",
            marks=pytest.mark.skipif(shutil.which("zsh") is None, reason="zsh not installed"),
        ),
    ],
)
class TestWfFunction:
    def test_wf_changes_directory(self, shell: str, repo: Repo) -> None:
        ctx = commands.build_context(repo.path)
        commands.cmd_create(ctx, "feat", no_open=True)
        script = f'eval "$(workforest shell-init {shell})"\nwf open feat -o true\npwd\n'
        result = run_shell(shell, script, cwd=repo.path)
        assert result.returncode == 0, result.stderr
        assert result.stdout.strip() == str(ctx.worktrees_dir / "feat")

    def test_wf_passes_through_non_cd_output(self, shell: str, repo: Repo) -> None:
        ctx = commands.build_context(repo.path)
        commands.cmd_create(ctx, "feat", no_open=True)
        script = f'eval "$(workforest shell-init {shell})"\nwf list --porcelain\n'
        result = run_shell(shell, script, cwd=repo.path)
        assert result.returncode == 0, result.stderr
        assert result.stdout.startswith("feat\tfeat\t")

    def test_wf_propagates_failure(self, shell: str, repo: Repo) -> None:
        script = f'eval "$(workforest shell-init {shell})"\nwf open ghost\n'
        result = run_shell(shell, script, cwd=repo.path)
        assert result.returncode == 1
        assert "not found" in result.stderr


class TestCompleteBackend:
    def test_branches_exclude_checked_out(self, repo: Repo, run_cli: Run) -> None:
        repo.add_branch("free")
        repo.add_branch("taken")
        ctx = commands.build_context(repo.path)
        commands.cmd_create(ctx, "taken", no_open=True)
        result = run_cli("--complete", "branches", cwd=repo.path)
        assert result.code == 0
        candidates = [line.split("\t")[0] for line in result.out.splitlines()]
        assert "free" in candidates
        assert "taken" not in candidates
        assert "main" not in candidates  # checked out in the main worktree

    def test_branches_annotated_with_location(
        self, make_repo: Callable[..., Repo], run_cli: Run
    ) -> None:
        repo = make_repo(origin=True)
        repo.add_branch("everywhere")
        repo.add_branch("remote-only", remote_only=True)
        result = run_cli("--complete", "branches", cwd=repo.path)
        rows = dict(line.split("\t") for line in result.out.splitlines())
        assert rows["everywhere"] == "local, origin"
        # remote-only branches are offered remote-qualified
        assert "remote-only" not in rows
        assert rows["origin/remote-only"] == "origin"

    def test_branches_on_multiple_remotes_offered_qualified(
        self, make_repo: Callable[..., Repo], run_cli: Run
    ) -> None:
        repo = make_repo(origin=True)
        repo.add_remote("upstream")
        repo.add_branch("shared")
        repo.git("push", "-q", "upstream", "shared")
        repo.git("branch", "-D", "shared")
        result = run_cli("--complete", "branches", cwd=repo.path)
        rows = dict(line.split("\t") for line in result.out.splitlines())
        assert "shared" not in rows
        assert rows["origin/shared"] == "origin"
        assert rows["upstream/shared"] == "upstream"

    def test_worktrees(self, repo: Repo, run_cli: Run) -> None:
        ctx = commands.build_context(repo.path)
        commands.cmd_create(ctx, "feature/x", no_open=True)
        result = run_cli("--complete", "worktrees", cwd=repo.path)
        assert result.out.splitlines() == ["x"]

    def test_scripts_and_openers(self, repo: Repo, run_cli: Run) -> None:
        repo.write_project_config(
            "scripts:\n  migrate: 'true'\n  test: 'true'\nopeners:\n  edit: '$EDITOR {target}'\n"
        )
        result = run_cli("--complete", "scripts", cwd=repo.path)
        assert result.out.splitlines() == ["migrate", "test"]
        result = run_cli("--complete", "openers", cwd=repo.path)
        assert result.out.splitlines() == ["edit"]

    def test_commands_include_subcommands_and_openers(self, repo: Repo, run_cli: Run) -> None:
        repo.write_project_config("openers:\n  myedit: '$EDITOR'\n")
        result = run_cli("--complete", "commands", cwd=repo.path)
        rows = [line.split("\t") for line in result.out.splitlines()]
        assert all(len(row) == 3 for row in rows)  # NAME, KIND, DESCRIPTION
        by_name = {name: (kind, desc) for name, kind, desc in rows}
        assert by_name["create"][0] == "command"
        assert by_name["create"][1]  # help text carried through
        assert by_name["myedit"] == ("opener", "$EDITOR")
        assert "claude" not in by_name  # gated: no ~/.claude

    def test_commands_drop_openers_shadowed_by_subcommands(self, repo: Repo, run_cli: Run) -> None:
        """An opener named like a subcommand never dispatches (the subcommand
        wins in cli._preprocess), so it must not be offered."""
        repo.write_project_config("openers:\n  create: 'code {target}'\n  myedit: '$EDITOR'\n")
        result = run_cli("--complete", "commands", cwd=repo.path)
        names = [line.split("\t")[0] for line in result.out.splitlines()]
        assert names.count("create") == 1
        assert "myedit" in names

    def test_never_errors(self, tmp_path: Path, run_cli: Run) -> None:
        outside = tmp_path / "outside"
        outside.mkdir()
        for topic in ("branches", "worktrees", "scripts", "bogus", ""):
            result = run_cli("--complete", topic, cwd=outside)
            assert result.code == 0
            assert result.out == ""

    def test_pure_backend_outside_repo(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        outside = tmp_path / "outside"
        outside.mkdir()
        monkeypatch.chdir(outside)
        assert completions.complete("worktrees") == []
        names = [line.split("\t")[0] for line in completions.complete("commands")]
        assert "create" in names
