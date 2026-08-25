"""`workforest shell-init [bash|zsh]`: print the wf wrapper + completions
(+ a $MANPATH entry for installs whose man pages man(1) would not find)."""

import importlib.resources
import os
import shlex
import sys
from pathlib import Path, PurePath

from workforest.errors import WorkforestError

# Prefixes whose share/man is on every man(1) default search path already.
SYSTEM_PREFIXES = frozenset({Path("/usr"), Path("/usr/local")})


def _resource(name: str) -> str:
    return (importlib.resources.files("workforest") / "shell" / name).read_text()


def detect_shell() -> str:
    shell = PurePath(os.environ.get("SHELL", "")).name
    if shell in ("bash", "zsh"):
        return shell
    raise WorkforestError(
        f"cannot detect shell from $SHELL ({shell or 'unset'}); "
        "pass one explicitly: workforest shell-init bash|zsh"
    )


def manpath_snippet(prefix: Path) -> str:
    """Shell lines putting PREFIX/share/man on $MANPATH, or "" when man(1)
    searches it anyway (system prefixes) or our pages are not there (a
    package manager relocated them).

    The wheel ships the pages as share/man data, so a `uv tool`/pipx venv
    holds them where no default man path looks. Idempotent for repeated
    evals; the trailing colon when MANPATH was unset means "then the
    system default" to both man-db and BSD/macOS man.
    """
    man_dir = prefix / "share" / "man"
    if prefix in SYSTEM_PREFIXES or not (man_dir / "man1" / "workforest.1").is_file():
        return ""
    quoted = shlex.quote(str(man_dir))
    return (
        "# workforest's man pages live in its Python environment, off the default\n"
        "# man path; a trailing colon keeps the system path when MANPATH was unset.\n"
        f'case ":${{MANPATH-}}:" in\n'
        f'    *":"{quoted}":"*) ;;\n'
        f'    *) export MANPATH={quoted}":${{MANPATH-}}" ;;\n'
        "esac\n"
    )


def shell_init(shell: str | None) -> str:
    shell = shell or detect_shell()
    completion = _resource("completion.bash" if shell == "bash" else "completion.zsh")
    parts = [_resource("workforest.sh"), manpath_snippet(Path(sys.prefix)), completion]
    return "\n".join(part for part in parts if part)
