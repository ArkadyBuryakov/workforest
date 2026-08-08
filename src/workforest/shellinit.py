"""`workforest shell-init [bash|zsh]`: print the wf wrapper + completions."""

import importlib.resources
import os
from pathlib import PurePath

from workforest.errors import WorkforestError


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


def shell_init(shell: str | None) -> str:
    shell = shell or detect_shell()
    completion = _resource("completion.bash" if shell == "bash" else "completion.zsh")
    return _resource("workforest.sh") + "\n" + completion
