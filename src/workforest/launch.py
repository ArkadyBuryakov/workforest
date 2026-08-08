"""Opener/window template resolution and cd-protocol emission (DESIGN §3.4).

No tool names appear here: what to run comes from config templates and the
$VISUAL/$EDITOR contract; where to run it is the current shell (ShellAction)
unless the user configured a window_command template.
"""

import os
import shlex
import string
import subprocess
from dataclasses import dataclass
from pathlib import Path

from workforest.config import Config
from workforest.errors import WorkforestError


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


def resolve_target(worktree: Path, path_arg: str | None) -> tuple[Path, str]:
    """-p resolution: directories become cwd, files stay opener arguments."""
    if path_arg in (None, "", "."):
        return worktree, "."
    assert path_arg is not None
    target = worktree / path_arg
    if target.is_dir():
        return target, "."
    return worktree, path_arg


def _expand_env(template: str) -> str:
    return string.Template(template).safe_substitute(os.environ)


def build_opener_command(template: str, path_arg: str) -> str:
    expanded = _expand_env(template)
    if "{path}" in expanded:
        return expanded.replace("{path}", shlex.quote(path_arg))
    return expanded


def launch(
    config: Config,
    *,
    worktree: Path,
    repo_name: str,
    opener_arg: str | None = None,
    path_arg: str | None = None,
) -> ShellAction | None:
    """Open a worktree: a ShellAction for the current shell, or a detached
    window spawn (returning None) when window_command is configured."""
    template = resolve_opener_template(config, opener_arg)
    cwd, resolved_path = resolve_target(worktree, path_arg)
    command = build_opener_command(template, resolved_path)
    if config.window_command:
        title = f"{repo_name}: {worktree.name}"
        spawn_window(config.window_command, title=title, cwd=cwd, command=command)
        return None
    return ShellAction(f"cd {shlex.quote(str(cwd))} && {command}")


def spawn_window(window_template: str, *, title: str, cwd: Path, command: str) -> None:
    """Spawn the user's window_command fully detached (DESIGN §3.4)."""
    expanded = (
        _expand_env(window_template)
        .replace("{title}", shlex.quote(title))
        .replace("{path}", shlex.quote(str(cwd)))
        .replace("{command}", command)
    )
    argv = shlex.split(expanded)
    if not argv:
        raise WorkforestError("window_command expanded to an empty command")
    try:
        subprocess.Popen(
            argv,
            cwd=cwd,
            start_new_session=True,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except FileNotFoundError as exc:
        raise WorkforestError(f"window_command program not found: {argv[0]!r}") from exc
