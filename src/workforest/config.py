"""Configuration: schema, layered loading, merging, template resolution.

Layers (low → high): built-in defaults → system → user →
project-shared (main worktree root) → project-local (.vscode/ then .idea/) →
CLI flags (applied by commands, not here). Nothing in the environment
changes the result.
"""

import json
import os
import string
from dataclasses import dataclass, field, fields
from pathlib import Path
from typing import Any

import yaml

from workforest.errors import ConfigError

SYSTEM_CONFIG_DIR = Path("/etc/workforest")
GLOBAL_BASENAMES = ("config.yaml", "config.yml", "config.json")
PROJECT_BASENAMES = (".workforest.yaml", ".workforest.yml", ".workforest.json")
PROJECT_LOCAL_DIRS = (".vscode", ".idea")


@dataclass(slots=True, frozen=True)
class CommandSpec:
    """A `wrappers` entry, and what an opener resolves to: a shell command
    and where it runs."""

    command: str
    background: bool = False  # spawn detached instead of running in the user's terminal


@dataclass(slots=True, frozen=True)
class OpenerSpec:
    """An `openers` entry: a shell command of its own, or another opener's
    (`from`), optionally through a wrapper. Exactly one of `command`/`from_`
    is set; `from_` targets carry a `command` themselves (one level)."""

    command: str | None = None  # always a shell command, never a name
    from_: str | None = None  # an `openers` name (YAML key `from`)
    wrap: str | None = None  # a `wrappers` name; the wrapper then decides where it runs
    background: bool | None = None  # None: the `from` target's setting (own command: False)


@dataclass(slots=True, frozen=True)
class ScriptSpec:
    """A `scripts` entry: what `wf run NAME` runs — a shell command, or a
    group of other entries by name, `bulk` (in parallel) or `pipeline` (in
    order); exactly one of the three — where, whether only one instance may
    run per project, whether it is offered by name, and what to run once it
    has ended."""

    command: str | None = None
    bulk: tuple[str, ...] | None = None  # members run at once; done when all are
    pipeline: tuple[str, ...] | None = None  # members run in order; stops at the first failure
    background: bool = False  # detach, with output to a log file, instead of holding the terminal
    exclusive: bool = False  # starting it stops every running instance in the project
    hidden: bool = False  # left out of completions and the editor lists; still runs by name
    cleanup: str | None = None  # runs after the command ends, however it ended
    stop_timeout: float | None = None  # seconds between SIGTERM and SIGKILL; None: the global one

    @property
    def members(self) -> tuple[str, ...]:
        """The names a group runs; empty for a command."""
        return self.bulk or self.pipeline or ()


type ConfigEntry = CommandSpec | OpenerSpec | ScriptSpec


def _field_name(key: str) -> str:
    """YAML key → dataclass field (`from` is a Python keyword)."""
    return "from_" if key == "from" else key


def _key_name(field_name: str) -> str:
    return "from" if field_name == "from_" else field_name


@dataclass(slots=True, frozen=True)
class _FieldSpec:
    """Kinds: "str", "number", "list", "map" (a null value deletes the
    inherited entry during merge). Map entries normalize to the `entry` dataclass; a string
    is shorthand for `{command: <string>}`."""

    kind: str
    default: Any
    entry: type[ConfigEntry] | None = None


_SCHEMA: dict[str, _FieldSpec] = {
    "worktrees_dir": _FieldSpec("str", "$WF_MAIN/../worktrees/$WF_NAME"),
    "opener": _FieldSpec("str", ""),
    "openers": _FieldSpec("map", {}, OpenerSpec),
    "wrappers": _FieldSpec("map", {}, CommandSpec),
    "symlinks": _FieldSpec("list", []),
    "setup_scripts": _FieldSpec("list", []),
    "scripts": _FieldSpec("map", {}, ScriptSpec),
    "stop_timeout": _FieldSpec("number", 30.0),
}


@dataclass(slots=True, frozen=True)
class ConfigSource:
    layer: str  # "system", "user", "project", or "project-local"
    path: Path


@dataclass(slots=True)
class Config:
    worktrees_dir: str = "$WF_MAIN/../worktrees/$WF_NAME"
    opener: str = ""
    openers: dict[str, OpenerSpec] = field(default_factory=dict)
    wrappers: dict[str, CommandSpec] = field(default_factory=dict)
    symlinks: list[str] = field(default_factory=list)
    setup_scripts: list[str] = field(default_factory=list)
    scripts: dict[str, ScriptSpec] = field(default_factory=dict)
    stop_timeout: float = 30.0  # seconds a stopped script gets between SIGTERM and SIGKILL
    sources: list[ConfigSource] = field(default_factory=list)

    def as_dict(self) -> dict[str, Any]:
        return {
            "worktrees_dir": self.worktrees_dir,
            "opener": self.opener,
            "openers": {name: _entry_data(spec) for name, spec in self.openers.items()},
            "wrappers": {name: _entry_data(spec) for name, spec in self.wrappers.items()},
            "symlinks": self.symlinks,
            "setup_scripts": self.setup_scripts,
            "scripts": {name: _entry_data(spec) for name, spec in self.scripts.items()},
            "stop_timeout": self.stop_timeout,
        }


def _entry_data(spec: ConfigEntry) -> str | dict[str, Any]:
    """The config-file form of an entry: the bare command when that is all
    there is, else the mapping form with unset keys left out. An opener's
    explicit `background: false` is an override of its `from` target and
    is kept."""
    if isinstance(spec, CommandSpec):
        return {"command": spec.command, "background": True} if spec.background else spec.command
    if isinstance(spec, ScriptSpec):
        data: dict[str, Any] = {}
        if spec.command is not None:
            data["command"] = spec.command
        if spec.bulk is not None:
            data["bulk"] = list(spec.bulk)
        if spec.pipeline is not None:
            data["pipeline"] = list(spec.pipeline)
        if spec.background:
            data["background"] = True
        if spec.exclusive:
            data["exclusive"] = True
        if spec.hidden:
            data["hidden"] = True
        if spec.cleanup is not None:
            data["cleanup"] = spec.cleanup
        if spec.stop_timeout is not None:
            data["stop_timeout"] = spec.stop_timeout
        return data["command"] if list(data) == ["command"] else data
    data = {
        _key_name(f.name): value
        for f in fields(spec)
        if (value := getattr(spec, f.name)) is not None
    }
    return data["command"] if list(data) == ["command"] else data


def _normalize_entry(entry: type[ConfigEntry], value: str | dict[str, Any]) -> ConfigEntry:
    if isinstance(value, str):
        return entry(command=value)
    kwargs: dict[str, Any] = {
        _field_name(key): tuple(item) if isinstance(item, list) else item
        for key, item in value.items()
    }
    return entry(**kwargs)


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
            case "number":
                if not _is_positive_number(value):
                    raise ConfigError(f"{path}: {key!r} must be a number of seconds above 0")
            case "list":
                if not isinstance(value, list) or not all(isinstance(v, str) for v in value):
                    raise ConfigError(f"{path}: {key!r} must be a list of strings")
            case "map":
                if not isinstance(value, dict) or not all(isinstance(k, str) for k in value):
                    raise ConfigError(f"{path}: {key!r} must be a mapping")
                for name, entry in value.items():
                    _validate_entry(entry, key=key, name=name, spec=_SCHEMA[key], path=path)


def _is_positive_number(value: Any) -> bool:
    return isinstance(value, int | float) and not isinstance(value, bool) and value > 0


def _is_name_list(value: Any) -> bool:
    return (
        isinstance(value, list)
        and bool(value)
        and all(isinstance(item, str) and item.strip() for item in value)
    )


def _validate_entry(entry: Any, *, key: str, name: str, spec: _FieldSpec, path: Path) -> None:
    if entry is None:
        return
    where = f"{path}: {key}.{name}"
    if isinstance(entry, str):
        if not entry.strip():
            raise ConfigError(f"{where}: must not be empty")
        return
    assert spec.entry is not None  # every "map" field names its entry type
    known = [_key_name(f.name) for f in fields(spec.entry)]
    if not isinstance(entry, dict):
        raise ConfigError(f"{where}: must be a shell command or a mapping ({', '.join(known)})")
    for field_name in entry:
        if field_name not in known:
            raise ConfigError(
                f"{where}: unknown key {field_name!r} (known keys: {', '.join(known)})"
            )
    for field_name in ("command", "from"):
        if field_name in entry and not isinstance(entry[field_name], str):
            raise ConfigError(f"{where}: {field_name!r} must be a string")
    if "command" in entry and not entry["command"].strip():
        raise ConfigError(f"{where}: 'command' must not be empty")
    for field_name in ("bulk", "pipeline"):
        if field_name in entry and not _is_name_list(entry[field_name]):
            raise ConfigError(f"{where}: {field_name!r} must be a non-empty list of script names")
    if "wrap" in entry and not isinstance(entry["wrap"], str | None):
        raise ConfigError(f"{where}: 'wrap' must be a string")
    for field_name in ("background", "exclusive", "hidden"):
        if field_name in entry and not isinstance(entry[field_name], bool):
            raise ConfigError(f"{where}: {field_name!r} must be true or false")
    if "cleanup" in entry and (
        not isinstance(entry["cleanup"], str) or not entry["cleanup"].strip()
    ):
        raise ConfigError(f"{where}: 'cleanup' must be a non-empty string")
    if "stop_timeout" in entry and not _is_positive_number(entry["stop_timeout"]):
        raise ConfigError(f"{where}: 'stop_timeout' must be a number of seconds above 0")
    if spec.entry is OpenerSpec:
        if ("command" in entry) == ("from" in entry):
            raise ConfigError(f"{where}: exactly one of 'command' and 'from' is required")
    elif spec.entry is ScriptSpec:
        if sum(key in entry for key in ("command", "bulk", "pipeline")) != 1:
            raise ConfigError(
                f"{where}: exactly one of 'command', 'bulk', and 'pipeline' is required"
            )
    elif "command" not in entry:
        raise ConfigError(f"{where}: 'command' is required")
    if "wrap" in entry and "background" in entry:
        raise ConfigError(
            f"{where}: 'background' and 'wrap' are mutually exclusive "
            "(the wrapper decides where the command runs)"
        )


def _validate_references(
    openers: dict[str, OpenerSpec],
    wrappers: dict[str, CommandSpec],
    scripts: dict[str, ScriptSpec],
) -> None:
    """Cross-entry checks on the merged result: a `from` names an opener
    that has a command of its own (one level — no chains, so no cycles, and
    a target removed by a higher layer is caught here), a `wrap` names a
    wrapper, and a group's members name scripts, once each, without
    forming a cycle (a member removed by a higher layer is caught here)."""
    for name, spec in openers.items():
        where = f"openers.{name}"
        if spec.from_ is not None:
            target = openers.get(spec.from_)
            if target is None:
                known = ", ".join(sorted(openers)) or "none defined"
                raise ConfigError(f"{where}: 'from: {spec.from_}' names no opener (known: {known})")
            if target.command is None:
                raise ConfigError(
                    f"{where}: 'from: {spec.from_}' must name an opener with a command, "
                    f"but {spec.from_} has 'from: {target.from_}'"
                )
        if spec.wrap and spec.wrap not in wrappers:
            known = ", ".join(sorted(wrappers)) or "none defined"
            raise ConfigError(f"{where}: unknown wrapper {spec.wrap!r} (available: {known})")
    for name in scripts:
        _validate_members(scripts, name, trail=(name,))


def _validate_members(scripts: dict[str, ScriptSpec], name: str, *, trail: tuple[str, ...]) -> None:
    where = f"scripts.{name}"
    members = scripts[name].members
    for member in members:
        if member not in scripts:
            known = ", ".join(sorted(scripts))
            raise ConfigError(f"{where}: member {member!r} names no script (known: {known})")
        if members.count(member) > 1:
            raise ConfigError(f"{where}: member {member!r} is listed more than once")
        if member in trail:
            chain = " -> ".join((*trail, member))
            raise ConfigError(f"{where}: groups form a cycle: {chain}")
        _validate_members(scripts, member, trail=(*trail, member))


def _merge(base: dict[str, Any], overlay: dict[str, Any]) -> dict[str, Any]:
    """Scalars/lists replace; mappings merge per key with null deleting."""
    merged = dict(base)
    for key, value in overlay.items():
        if _SCHEMA[key].kind == "map":
            combined: dict[str, Any] = dict(merged.get(key, {}))
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

    for key, spec in _SCHEMA.items():
        if spec.entry is not None:
            merged[key] = {
                name: _normalize_entry(spec.entry, value) for name, value in merged[key].items()
            }
    _validate_references(merged["openers"], merged["wrappers"], merged["scripts"])
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
