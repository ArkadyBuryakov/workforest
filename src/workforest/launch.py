"""Opener/window template resolution and cd-protocol emission.

No tool names appear here: what to run comes from config templates and the
$VISUAL/$EDITOR contract; where to run it is the current shell (ShellAction)
unless the user configured a window_command template.

Templates and launched processes share one variable family, WF_*:

* `$WF_X` (and any environment variable) is substituted as raw text at
  template time, so it word-splits into multiple arguments;
* `{x}` is the shell-quoted form of `$WF_X` — always exactly one argument;
* the launched process receives the same family as environment variables
  (Popen env in the window path, prefix assignments in the shell path).
"""

import os
import re
import shlex
import string
import subprocess
from dataclasses import dataclass
from pathlib import Path

from workforest.config import Config
from workforest.errors import WorkforestError

_PLACEHOLDER = re.compile(r"\{([a-z_]+)\}")


@dataclass(slots=True, frozen=True)
class _Activation:
    """One tool's shell-session activation state that must not leak into a
    spawned window. An empty bin subdir means the prefix variable is itself
    the PATH entry."""

    prefix_var: str
    bin_subdirs: tuple[str, ...]  # subdirs under the prefix that activation put on PATH
    companion_vars: tuple[str, ...]  # variables set alongside the prefix


_ACTIVATION_STATE = (
    _Activation("VIRTUAL_ENV", ("bin", "Scripts"), ("VIRTUAL_ENV_PROMPT",)),
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


def resolve_opener_template(config: Config, opener_arg: str | None) -> str:
    """-o NAME → `openers` lookup, else verbatim; default $VISUAL → $EDITOR."""
    value = opener_arg or config.opener
    if value:
        return config.openers.get(value, value)
    for var in ("VISUAL", "EDITOR"):
        if os.environ.get(var):
            return os.environ[var]
    raise WorkforestError(
        "no opener: pass -o, set `opener` in a config file, or export $VISUAL/$EDITOR"
    )


def launch_vars(
    *,
    main: Path,
    worktree: Path,
    worktrees_dir: Path,
    branch: str | None,
    target: str,
) -> dict[str, str]:
    """The full WF_* family for a launch: the scripts'
    structural five plus the launch-only WF_TARGET and WF_TITLE."""
    return {
        "WF_MAIN": str(main),
        "WF_NAME": main.name,
        "WF_WORKTREES_DIR": str(worktrees_dir),
        "WF_WORKTREE": str(worktree),
        "WF_BRANCH": branch or "",
        "WF_TARGET": target,
        "WF_TITLE": f"{main.name}: {worktree.name}",
    }


def expand_template(template: str, variables: dict[str, str]) -> str:
    """`$WF_X`/`$ENV` insert raw text; `{x}` inserts $WF_X shell-quoted.

    Raw substitution never touches the quoted insertions: the template is
    split on `{x}` placeholders and only the literal segments go through
    string.Template, so values containing `$` or braces are inert.
    """
    mapping = {**os.environ, **variables}
    quoted = {name.removeprefix("WF_").lower(): value for name, value in variables.items()}
    parts: list[str] = []
    pos = 0
    for match in _PLACEHOLDER.finditer(template):
        parts.append(string.Template(template[pos : match.start()]).safe_substitute(mapping))
        name = match.group(1)
        if name not in quoted:
            known = ", ".join("{" + key + "}" for key in sorted(quoted))
            raise WorkforestError(f"unknown placeholder {{{name}}} (known: {known})")
        parts.append(shlex.quote(quoted[name]))
        pos = match.end()
    parts.append(string.Template(template[pos:]).safe_substitute(mapping))
    return "".join(parts)


def launch(
    config: Config,
    *,
    main: Path,
    worktree: Path,
    worktrees_dir: Path,
    branch: str | None,
    opener_arg: str | None = None,
    path_arg: str | None = None,
) -> ShellAction | None:
    """Open a worktree: a ShellAction for the current shell, or a detached
    window spawn (returning None) when window_command is configured.

    The launch cwd is always the worktree root; -p only sets WF_TARGET,
    the opener's argument.
    """
    template = resolve_opener_template(config, opener_arg)
    target = path_arg if path_arg else "."
    variables = launch_vars(
        main=main,
        worktree=worktree,
        worktrees_dir=worktrees_dir,
        branch=branch,
        target=target,
    )
    command = expand_template(template, variables)
    if config.window_command:
        spawn_window(config.window_command, variables=variables, command=command, cwd=worktree)
        return None
    # Prefix assignments scope WF_* to the opener command alone — nothing
    # leaks into (or goes stale in) the user's interactive shell.
    assignments = " ".join(f"{name}={shlex.quote(value)}" for name, value in variables.items())
    return ShellAction(f"cd {shlex.quote(str(worktree))} && {assignments} {command}")


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


def spawn_window(
    window_template: str,
    *,
    variables: dict[str, str],
    command: str,
    cwd: Path,
) -> None:
    """Spawn the user's window_command fully detached.

    The resolved opener command joins the family as {command} (one argument,
    e.g. for `$SHELL -c {command}`) / `$WF_COMMAND` (spliced into argv words);
    it is template-only and not exported to the environment. The inherited
    environment is passed through scrub_activation_state first.
    """
    expanded = expand_template(window_template, {**variables, "WF_COMMAND": command})
    argv = shlex.split(expanded)
    if not argv:
        raise WorkforestError("window_command expanded to an empty command")
    try:
        subprocess.Popen(
            argv,
            cwd=cwd,
            env=scrub_activation_state({**os.environ, **variables}),
            start_new_session=True,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except FileNotFoundError as exc:
        raise WorkforestError(f"window_command program not found: {argv[0]!r}") from exc
