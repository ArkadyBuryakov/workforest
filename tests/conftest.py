"""Shared fixtures (IMPLEMENTATION_PLAN "Testing suite").

Isolation contract: no test may read or write the real environment — HOME and
XDG dirs are redirected into tmp_path, WORKFOREST_* is cleared, user tooling
variables are pinned to stubs, and git identity/config is local to the test.
"""

from __future__ import annotations

import os
import stat
import subprocess
import sys
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

import pytest


@pytest.fixture(autouse=True)
def isolated_env(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Redirect every environment touchpoint into tmp_path."""
    home = tmp_path / "home"
    xdg = home / ".config"
    xdg.mkdir(parents=True)
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("XDG_CONFIG_HOME", str(xdg))

    for var in list(os.environ):
        if var.startswith(("WORKFOREST_", "WF_", "GIT_")):
            monkeypatch.delenv(var, raising=False)
    monkeypatch.delenv("VISUAL", raising=False)
    monkeypatch.setenv("EDITOR", "stub-editor")
    monkeypatch.setenv("SHELL", "/bin/sh")
    monkeypatch.setenv("NO_COLOR", "1")

    gitconfig = tmp_path / "gitconfig"
    gitconfig.write_text(
        "[user]\n\tname = Test\n\temail = test@example.invalid\n[init]\n\tdefaultBranch = main\n"
    )
    monkeypatch.setenv("GIT_CONFIG_GLOBAL", str(gitconfig))
    monkeypatch.setenv("GIT_CONFIG_SYSTEM", os.devnull)

    # System config dir is /etc/workforest in production; tests get their own.
    from workforest import config as config_mod

    system_dir = tmp_path / "etc-workforest"
    monkeypatch.setattr(config_mod, "SYSTEM_CONFIG_DIR", system_dir)
    return tmp_path


@dataclass
class Repo:
    """Handle to a throwaway git repository."""

    path: Path
    origin: Path | None = None

    def git(self, *args: str, cwd: Path | None = None) -> str:
        result = subprocess.run(
            ["git", "-C", str(cwd or self.path), *args],
            capture_output=True,
            text=True,
            check=True,
        )
        return result.stdout.strip()

    def commit(self, message: str = "commit") -> None:
        self.git("add", "-A")
        self.git("commit", "--allow-empty", "-m", message)

    def add_branch(self, name: str, *, remote_only: bool = False) -> None:
        """Create a branch; with remote_only=True it exists only on origin."""
        self.git("branch", name)
        if remote_only:
            if self.origin is None:
                raise RuntimeError("repo has no origin")
            self.git("push", "-q", "origin", name)
            self.git("branch", "-D", name)
        elif self.origin is not None:
            self.git("push", "-q", "origin", name)

    def make_dirty(self, worktree: Path | None = None, name: str = "dirty.txt") -> None:
        ((worktree or self.path) / name).write_text("uncommitted\n")

    def write_project_config(self, content: str, *, subdir: str = "") -> Path:
        target = self.path / subdir if subdir else self.path
        target.mkdir(parents=True, exist_ok=True)
        config = target / ".workforest.yaml"
        config.write_text(content)
        return config


@pytest.fixture
def make_repo(tmp_path: Path) -> Callable[..., Repo]:
    """Factory for real git repos under tmp_path (DESIGN §7.3 layer 2)."""

    def _make(name: str = "api", *, origin: bool = False) -> Repo:
        path = tmp_path / "dev" / name
        path.mkdir(parents=True)
        subprocess.run(["git", "init", "-q", "-b", "main", str(path)], check=True)
        repo = Repo(path=path)
        (path / "README.md").write_text(f"# {name}\n")
        repo.commit("init")
        if origin:
            bare = tmp_path / "remotes" / f"{name}.git"
            bare.parent.mkdir(parents=True, exist_ok=True)
            subprocess.run(["git", "init", "-q", "--bare", str(bare)], check=True)
            repo.git("remote", "add", "origin", str(bare))
            repo.git("push", "-q", "-u", "origin", "main")
            repo.origin = bare
        return repo

    return _make


@pytest.fixture
def repo(make_repo: Callable[..., Repo]) -> Repo:
    return make_repo()


@dataclass
class Recorder:
    """Executable stub that logs each invocation instead of doing anything."""

    path: Path
    log: Path

    def lines(self) -> list[str]:
        if not self.log.exists():
            return []
        return self.log.read_text().splitlines()

    def wait_for_lines(self, count: int = 1, timeout: float = 5.0) -> list[str]:
        """Poll for detached spawns that write the log asynchronously."""
        import time

        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            lines = self.lines()
            if len(lines) >= count:
                return lines
            time.sleep(0.02)
        raise TimeoutError(f"recorder log never reached {count} line(s): {self.lines()}")


@pytest.fixture
def recorder(tmp_path: Path) -> Recorder:
    log = tmp_path / "recorder.log"
    script = tmp_path / "recorder"
    script.write_text(
        f'#!/bin/sh\necho "argv=$* argc=$# cwd=$PWD wf_worktree=$WF_WORKTREE" >> {log}\n'
    )
    script.chmod(script.stat().st_mode | stat.S_IXUSR)
    return Recorder(path=script, log=log)


@dataclass
class CliResult:
    code: int
    out: str
    err: str


@pytest.fixture
def run_cli(capsys: pytest.CaptureFixture[str]) -> Callable[..., CliResult]:
    """Invoke main(argv) in-process; the only sanctioned CLI entry in tests."""

    def _run(*argv: str, cwd: Path | None = None) -> CliResult:
        from workforest import cli

        old_cwd = Path.cwd()
        if cwd is not None:
            os.chdir(cwd)
        try:
            code = cli.main(list(argv))
        finally:
            os.chdir(old_cwd)
        captured = capsys.readouterr()
        return CliResult(code=code, out=captured.out, err=captured.err)

    return _run


@pytest.fixture
def tty(monkeypatch: pytest.MonkeyPatch) -> Callable[[list[str]], None]:
    """Pretend stdin/stderr are a terminal and script the prompt answers."""

    answers: list[str] = []

    def _arm(scripted: list[str]) -> None:
        answers.extend(scripted)

    monkeypatch.setattr(sys.stdin, "isatty", lambda: True)
    monkeypatch.setattr(sys.stderr, "isatty", lambda: True)
    monkeypatch.setattr("builtins.input", lambda: answers.pop(0))
    return _arm
