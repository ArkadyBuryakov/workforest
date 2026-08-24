"""Opener resolution, wrapper composition, and cd-protocol emission.

No tool names appear here: what to run comes from config (`openers`,
`wrappers`) and the $VISUAL/$EDITOR contract; where to run it is the user's
terminal (ShellAction) unless what spawns is `background`.

Every entry is a plain shell command, exactly like `scripts`: it runs via
`$SHELL -c` with the WF_* family in the environment, so expansion, quoting,
and word splitting are the shell's, never ours — `"$WF_X"` is one argument,
bare `$WF_X` word-splits, and there is no workforest template syntax to
escape. A wrapper additionally gets the opener command unexpanded as
$WF_COMMAND and runs it through a shell of its own, e.g.
`kitty ... $SHELL -c "$WF_COMMAND"`.
"""

import os
import re
import shlex
import signal
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path

from workforest import output
from workforest.config import CommandSpec, Config, OpenerSpec
from workforest.errors import WorkforestError


def _user_shell() -> str:
    return os.environ.get("SHELL") or "sh"


@dataclass(slots=True, frozen=True)
class _Activation:
    """One tool's shell-session activation state that must not leak into a
    spawned window. An empty bin subdir means the prefix variable is itself
    the PATH entry."""

    prefix_var: str
    bin_subdirs: tuple[str, ...]  # subdirs under the prefix that activation put on PATH
    companion_vars: tuple[str, ...]  # variables set alongside the prefix


_ACTIVATION_STATE = (
    _Activation("VIRTUAL_ENV", ("bin",), ("VIRTUAL_ENV_PROMPT",)),
    _Activation(
        "CONDA_PREFIX", ("bin",), ("CONDA_DEFAULT_ENV", "CONDA_PROMPT_MODIFIER", "CONDA_SHLVL")
    ),
    _Activation("NVM_BIN", ("",), ("NVM_INC",)),
    _Activation("GEM_HOME", ("bin",), ("GEM_PATH",)),
    _Activation("MY_RUBY_HOME", ("bin",), ("RUBY_VERSION",)),
)
_CONDA_STACK = re.compile(r"CONDA_PREFIX_\d+")


@dataclass(slots=True, frozen=True)
class ShellAction:
    """A directive for the wf shell wrapper; the only thing cli.py prints
    to stdout for create/open/checkout."""

    script: str


def cd_action(path: Path) -> ShellAction:
    return ShellAction(f"cd {shlex.quote(str(path))}")


@dataclass(slots=True, frozen=True)
class ResolvedOpener:
    """What a launch spawns: the opener's own command, or its wrapper with
    the opener command riding along as $WF_COMMAND."""

    run: CommandSpec  # still unexpanded; its `background` decides where
    inner: str | None = None  # the opener command when `run` is a wrapper


def _opener_spec(config: Config, opener_arg: str | None) -> OpenerSpec:
    """-o VALUE (or `opener`) → `openers` entry, else the value itself as a
    shell command; default $VISUAL → $EDITOR."""
    value = opener_arg or config.opener
    if value:
        return config.openers.get(value) or OpenerSpec(command=value)
    for var in ("VISUAL", "EDITOR"):
        if os.environ.get(var):
            return OpenerSpec(command=os.environ[var])
    raise WorkforestError(
        "no opener: pass -o, set `opener` in a config file, or export $VISUAL/$EDITOR"
    )


def _effective(config: Config, spec: OpenerSpec) -> tuple[CommandSpec, str | None]:
    """The command an opener entry runs — its own, or its `from` target's
    with `background` inherited unless overridden — and its wrapper name."""
    base = spec if spec.from_ is None else config.openers.get(spec.from_)
    if base is None or base.command is None:
        # load_config validates this; only a hand-built Config gets here
        raise WorkforestError(f"opener 'from: {spec.from_}' has no command")
    background = base.background if spec.background is None else spec.background
    return CommandSpec(base.command, bool(background)), spec.wrap


def resolve_opener(
    config: Config, opener_arg: str | None, wrap_arg: str | None = None
) -> ResolvedOpener:
    """Resolve the opener to what spawns. --wrap beats the opener's own
    `wrap`; an empty value means none."""
    command, wrap = _effective(config, _opener_spec(config, opener_arg))
    if wrap_arg is not None:
        wrap = wrap_arg
    if not wrap:
        return ResolvedOpener(command)
    wrapper = config.wrappers.get(wrap)
    if wrapper is None:
        known = ", ".join(sorted(config.wrappers)) or "none defined"
        raise WorkforestError(f"unknown wrapper {wrap!r} (available: {known})")
    return ResolvedOpener(wrapper, inner=command.command)


def describe_opener(config: Config, name: str) -> str:
    """One-line summary for completions and the TUI: the command text, plus
    the wrapper when there is one."""
    command, wrap = _effective(config, config.openers.get(name) or OpenerSpec(command=name))
    return f"{command.command} via {wrap}" if wrap else command.command


def launch_vars(
    *,
    main: Path,
    worktree: Path,
    worktrees_dir: Path,
    branch: str | None,
    target: str,
) -> dict[str, str]:
    """The full WF_* family for a launch: the scripts' structural five, the
    launch-only WF_TARGET and WF_TITLE, and WF_ENV — all of the above as
    shell-quoted assignments, for a command string that crosses into a
    process that does not inherit this environment (a tmux server, ssh):
    `"export $WF_ENV; $WF_COMMAND"` re-creates the family on the far side."""
    family = {
        "WF_MAIN": str(main),
        "WF_NAME": main.name,
        "WF_WORKTREES_DIR": str(worktrees_dir),
        "WF_WORKTREE": str(worktree),
        "WF_BRANCH": branch or "",
        "WF_TARGET": target,
        "WF_TITLE": f"{main.name}: {worktree.name}",
    }
    family["WF_ENV"] = _assignments(family)
    return family


def _assignments(variables: dict[str, str]) -> str:
    """`NAME=value ...`, each value shell-quoted."""
    return " ".join(f"{name}={shlex.quote(value)}" for name, value in variables.items())


def launch(
    config: Config,
    *,
    main: Path,
    worktree: Path,
    worktrees_dir: Path,
    branch: str | None,
    opener_arg: str | None = None,
    wrap_arg: str | None = None,
    path_arg: str | None = None,
) -> ShellAction | None:
    """Open a worktree: a ShellAction for the user's terminal, or a detached
    spawn (returning None) when what spawns is `background`.

    The launch cwd is always the worktree root; -p only sets WF_TARGET,
    the opener's argument.
    """
    resolved = resolve_opener(config, opener_arg, wrap_arg)
    target = path_arg if path_arg else "."
    variables = launch_vars(
        main=main,
        worktree=worktree,
        worktrees_dir=worktrees_dir,
        branch=branch,
        target=target,
    )
    if resolved.inner is not None:
        variables["WF_COMMAND"] = resolved.inner
    command = resolved.run.command
    if resolved.run.background:
        spawn_background(command, variables=variables, cwd=worktree)
        output.success(f"opened {worktree.name} in the background")
        return None
    # Prefix assignments scope WF_* to the child shell alone — nothing leaks
    # into (or goes stale in) the user's interactive shell — and that child
    # shell, not workforest, expands the command with WF_* in its
    # environment.
    runner = f"{shlex.quote(_user_shell())} -c {shlex.quote(command)}"
    return ShellAction(f"cd {shlex.quote(str(worktree))} && {_assignments(variables)} {runner}")


def scrub_activation_state(env: dict[str, str]) -> dict[str, str]:
    """Drop venv/conda/nvm/rvm activation inherited from the invoking shell.

    A spawned window is a fresh context: the tools' `deactivate` counterparts
    are shell functions that don't exist there, so inherited activation is
    unremovable and points at the wrong worktree's environment. Prompt-hook
    managers (direnv, mise, asdf) re-derive their state in the new shell and
    need no help; nix-shell is left alone because on NixOS its PATH entries
    are indistinguishable from the system PATH.
    """
    env = dict(env)
    stale_dirs: set[str] = set()
    for activation in _ACTIVATION_STATE:
        prefix = env.pop(activation.prefix_var, None)
        for name in activation.companion_vars:
            env.pop(name, None)
        if prefix:
            stale_dirs.update(
                str(Path(prefix) / sub) if sub else prefix for sub in activation.bin_subdirs
            )
    for var in [name for name in env if _CONDA_STACK.fullmatch(name)]:
        stale_dirs.add(str(Path(env.pop(var)) / "bin"))
    path = env.get("PATH")
    if path and stale_dirs:
        env["PATH"] = os.pathsep.join(
            entry for entry in path.split(os.pathsep) if entry not in stale_dirs
        )
    return env


# How long a background command gets to fail: long enough to catch argv/env/
# display errors, short enough to be imperceptible next to a window opening.
_GRACE_SECONDS = 0.3


def _describe_exit(returncode: int) -> str:
    if returncode < 0:
        try:
            name = signal.Signals(-returncode).name
        except ValueError:
            name = f"signal {-returncode}"
        return f"was killed by {name}"
    return f"exited with status {returncode}"


def spawn_background(command: str, *, variables: dict[str, str], cwd: Path) -> None:
    """Spawn a `background` command fully detached, via `$SHELL -c`.

    The shell — not workforest — expands the command, with the WF_* family
    (plus WF_COMMAND, the still-unexpanded opener command, when a wrapper is
    involved) in its environment. The inherited environment is passed
    through scrub_activation_state first.

    Detached is not silent: stderr goes to an unlinked temp file (a pipe
    would SIGPIPE a long-lived window once we exit), and a command that dies
    within the grace period is reported with that stderr instead of failing
    invisibly. A quick clean exit is fine — clients that hand off to a
    daemon (`code .`, `kitty @ launch`) look exactly like that.
    """
    shell = _user_shell()
    with tempfile.TemporaryFile() as stderr_file:
        try:
            process = subprocess.Popen(
                [shell, "-c", command],
                cwd=cwd,
                env=scrub_activation_state({**os.environ, **variables}),
                start_new_session=True,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=stderr_file,
            )
        except OSError as exc:
            raise WorkforestError(
                f"cannot run the opener via $SHELL ({shell!r}): {exc.strerror or exc}"
            ) from exc
        try:
            returncode = process.wait(timeout=_GRACE_SECONDS)
        except subprocess.TimeoutExpired:
            return  # still running: the window is up or coming up
        if returncode == 0:
            return
        stderr_file.seek(0)
        tail = stderr_file.read().decode(errors="replace").strip().splitlines()[-10:]
        message = f"opener {_describe_exit(returncode)} right after launch"
        if tail:
            message += ":\n" + "\n".join(tail)
        raise WorkforestError(message)
