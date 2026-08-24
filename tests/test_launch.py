"""launch: opener resolution, wrapper composition, and the cd protocol."""

import os
import shlex
import subprocess
from pathlib import Path

import pytest

from workforest import launch
from workforest.config import CommandSpec, Config, OpenerSpec
from workforest.errors import WorkforestError

from .conftest import Recorder

EDIT = '$EDITOR "$WF_TARGET"'
KITTY = CommandSpec('kitty $SHELL -c "$WF_COMMAND"', background=True)
DIRENV = CommandSpec('direnv exec "$WF_WORKTREE" $SHELL -c "$WF_COMMAND"')


def resolved(command: str, background: bool = False) -> launch.ResolvedOpener:
    """An unwrapped resolution: the opener's own command spawns."""
    return launch.ResolvedOpener(CommandSpec(command, background))


class TestOpenerResolution:
    def test_named_opener_from_config(self) -> None:
        cfg = Config(openers={"edit": OpenerSpec(EDIT)})
        assert launch.resolve_opener(cfg, "edit") == resolved(EDIT)

    def test_unknown_name_used_verbatim(self) -> None:
        cfg = Config(openers={"edit": OpenerSpec(EDIT)})
        assert launch.resolve_opener(cfg, "my-tool --flag") == resolved("my-tool --flag")

    def test_config_default_opener(self) -> None:
        cfg = Config(opener="edit", openers={"edit": OpenerSpec(EDIT)})
        assert launch.resolve_opener(cfg, None) == resolved(EDIT)

    def test_visual_beats_editor(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("VISUAL", "visual-tool")
        assert launch.resolve_opener(Config(), None) == resolved("visual-tool")

    def test_editor_fallback(self) -> None:
        # EDITOR is pinned to stub-editor by the isolation fixture
        assert launch.resolve_opener(Config(), None) == resolved("stub-editor")

    def test_no_opener_anywhere_errors(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.delenv("EDITOR")
        with pytest.raises(WorkforestError, match="no opener"):
            launch.resolve_opener(Config(), None)

    def test_background_opener(self) -> None:
        cfg = Config(opener="code", openers={"code": OpenerSpec("code .", background=True)})
        assert launch.resolve_opener(cfg, "code") == resolved("code .", background=True)
        assert launch.resolve_opener(cfg, None) == resolved("code .", background=True)

    def test_from_reuses_command_and_background(self) -> None:
        cfg = Config(
            openers={
                "code": OpenerSpec("code .", background=True),
                "ide": OpenerSpec(from_="code"),
                "here": OpenerSpec(from_="code", background=False),
            }
        )
        assert launch.resolve_opener(cfg, "ide") == resolved("code .", background=True)
        assert launch.resolve_opener(cfg, "here") == resolved("code .", background=False)

    def test_from_with_wrapper(self) -> None:
        cfg = Config(
            openers={"edit": OpenerSpec(EDIT), "win": OpenerSpec(from_="edit", wrap="kitty")},
            wrappers={"kitty": KITTY},
        )
        assert launch.resolve_opener(cfg, "win") == launch.ResolvedOpener(KITTY, inner=EDIT)

    def test_own_command_with_wrapper(self) -> None:
        cfg = Config(openers={"env": OpenerSpec(EDIT, wrap="direnv")}, wrappers={"direnv": DIRENV})
        assert launch.resolve_opener(cfg, "env") == launch.ResolvedOpener(DIRENV, inner=EDIT)

    def test_wrapper_decides_background(self) -> None:
        # a background command through an attached wrapper runs attached, and
        # an attached command through a background wrapper runs detached
        cfg = Config(
            openers={"code": OpenerSpec("code .", background=True), "edit": OpenerSpec(EDIT)},
            wrappers={"direnv": DIRENV, "kitty": KITTY},
        )
        assert launch.resolve_opener(cfg, "code", "direnv").run.background is False
        assert launch.resolve_opener(cfg, "edit", "kitty").run.background is True

    def test_wrap_arg_beats_opener(self) -> None:
        cfg = Config(
            openers={"win": OpenerSpec(EDIT, wrap="kitty")},
            wrappers={"kitty": KITTY, "direnv": DIRENV},
        )
        assert launch.resolve_opener(cfg, "win").run is KITTY
        assert launch.resolve_opener(cfg, "win", "direnv").run is DIRENV

    def test_empty_wrap_means_none(self) -> None:
        cfg = Config(openers={"win": OpenerSpec(EDIT, wrap="kitty")}, wrappers={"kitty": KITTY})
        assert launch.resolve_opener(cfg, "win", "") == resolved(EDIT)

    def test_unknown_wrapper_errors(self) -> None:
        cfg = Config(openers={"win": OpenerSpec(EDIT, wrap="kity")}, wrappers={"kitty": KITTY})
        with pytest.raises(WorkforestError, match=r"unknown wrapper 'kity' \(available: kitty\)"):
            launch.resolve_opener(cfg, "win")
        with pytest.raises(WorkforestError, match="none defined"):
            launch.resolve_opener(Config(), "x", "kitty")

    def test_from_without_a_command_errors(self) -> None:
        # load_config rejects these; a hand-built Config still fails cleanly
        with pytest.raises(WorkforestError, match="'from: b' has no command"):
            launch.resolve_opener(Config(openers={"a": OpenerSpec(from_="b")}), "a")
        chain = Config(openers={"a": OpenerSpec(from_="b"), "b": OpenerSpec(from_="a")})
        with pytest.raises(WorkforestError, match="'from: b' has no command"):
            launch.resolve_opener(chain, "a")


class TestDescribe:
    def test_describe(self) -> None:
        cfg = Config(
            openers={
                "edit": OpenerSpec(EDIT),
                "win": OpenerSpec(from_="edit", wrap="kitty"),
                "git": OpenerSpec("lazygit"),
            }
        )
        assert launch.describe_opener(cfg, "win") == f"{EDIT} via kitty"
        assert launch.describe_opener(cfg, "git") == "lazygit"
        assert launch.describe_opener(cfg, "edit") == EDIT
        assert launch.describe_opener(cfg, "anything") == "anything"


class TestLaunchVars:
    def test_full_family(self, tmp_path: Path) -> None:
        variables = launch.launch_vars(
            main=tmp_path / "api",
            worktree=tmp_path / "worktrees" / "api" / "feat",
            worktrees_dir=tmp_path / "worktrees" / "api",
            branch="feat/x",
            target="src/foo.py",
        )
        family = {
            "WF_MAIN": str(tmp_path / "api"),
            "WF_NAME": "api",
            "WF_WORKTREES_DIR": str(tmp_path / "worktrees" / "api"),
            "WF_WORKTREE": str(tmp_path / "worktrees" / "api" / "feat"),
            "WF_BRANCH": "feat/x",
            "WF_TARGET": "src/foo.py",
            "WF_TITLE": "api: feat",
        }
        env = variables.pop("WF_ENV")
        assert variables == family
        assert env == " ".join(f"{k}={shlex.quote(v)}" for k, v in family.items())

    def test_wf_env_recreates_the_family_elsewhere(self, tmp_path: Path) -> None:
        # A command string that crosses into a process without our environment
        # (tmux server, ssh) can `export $WF_ENV` to get it back, quoting intact.
        variables = launch.launch_vars(
            main=tmp_path / "api",
            worktree=tmp_path / "o'brien",
            worktrees_dir=tmp_path,
            branch=None,
            target="src/a b",
        )
        # the outer shell expands $WF_ENV into the string; a fresh shell
        # (standing in for the one tmux spawns) re-parses it from scratch
        probe = 'sh -c "export $WF_ENV; printf %s \\"$WF_TITLE|$WF_TARGET|$WF_BRANCH.\\""'
        result = subprocess.run(
            ["sh", "-c", probe],
            env={**os.environ, **variables},
            capture_output=True,
            text=True,
            check=True,
        )
        assert result.stdout == "api: o'brien|src/a b|."

    def test_detached_branch_is_empty(self, tmp_path: Path) -> None:
        variables = launch.launch_vars(
            main=tmp_path / "api",
            worktree=tmp_path / "feat",
            worktrees_dir=tmp_path,
            branch=None,
            target=".",
        )
        assert variables["WF_BRANCH"] == ""


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
    wrap_arg: str | None = None,
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
        wrap_arg=wrap_arg,
        path_arg=path_arg,
    )
    return action, worktree


def background_wrapper(command: str) -> Config:
    """A config whose only wrapper runs `command` detached."""
    return Config(wrappers={"win": CommandSpec(command, background=True)})


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
        # SHELL is pinned to /bin/sh by the isolation fixture; the child
        # shell, not workforest, expands the opener command.
        assert action.script == f"cd {worktree} && {assignments} /bin/sh -c stub-editor"
        # WF_* is scoped to the child shell via prefix assignments, not exported.
        assert "export" not in action.script
        assert "WF_TITLE='api: feat'" in action.script

    def test_opener_is_shell_expanded_not_pre_expanded(self, tmp_path: Path) -> None:
        (tmp_path / "worktrees" / "api" / "feat" / "src").mkdir(parents=True)
        action, worktree = run_launch(
            Config(), tmp_path, opener_arg='tool "$WF_TARGET"', path_arg="src"
        )
        assert action is not None
        # cwd stays the worktree root; -p only sets WF_TARGET, and the
        # command reaches the child shell verbatim — no workforest expansion.
        assert action.script.startswith(f"cd {worktree} && ")
        assert action.script.endswith(""" /bin/sh -c 'tool "$WF_TARGET"'""")
        assert "WF_TARGET=src" in action.script

    def test_shell_syntax_passes_through_verbatim(self, tmp_path: Path) -> None:
        # tmux braces, $$, && — all shell business, none of ours.
        opener = "tmux display -p '#{pane_id}' && echo $$"
        action, _ = run_launch(Config(), tmp_path, opener_arg=opener)
        assert action is not None
        assert action.script.endswith(f" /bin/sh -c {shlex.quote(opener)}")

    def test_attached_wrapper_runs_in_the_shell(self, tmp_path: Path) -> None:
        cfg = Config(wrappers={"direnv": CommandSpec('direnv exec . $SHELL -c "$WF_COMMAND"')})
        action, worktree = run_launch(cfg, tmp_path, opener_arg="the-opener", wrap_arg="direnv")
        assert action is not None
        # the wrapper is what the child shell runs; the opener rides along as
        # WF_COMMAND, unexpanded, after the rest of the family
        assert action.script.startswith(f"cd {worktree} && WF_MAIN=")
        assert action.script.endswith(
            """ WF_COMMAND=the-opener /bin/sh -c 'direnv exec . $SHELL -c "$WF_COMMAND"'"""
        )

    def test_background_command_spawns_detached(self, tmp_path: Path, recorder: Recorder) -> None:
        cfg = Config(openers={"rec": OpenerSpec(f"{recorder.path} --flag", background=True)})
        action, worktree = run_launch(cfg, tmp_path, opener_arg="rec")
        assert action is None  # nothing on stdout when spawning in the background
        (line,) = recorder.wait_for_lines(1)
        assert "argv=--flag" in line
        assert f"cwd={worktree}" in line
        assert f"wf_worktree={worktree}" in line

    def test_background_wrapper_spawns_detached(self, tmp_path: Path, recorder: Recorder) -> None:
        cfg = background_wrapper(f'{recorder.path} --title "$WF_TITLE" $WF_COMMAND')
        action, worktree = run_launch(cfg, tmp_path, opener_arg="the-opener", wrap_arg="win")
        assert action is None
        (line,) = recorder.wait_for_lines(1)
        assert "--title" in line
        assert "api: feat" in line
        assert "the-opener" in line
        assert f"cwd={worktree}" in line

    def test_wrapper_worktree_variable(self, tmp_path: Path, recorder: Recorder) -> None:
        cfg = background_wrapper(f'{recorder.path} -d "$WF_WORKTREE" $WF_COMMAND')
        _, worktree = run_launch(cfg, tmp_path, opener_arg="x", wrap_arg="win")
        (line,) = recorder.wait_for_lines(1)
        assert f"-d {worktree}" in line

    def test_unquoted_wf_command_word_splits(self, tmp_path: Path, recorder: Recorder) -> None:
        cfg = background_wrapper(f"{recorder.path} $WF_COMMAND")
        run_launch(cfg, tmp_path, opener_arg="the-opener --flag", wrap_arg="win")
        (line,) = recorder.wait_for_lines(1)
        assert "argv=the-opener --flag argc=2" in line

    def test_quoted_wf_command_is_one_argument(self, tmp_path: Path, recorder: Recorder) -> None:
        cfg = background_wrapper(f'{recorder.path} "$WF_COMMAND"')
        run_launch(cfg, tmp_path, opener_arg="the-opener --flag", wrap_arg="win")
        (line,) = recorder.wait_for_lines(1)
        assert "argv=the-opener --flag argc=1" in line

    def test_background_process_sheds_inherited_venv(
        self, tmp_path: Path, recorder: Recorder, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("VIRTUAL_ENV", str(tmp_path / ".venv"))
        monkeypatch.setenv("PATH", f"{tmp_path / '.venv' / 'bin'}:{os.environ['PATH']}")
        cfg = background_wrapper(f"{recorder.path} $WF_COMMAND")
        run_launch(cfg, tmp_path, opener_arg="x", wrap_arg="win")
        (line,) = recorder.wait_for_lines(1)
        assert line.endswith("virtual_env=")

    def test_missing_background_program(self, tmp_path: Path) -> None:
        # The shell reports the missing program (127) through the grace check.
        cfg = background_wrapper("no-such-terminal-xyz $WF_COMMAND")
        with pytest.raises(WorkforestError, match="not found"):
            run_launch(cfg, tmp_path, opener_arg="x", wrap_arg="win")

    def test_non_executable_background_program(self, tmp_path: Path) -> None:
        program = tmp_path / "not-executable"
        program.write_text("#!/bin/sh\n")
        cfg = background_wrapper(f"{program} $WF_COMMAND")
        with pytest.raises(WorkforestError, match=r"[Pp]ermission denied"):
            run_launch(cfg, tmp_path, opener_arg="x", wrap_arg="win")

    def test_quote_in_wf_value_passes_through(self, tmp_path: Path, recorder: Recorder) -> None:
        # Regression for the template era: a quote in a WF_* value is just a
        # character in an env var now, never a parse error.
        launch.spawn_background(
            f'{recorder.path} --title "$WF_TITLE"',
            variables={"WF_TITLE": "o'brien: feat"},
            cwd=tmp_path,
        )
        (line,) = recorder.wait_for_lines(1)
        assert "--title o'brien: feat" in line

    def test_cd_action_quotes(self) -> None:
        action = launch.cd_action(Path("/tmp/with space"))
        assert action.script == "cd '/tmp/with space'"


class TestSpawnGracePeriod:
    def make_stub(self, tmp_path: Path, body: str) -> Path:
        program = tmp_path / "stub-term"
        program.write_text(f"#!/bin/sh\n{body}\n")
        program.chmod(0o755)
        return program

    def run_background(self, tmp_path: Path, body: str) -> launch.ShellAction | None:
        cfg = Config(
            openers={"bg": OpenerSpec(str(self.make_stub(tmp_path, body)), background=True)}
        )
        return run_launch(cfg, tmp_path, opener_arg="bg")[0]

    def test_immediate_failure_reports_status_and_stderr(self, tmp_path: Path) -> None:
        with pytest.raises(WorkforestError, match="exited with status 2") as excinfo:
            self.run_background(tmp_path, "echo 'cannot open display' >&2\nexit 2")
        assert "cannot open display" in str(excinfo.value)

    def test_death_by_signal_reports_signal_name(self, tmp_path: Path) -> None:
        with pytest.raises(WorkforestError, match="killed by SIGTERM"):
            self.run_background(tmp_path, "kill -TERM $$")

    def test_long_lived_process_is_success(self, tmp_path: Path) -> None:
        # outlived the grace period: spawned fine, nothing on stdout
        assert self.run_background(tmp_path, "sleep 5") is None

    def test_quick_clean_exit_is_success(self, tmp_path: Path) -> None:
        # daemon-handoff clients (`code .`) exit 0 immediately; not a failure
        assert self.run_background(tmp_path, "exit 0") is None

    def test_spawn_reports_success_on_stderr(
        self, tmp_path: Path, capsys: pytest.CaptureFixture[str]
    ) -> None:
        self.run_background(tmp_path, "exit 0")
        assert "opened feat in the background" in capsys.readouterr().err
