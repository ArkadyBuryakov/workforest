"""launch: opener/window template resolution and the cd protocol."""

import os
import shlex
from pathlib import Path

import pytest

from workforest import launch
from workforest.config import Config
from workforest.errors import WorkforestError

from .conftest import Recorder


class TestOpenerResolution:
    def test_named_opener_from_config(self) -> None:
        cfg = Config(openers={"edit": "$EDITOR {target}"})
        assert launch.resolve_opener_template(cfg, "edit") == "$EDITOR {target}"

    def test_unknown_name_used_verbatim(self) -> None:
        cfg = Config(openers={"edit": "$EDITOR {target}"})
        assert launch.resolve_opener_template(cfg, "my-tool --flag") == "my-tool --flag"

    def test_config_default_opener(self) -> None:
        cfg = Config(opener="edit", openers={"edit": "$EDITOR {target}"})
        assert launch.resolve_opener_template(cfg, None) == "$EDITOR {target}"

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


class TestLaunchVars:
    def test_full_family(self, tmp_path: Path) -> None:
        variables = launch.launch_vars(
            main=tmp_path / "api",
            worktree=tmp_path / "worktrees" / "api" / "feat",
            worktrees_dir=tmp_path / "worktrees" / "api",
            branch="feat/x",
            target="src/foo.py",
        )
        assert variables == {
            "WF_MAIN": str(tmp_path / "api"),
            "WF_NAME": "api",
            "WF_WORKTREES_DIR": str(tmp_path / "worktrees" / "api"),
            "WF_WORKTREE": str(tmp_path / "worktrees" / "api" / "feat"),
            "WF_BRANCH": "feat/x",
            "WF_TARGET": "src/foo.py",
            "WF_TITLE": "api: feat",
        }

    def test_detached_branch_is_empty(self, tmp_path: Path) -> None:
        variables = launch.launch_vars(
            main=tmp_path / "api",
            worktree=tmp_path / "feat",
            worktrees_dir=tmp_path,
            branch=None,
            target=".",
        )
        assert variables["WF_BRANCH"] == ""


class TestTemplateExpansion:
    def test_placeholder_is_one_quoted_argument(self) -> None:
        result = launch.expand_template("tool {target}", {"WF_TARGET": "a file.txt"})
        assert result == "tool 'a file.txt'"

    def test_dollar_form_is_raw_and_word_splits(self) -> None:
        result = launch.expand_template("tool $WF_TARGET", {"WF_TARGET": "--flag one"})
        assert result == "tool --flag one"

    def test_env_expansion(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("MYTOOL", "actual-tool")
        assert launch.expand_template("$MYTOOL {target}", {"WF_TARGET": "."}) == "actual-tool ."

    def test_unknown_env_left_alone(self) -> None:
        assert launch.expand_template("tool $NO_SUCH_VAR", {}) == "tool $NO_SUCH_VAR"

    def test_unknown_placeholder_errors_with_known_list(self) -> None:
        with pytest.raises(WorkforestError, match=r"unknown placeholder \{path\}.*\{target\}"):
            launch.expand_template("tool {path}", {"WF_TARGET": "."})

    def test_values_are_inert(self) -> None:
        # A substituted value must never be re-expanded, even if it looks
        # like a template itself.
        result = launch.expand_template("edit {target}", {"WF_TARGET": "$HOME/{title}"})
        assert result == "edit '$HOME/{title}'"

    def test_no_placeholder_left_alone(self) -> None:
        assert launch.expand_template("plain-tool --flag", {"WF_TARGET": "."}) == (
            "plain-tool --flag"
        )


class TestScrubActivationState:
    def test_venv_vars_and_path_entry_removed(self) -> None:
        env = {
            "VIRTUAL_ENV": "/repo/.venv",
            "VIRTUAL_ENV_PROMPT": "(.venv)",
            "PATH": "/repo/.venv/bin:/usr/local/bin:/usr/bin",
            "HOME": "/home/u",
        }
        assert launch.scrub_activation_state(env) == {
            "PATH": "/usr/local/bin:/usr/bin",
            "HOME": "/home/u",
        }

    def test_clean_environment_untouched(self) -> None:
        env = {"PATH": "/usr/local/bin:/usr/bin", "HOME": "/home/u", "WF_BRANCH": "feat"}
        assert launch.scrub_activation_state(env) == env

    def test_conda_including_stacked_prefixes(self) -> None:
        env = {
            "CONDA_PREFIX": "/opt/conda/envs/proj",
            "CONDA_PREFIX_1": "/opt/conda",
            "CONDA_DEFAULT_ENV": "proj",
            "CONDA_SHLVL": "2",
            "CONDA_PROMPT_MODIFIER": "(proj) ",
            "PATH": "/opt/conda/envs/proj/bin:/opt/conda/bin:/opt/conda/condabin:/usr/bin",
        }
        # condabin survives so `conda` itself keeps working in the window.
        assert launch.scrub_activation_state(env) == {
            "PATH": "/opt/conda/condabin:/usr/bin",
        }

    def test_nvm_bin_is_itself_the_path_entry(self) -> None:
        env = {
            "NVM_BIN": "/home/u/.nvm/versions/node/v22.0.0/bin",
            "NVM_INC": "/home/u/.nvm/versions/node/v22.0.0/include/node",
            "PATH": "/home/u/.nvm/versions/node/v22.0.0/bin:/usr/bin",
        }
        assert launch.scrub_activation_state(env) == {"PATH": "/usr/bin"}

    def test_rvm_ruby(self) -> None:
        env = {
            "GEM_HOME": "/home/u/.rvm/gems/ruby-3.3.0",
            "GEM_PATH": "/home/u/.rvm/gems/ruby-3.3.0:/home/u/.rvm/gems/ruby-3.3.0@global",
            "MY_RUBY_HOME": "/home/u/.rvm/rubies/ruby-3.3.0",
            "RUBY_VERSION": "ruby-3.3.0",
            "PATH": (
                "/home/u/.rvm/gems/ruby-3.3.0/bin:/home/u/.rvm/rubies/ruby-3.3.0/bin:/usr/bin"
            ),
        }
        assert launch.scrub_activation_state(env) == {"PATH": "/usr/bin"}

    def test_missing_path_is_fine(self) -> None:
        assert launch.scrub_activation_state({"VIRTUAL_ENV": "/repo/.venv"}) == {}


def run_launch(
    cfg: Config,
    tmp_path: Path,
    *,
    branch: str | None = "feat",
    opener_arg: str | None = None,
    path_arg: str | None = None,
) -> tuple[launch.ShellAction | None, Path]:
    main = tmp_path / "api"
    worktree = tmp_path / "worktrees" / "api" / "feat"
    worktree.mkdir(parents=True, exist_ok=True)
    action = launch.launch(
        cfg,
        main=main,
        worktree=worktree,
        worktrees_dir=worktree.parent,
        branch=branch,
        opener_arg=opener_arg,
        path_arg=path_arg,
    )
    return action, worktree


class TestLaunch:
    def test_shell_action_by_default(self, tmp_path: Path) -> None:
        action, worktree = run_launch(Config(), tmp_path)
        assert action is not None
        variables = launch.launch_vars(
            main=tmp_path / "api",
            worktree=worktree,
            worktrees_dir=worktree.parent,
            branch="feat",
            target=".",
        )
        assignments = " ".join(f"{k}={shlex.quote(v)}" for k, v in variables.items())
        assert action.script == f"cd {worktree} && {assignments} stub-editor"
        # WF_* is scoped to the command via prefix assignments, not exported.
        assert "export" not in action.script
        assert "WF_TITLE='api: feat'" in action.script

    def test_path_arg_is_target_not_cwd(self, tmp_path: Path) -> None:
        (tmp_path / "worktrees" / "api" / "feat" / "src").mkdir(parents=True)
        action, worktree = run_launch(
            Config(), tmp_path, opener_arg="tool {target}", path_arg="src"
        )
        assert action is not None
        # cwd stays the worktree root; -p only sets the opener argument.
        assert action.script.startswith(f"cd {worktree} && ")
        assert action.script.endswith(" tool src")
        assert "WF_TARGET=src" in action.script

    def test_window_command_spawns_detached(self, tmp_path: Path, recorder: Recorder) -> None:
        cfg = Config(window_command=f"{recorder.path} --title {{title}} $WF_COMMAND")
        action, worktree = run_launch(cfg, tmp_path, opener_arg="the-opener")
        assert action is None  # nothing on stdout when a window is spawned
        (line,) = recorder.wait_for_lines(1)
        assert "--title" in line
        assert "api: feat" in line
        assert "the-opener" in line
        assert f"cwd={worktree}" in line

    def test_window_worktree_placeholder(self, tmp_path: Path, recorder: Recorder) -> None:
        cfg = Config(window_command=f"{recorder.path} -d {{worktree}} $WF_COMMAND")
        _, worktree = run_launch(cfg, tmp_path, opener_arg="x")
        (line,) = recorder.wait_for_lines(1)
        assert f"-d {worktree}" in line

    def test_wf_command_splices_into_argv_words(self, tmp_path: Path, recorder: Recorder) -> None:
        cfg = Config(window_command=f"{recorder.path} $WF_COMMAND")
        run_launch(cfg, tmp_path, opener_arg="the-opener --flag")
        (line,) = recorder.wait_for_lines(1)
        assert "argv=the-opener --flag argc=2" in line

    def test_command_placeholder_is_one_argument(self, tmp_path: Path, recorder: Recorder) -> None:
        cfg = Config(window_command=f"{recorder.path} {{command}}")
        run_launch(cfg, tmp_path, opener_arg="the-opener --flag")
        (line,) = recorder.wait_for_lines(1)
        assert "argv=the-opener --flag argc=1" in line

    def test_window_process_inherits_wf_env(self, tmp_path: Path, recorder: Recorder) -> None:
        cfg = Config(window_command=f"{recorder.path} $WF_COMMAND")
        _, worktree = run_launch(cfg, tmp_path, opener_arg="x")
        (line,) = recorder.wait_for_lines(1)
        assert f"wf_worktree={worktree}" in line

    def test_window_process_sheds_inherited_venv(
        self, tmp_path: Path, recorder: Recorder, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("VIRTUAL_ENV", str(tmp_path / ".venv"))
        monkeypatch.setenv("PATH", f"{tmp_path / '.venv' / 'bin'}:{os.environ['PATH']}")
        cfg = Config(window_command=f"{recorder.path} $WF_COMMAND")
        run_launch(cfg, tmp_path, opener_arg="x")
        (line,) = recorder.wait_for_lines(1)
        assert line.endswith("virtual_env=")

    def test_empty_window_command_after_expansion(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("MY_TERM", "")
        cfg = Config(window_command="$MY_TERM")
        with pytest.raises(WorkforestError, match="empty"):
            run_launch(cfg, tmp_path, opener_arg="x")

    def test_missing_window_program(self, tmp_path: Path) -> None:
        cfg = Config(window_command="no-such-terminal-xyz $WF_COMMAND")
        with pytest.raises(WorkforestError, match="not found"):
            run_launch(cfg, tmp_path, opener_arg="x")

    def test_cd_action_quotes(self) -> None:
        action = launch.cd_action(Path("/tmp/with space"))
        assert action.script == "cd '/tmp/with space'"
