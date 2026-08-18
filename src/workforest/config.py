"""Configuration: schema, layered loading, merging, template resolution.

Layers (low → high): built-in defaults → system → user →
project-shared (main worktree root) → project-local (.vscode/ then .idea/) →
environment → CLI flags (applied by commands, not here).
"""

import json
import os
import string
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import yaml

from workforest.errors import ConfigError

SYSTEM_CONFIG_DIR = Path("/etc/workforest")
GLOBAL_BASENAMES = ("config.yaml", "config.yml", "config.json")
PROJECT_BASENAMES = (".workforest.yaml", ".workforest.yml", ".workforest.json")
PROJECT_LOCAL_DIRS = (".vscode", ".idea")


@dataclass(slots=True, frozen=True)
class _FieldSpec:
    """Kinds: "str", "list", "map" (str -> str, where a null value deletes
    the inherited entry during merge)."""

    kind: str
    default: Any


_SCHEMA: dict[str, _FieldSpec] = {
    "worktrees_dir": _FieldSpec("str", "$WF_MAIN/../worktrees/$WF_NAME"),
    "opener": _FieldSpec("str", ""),
    "openers": _FieldSpec("map", {}),
    "window_command": _FieldSpec("str", ""),
    "symlinks": _FieldSpec("list", []),
    "setup_scripts": _FieldSpec("list", []),
    "scripts": _FieldSpec("map", {}),
}


@dataclass(slots=True, frozen=True)
class ConfigSource:
    layer: str  # "system", "user", "project", or "project-local"
    path: Path


@dataclass(slots=True)
class Config:
    worktrees_dir: str = "$WF_MAIN/../worktrees/$WF_NAME"
    opener: str = ""
    openers: dict[str, str] = field(default_factory=dict)
    window_command: str = ""
    symlinks: list[str] = field(default_factory=list)
    setup_scripts: list[str] = field(default_factory=list)
    scripts: dict[str, str] = field(default_factory=dict)
    sources: list[ConfigSource] = field(default_factory=list)

    def as_dict(self) -> dict[str, Any]:
        return {
            "worktrees_dir": self.worktrees_dir,
            "opener": self.opener,
            "openers": self.openers,
            "window_command": self.window_command,
            "symlinks": self.symlinks,
            "setup_scripts": self.setup_scripts,
            "scripts": self.scripts,
        }


def _parse_file(path: Path) -> dict[str, Any]:
    text = path.read_text()
    if path.suffix == ".json":
        try:
            data = json.loads(text)
        except json.JSONDecodeError as exc:
            raise ConfigError(f"{path}: invalid JSON: {exc}") from exc
    else:
        try:
            data = yaml.safe_load(text)
        except yaml.YAMLError as exc:
            raise ConfigError(f"{path}: invalid YAML: {exc}") from exc
    if data is None:
        return {}
    if not isinstance(data, dict):
        raise ConfigError(f"{path}: top level must be a mapping, got {type(data).__name__}")
    return data


def _validate(data: dict[str, Any], path: Path) -> None:
    for key, value in data.items():
        if key not in _SCHEMA:
            known = ", ".join(sorted(_SCHEMA))
            raise ConfigError(f"{path}: unknown key {key!r} (known keys: {known})")
        match _SCHEMA[key].kind:
            case "str":
                if not isinstance(value, str):
                    raise ConfigError(
                        f"{path}: {key!r} must be a string, got {type(value).__name__}"
                    )
            case "list":
                if not isinstance(value, list) or not all(isinstance(v, str) for v in value):
                    raise ConfigError(f"{path}: {key!r} must be a list of strings")
            case "map":
                if not isinstance(value, dict) or not all(
                    isinstance(k, str) and (v is None or isinstance(v, str))
                    for k, v in value.items()
                ):
                    raise ConfigError(
                        f"{path}: {key!r} must be a mapping of string to string (or null)"
                    )


def _merge(base: dict[str, Any], overlay: dict[str, Any]) -> dict[str, Any]:
    """Scalars/lists replace; mappings merge per key with null deleting."""
    merged = dict(base)
    for key, value in overlay.items():
        if _SCHEMA[key].kind == "map":
            combined: dict[str, str] = dict(merged.get(key, {}))
            for name, entry in value.items():
                if entry is None:
                    combined.pop(name, None)
                else:
                    combined[name] = entry
            merged[key] = combined
        else:
            merged[key] = value
    return merged


def _first_existing(directory: Path, basenames: tuple[str, ...]) -> Path | None:
    for basename in basenames:
        candidate = directory / basename
        if candidate.is_file():
            return candidate
    return None


def _layer_files(main_worktree: Path | None) -> list[ConfigSource]:
    layers: list[ConfigSource] = []
    if found := _first_existing(SYSTEM_CONFIG_DIR, GLOBAL_BASENAMES):
        layers.append(ConfigSource("system", found))
    xdg = os.environ.get("XDG_CONFIG_HOME")
    user_dir = (Path(xdg) if xdg else Path.home() / ".config") / "workforest"
    if found := _first_existing(user_dir, GLOBAL_BASENAMES):
        layers.append(ConfigSource("user", found))
    if main_worktree is not None:
        if found := _first_existing(main_worktree, PROJECT_BASENAMES):
            layers.append(ConfigSource("project", found))
        for local_dir in PROJECT_LOCAL_DIRS:
            if found := _first_existing(main_worktree / local_dir, PROJECT_BASENAMES):
                layers.append(ConfigSource("project-local", found))
                break
    return layers


def load_config(main_worktree: Path | None = None) -> Config:
    """Load and merge all layers; main_worktree=None skips project layers."""
    merged = {key: spec.default for key, spec in _SCHEMA.items()}
    sources: list[ConfigSource] = []
    for source in _layer_files(main_worktree):
        data = _parse_file(source.path)
        _validate(data, source.path)
        merged = _merge(merged, data)
        sources.append(source)

    # Set-but-empty is meaningful: WORKFOREST_WINDOW_COMMAND="" forces the
    # current-shell mode in sessions where the configured window_command
    # doesn't apply (ssh, plain tty); likewise an empty WORKFOREST_OPENER
    # resets to the $VISUAL/$EDITOR chain.
    if (opener := os.environ.get("WORKFOREST_OPENER")) is not None:
        merged["opener"] = opener
    if (window := os.environ.get("WORKFOREST_WINDOW_COMMAND")) is not None:
        merged["window_command"] = window

    return Config(**merged, sources=sources)


def template_vars(main_worktree: Path) -> dict[str, str]:
    """The WF_* family as template variables."""
    return {
        "WF_MAIN": str(main_worktree),
        "WF_NAME": main_worktree.name,
    }


def resolve_worktrees_dir(config: Config, main_worktree: Path) -> Path:
    """Expand $WF_* and environment variables, then normalize the path."""
    mapping = {**os.environ, **template_vars(main_worktree)}
    try:
        expanded = string.Template(config.worktrees_dir).substitute(mapping)
    except (KeyError, ValueError) as exc:
        raise ConfigError(f"worktrees_dir {config.worktrees_dir!r}: {exc}") from exc
    path = Path(expanded).expanduser()
    if not path.is_absolute():
        path = main_worktree / path
    return Path(os.path.normpath(path))
