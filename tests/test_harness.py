"""Canary tests: prove the isolation fixtures actually isolate."""

import os
import subprocess
from pathlib import Path

from workforest import __version__


def test_home_is_not_real_home(tmp_path: Path) -> None:
    assert os.environ["HOME"].startswith(str(tmp_path))
    assert Path.home() != Path("/home") / os.environ.get("LOGNAME", "nobody")
    assert os.environ["XDG_CONFIG_HOME"].startswith(str(tmp_path))


def test_user_tooling_is_pinned() -> None:
    assert os.environ["EDITOR"] == "stub-editor"
    assert os.environ["SHELL"] == "/bin/sh"
    assert "VISUAL" not in os.environ
    assert not [v for v in os.environ if v.startswith("WORKFOREST_")]


def test_git_identity_is_local() -> None:
    out = subprocess.run(
        ["git", "config", "--global", "user.email"],
        capture_output=True,
        text=True,
        check=True,
    )
    assert out.stdout.strip() == "test@example.invalid"


def test_version_is_a_version() -> None:
    assert __version__.count(".") == 2
