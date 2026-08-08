"""launch: opener/window template resolution and the cd protocol."""

from pathlib import Path

import pytest

from workforest import launch
from workforest.config import Config
from workforest.errors import WorkforestError

from .conftest import Recorder


class TestOpenerResolution:
    def test_named_opener_from_config(self) -> None:
        cfg = Config(openers={"edit": "$EDITOR {path}"})
        assert launch.resolve_opener_template(cfg, "edit") == "$EDITOR {path}"

    def test_unknown_name_used_verbatim(self) -> None:
        cfg = Config(openers={"edit": "$EDITOR {path}"})
        assert launch.resolve_opener_template(cfg, "my-tool --flag") == "my-tool --flag"

    def test_config_default_opener(self) -> None:
        cfg = Config(opener="edit", openers={"edit": "$EDITOR {path}"})
        assert launch.resolve_opener_template(cfg, None) == "$EDITOR {path}"

    def test_visual_beats_editor(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("VISUAL", "visual-tool")
        assert launch.resolve_opener_template(Config(), None) == "visual-tool"

    def test_editor_fallback(self) -> None:
        # EDITOR is pinned to stub-editor by the isolation fixture
        assert launch.resolve_opener_template(Config(), None) == "stub-editor"

    def test_no_opener_anywhere_errors(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.delenv("EDITOR")
        with pytest.raises(WorkforestError, match="no opener"):
            launch.resolve_opener_template(Config(), None)


class TestTargetResolution:
    def test_default_is_worktree_root(self, tmp_path: Path) -> None:
        assert launch.resolve_target(tmp_path, None) == (tmp_path, ".")
        assert launch.resolve_target(tmp_path, ".") == (tmp_path, ".")

    def test_directory_becomes_cwd(self, tmp_path: Path) -> None:
        (tmp_path / "src").mkdir()
        assert launch.resolve_target(tmp_path, "src") == (tmp_path / "src", ".")

    def test_file_stays_argument(self, tmp_path: Path) -> None:
        (tmp_path / "README.md").write_text("x")
        assert launch.resolve_target(tmp_path, "README.md") == (tmp_path, "README.md")

    def test_missing_path_stays_argument(self, tmp_path: Path) -> None:
        assert launch.resolve_target(tmp_path, "new-file.txt") == (tmp_path, "new-file.txt")


class TestCommandBuilding:
    def test_path_placeholder_substituted_and_quoted(self) -> None:
        assert launch.build_opener_command("tool {path}", "a file.txt") == "tool 'a file.txt'"

    def test_env_expansion(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("MYTOOL", "actual-tool")
        assert launch.build_opener_command("$MYTOOL {path}", ".") == "actual-tool ."

    def test_no_placeholder_left_alone(self) -> None:
        assert launch.build_opener_command("plain-tool --flag", ".") == "plain-tool --flag"


class TestLaunch:
    def test_shell_action_by_default(self, tmp_path: Path) -> None:
        worktree = tmp_path / "feat"
        worktree.mkdir()
        action = launch.launch(Config(), worktree=worktree, repo_name="api")
        assert action is not None
        assert action.script == f"cd {worktree} && stub-editor"

    def test_window_command_spawns_detached(self, tmp_path: Path, recorder: Recorder) -> None:
        worktree = tmp_path / "feat"
        worktree.mkdir()
        cfg = Config(window_command=f"{recorder.path} --title {{title}} {{command}}")
        action = launch.launch(cfg, worktree=worktree, repo_name="api", opener_arg="the-opener")
        assert action is None  # nothing on stdout when a window is spawned
        (line,) = recorder.wait_for_lines(1)
        assert "--title" in line
        assert "api: feat" in line
        assert "the-opener" in line
        assert f"cwd={worktree}" in line

    def test_window_path_placeholder(self, tmp_path: Path, recorder: Recorder) -> None:
        worktree = tmp_path / "feat"
        worktree.mkdir()
        cfg = Config(window_command=f"{recorder.path} -d {{path}} {{command}}")
        launch.launch(cfg, worktree=worktree, repo_name="api", opener_arg="x")
        (line,) = recorder.wait_for_lines(1)
        assert f"-d {worktree}" in line

    def test_empty_window_command_after_expansion(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("MY_TERM", "")
        cfg = Config(window_command="$MY_TERM")
        with pytest.raises(WorkforestError, match="empty"):
            launch.launch(cfg, worktree=tmp_path, repo_name="api", opener_arg="x")

    def test_missing_window_program(self, tmp_path: Path) -> None:
        cfg = Config(window_command="no-such-terminal-xyz {command}")
        with pytest.raises(WorkforestError, match="not found"):
            launch.launch(cfg, worktree=tmp_path, repo_name="api", opener_arg="x")

    def test_cd_action_quotes(self) -> None:
        action = launch.cd_action(Path("/tmp/with space"))
        assert action.script == "cd '/tmp/with space'"
