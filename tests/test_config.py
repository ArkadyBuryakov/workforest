"""config: layering, merge semantics, validation, template resolution."""

import os
from pathlib import Path

import pytest

from workforest import config as config_mod
from workforest.config import (
    CommandSpec,
    Config,
    OpenerSpec,
    load_config,
    resolve_worktrees_dir,
)
from workforest.errors import ConfigError


def write_user_config(content: str, basename: str = "config.yaml") -> Path:
    user_dir = Path(os.environ["XDG_CONFIG_HOME"]) / "workforest"
    user_dir.mkdir(parents=True, exist_ok=True)
    path = user_dir / basename
    path.write_text(content)
    return path


def write_system_config(content: str, basename: str = "config.yaml") -> Path:
    config_mod.SYSTEM_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    path = config_mod.SYSTEM_CONFIG_DIR / basename
    path.write_text(content)
    return path


def make_project(tmp_path: Path) -> Path:
    project = tmp_path / "dev" / "api"
    project.mkdir(parents=True, exist_ok=True)
    return project


class TestDefaults:
    def test_no_files_yields_defaults(self) -> None:
        cfg = load_config()
        assert cfg.worktrees_dir == "$WF_MAIN/../worktrees/$WF_NAME"
        assert cfg.opener == ""
        assert cfg.openers == {}
        assert cfg.wrappers == {}
        assert cfg.symlinks == []
        assert cfg.setup_scripts == []
        assert cfg.scripts == {}
        assert cfg.sources == []


class TestLayering:
    def test_precedence_system_user_project_local(self, tmp_path: Path) -> None:
        write_system_config("opener: system\nwrappers:\n  win: from-system\n")
        write_user_config("opener: user\n")
        project = make_project(tmp_path)
        (project / ".workforest.yaml").write_text("opener: project\n")
        (project / ".vscode").mkdir()
        (project / ".vscode" / ".workforest.yaml").write_text("opener: local\n")

        cfg = load_config(project)
        assert cfg.opener == "local"
        # keys not set by higher layers survive from lower ones
        assert cfg.wrappers == {"win": CommandSpec("from-system")}
        assert [s.layer for s in cfg.sources] == [
            "system",
            "user",
            "project",
            "project-local",
        ]

    def test_project_layers_skipped_without_main(self, tmp_path: Path) -> None:
        project = make_project(tmp_path)
        (project / ".workforest.yaml").write_text("opener: project\n")
        cfg = load_config()
        assert cfg.opener == ""

    def test_basename_order_yaml_yml_json(self, tmp_path: Path) -> None:
        project = make_project(tmp_path)
        (project / ".workforest.yml").write_text("opener: from-yml\n")
        (project / ".workforest.json").write_text('{"opener": "from-json"}')
        assert load_config(project).opener == "from-yml"
        (project / ".workforest.yaml").write_text("opener: from-yaml\n")
        assert load_config(project).opener == "from-yaml"

    def test_vscode_wins_over_idea(self, tmp_path: Path) -> None:
        project = make_project(tmp_path)
        for local_dir, value in ((".idea", "idea"), (".vscode", "vscode")):
            (project / local_dir).mkdir()
            (project / local_dir / ".workforest.yaml").write_text(f"opener: {value}\n")
        assert load_config(project).opener == "vscode"

    def test_idea_used_when_no_vscode(self, tmp_path: Path) -> None:
        project = make_project(tmp_path)
        (project / ".idea").mkdir()
        (project / ".idea" / ".workforest.yaml").write_text("opener: idea\n")
        cfg = load_config(project)
        assert cfg.opener == "idea"
        assert cfg.sources[-1].layer == "project-local"

    def test_local_overrides_only_its_keys(self, tmp_path: Path) -> None:
        project = make_project(tmp_path)
        (project / ".workforest.yaml").write_text("symlinks: [node_modules]\nopener: shared\n")
        (project / ".idea").mkdir()
        (project / ".idea" / ".workforest.yaml").write_text("opener: mine\n")
        cfg = load_config(project)
        assert cfg.opener == "mine"
        assert cfg.symlinks == ["node_modules"]

    def test_json_project_config(self, tmp_path: Path) -> None:
        project = make_project(tmp_path)
        (project / ".workforest.json").write_text('{"scripts": {"test": "make test"}}')
        assert load_config(project).scripts == {"test": "make test"}


class TestMergeSemantics:
    def test_lists_replace(self, tmp_path: Path) -> None:
        write_user_config("symlinks: [a, b]\n")
        project = make_project(tmp_path)
        (project / ".workforest.yaml").write_text("symlinks: []\n")
        assert load_config(project).symlinks == []

    def test_mappings_merge_per_key(self, tmp_path: Path) -> None:
        write_user_config("scripts:\n  sync: git fetch\n  build: make\n")
        project = make_project(tmp_path)
        (project / ".workforest.yaml").write_text("scripts:\n  build: npm run build\n")
        cfg = load_config(project)
        assert cfg.scripts == {"sync": "git fetch", "build": "npm run build"}

    def test_null_deletes_mapping_entry(self, tmp_path: Path) -> None:
        write_user_config("scripts:\n  sync: git fetch\n")
        project = make_project(tmp_path)
        (project / ".workforest.yaml").write_text("scripts:\n  sync: null\n")
        assert load_config(project).scripts == {}

    def test_openers_merge_like_scripts(self, tmp_path: Path) -> None:
        write_system_config('openers:\n  edit: $EDITOR "$WF_TARGET"\n')
        write_user_config("openers:\n  git: lazygit\n")
        cfg = load_config()
        assert cfg.openers == {
            "edit": OpenerSpec('$EDITOR "$WF_TARGET"'),
            "git": OpenerSpec("lazygit"),
        }

    def test_entry_mappings_replace_whole(self, tmp_path: Path) -> None:
        # An entry is one value: a higher layer's mapping replaces the lower
        # layer's mapping outright, never merges field by field.
        write_user_config(
            "openers:\n  edit: $EDITOR\n  win: {from: edit, wrap: kitty}\nwrappers:\n  kitty: k\n"
        )
        project = make_project(tmp_path)
        (project / ".workforest.yaml").write_text("openers:\n  win: {command: x}\n")
        assert load_config(project).openers["win"] == OpenerSpec("x")


class TestEntries:
    def test_string_is_shorthand_for_command(self, tmp_path: Path) -> None:
        write_user_config(
            "openers:\n  edit: '$EDITOR \"$WF_TARGET\"'\n"
            'wrappers:\n  direnv: \'direnv exec "$WF_WORKTREE" $SHELL -c "$WF_COMMAND"\'\n'
        )
        cfg = load_config()
        assert cfg.openers == {"edit": OpenerSpec('$EDITOR "$WF_TARGET"')}
        assert cfg.wrappers == {
            "direnv": CommandSpec('direnv exec "$WF_WORKTREE" $SHELL -c "$WF_COMMAND"')
        }

    def test_mapping_form(self, tmp_path: Path) -> None:
        write_user_config(
            "openers:\n"
            "  edit: '$EDITOR \"$WF_TARGET\"'\n"
            "  code: {command: 'code \"$WF_WORKTREE\"', background: true}\n"
            "  win: {from: edit, wrap: kitty}\n"
            "  here: {from: code, background: false}\n"
            "  plain: {command: x, wrap: null}\n"
            "wrappers:\n  kitty: {command: 'kitty $SHELL -c \"$WF_COMMAND\"', background: true}\n"
        )
        cfg = load_config()
        assert cfg.wrappers == {"kitty": CommandSpec('kitty $SHELL -c "$WF_COMMAND"', True)}
        assert cfg.openers == {
            "edit": OpenerSpec('$EDITOR "$WF_TARGET"'),
            "code": OpenerSpec('code "$WF_WORKTREE"', background=True),
            "win": OpenerSpec(from_="edit", wrap="kitty"),
            "here": OpenerSpec(from_="code", background=False),
            "plain": OpenerSpec("x"),  # wrap: null is "no wrapper"
        }

    def test_as_dict_round_trips_to_config_form(self, tmp_path: Path) -> None:
        write_user_config(
            "openers:\n  edit: '$EDITOR'\n  code: {command: code, background: true}\n"
            "  win: {from: edit, wrap: kitty}\n  here: {from: code, background: false}\n"
            "  git: lazygit\n"
            "wrappers:\n  kitty: {command: kitty, background: true}\n  direnv: direnv\n"
        )
        data = load_config().as_dict()
        assert "commands" not in data
        assert data["wrappers"] == {
            "kitty": {"command": "kitty", "background": True},
            "direnv": "direnv",
        }
        assert data["openers"] == {
            "edit": "$EDITOR",
            "code": {"command": "code", "background": True},
            "win": {"from": "edit", "wrap": "kitty"},
            "here": {"from": "code", "background": False},
            "git": "lazygit",
        }

    @pytest.mark.parametrize(
        ("content", "message"),
        [
            ("openers:\n  win: [a, b]\n", "openers.win: must be a shell command or a mapping"),
            ("openers:\n  win: ''\n", "openers.win: must not be empty"),
            ("openers:\n  win: {command: ' '}\n", "openers.win: 'command' must not be empty"),
            (
                "openers:\n  win: {wrap: kitty}\n",
                "openers.win: exactly one of 'command' and 'from' is required",
            ),
            (
                "openers:\n  win: {command: x, from: y}\n",
                "openers.win: exactly one of 'command' and 'from' is required",
            ),
            ("openers:\n  win: {command: x, mode: y}\n", "openers.win: unknown key 'mode'"),
            ("wrappers:\n  k: {command: x, wrap: y}\n", "wrappers.k: unknown key 'wrap'"),
            ("wrappers:\n  k: {from: x}\n", "wrappers.k: unknown key 'from'"),
            ("wrappers:\n  k: {background: true}\n", "wrappers.k: 'command' is required"),
            (
                "openers:\n  code: {command: x, background: yes please}\n",
                "openers.code: 'background' must be true or false",
            ),
            ("wrappers:\n  k: {command: 1}\n", "wrappers.k: 'command' must be a string"),
            ("openers:\n  win: {from: 1}\n", "openers.win: 'from' must be a string"),
            ("openers:\n  win: {command: x, wrap: 1}\n", "openers.win: 'wrap' must be a string"),
            (
                "openers:\n  win: {command: x, wrap: k, background: true}\n",
                "'background' and 'wrap' are mutually exclusive",
            ),
            ("scripts:\n  test: {command: x}\n", "'scripts' must be a mapping of string to string"),
        ],
    )
    def test_invalid_entries(self, tmp_path: Path, content: str, message: str) -> None:
        project = make_project(tmp_path)
        (project / ".workforest.yaml").write_text(content)
        with pytest.raises(ConfigError, match=message):
            load_config(project)


class TestReferences:
    """`from` and `wrap` names are checked on the merged config."""

    def test_resolve_across_layers(self, tmp_path: Path) -> None:
        write_system_config("openers:\n  edit: $EDITOR\n")
        write_user_config("openers:\n  win: {from: edit, wrap: kitty}\nwrappers:\n  kitty: k\n")
        assert load_config().openers["win"] == OpenerSpec(from_="edit", wrap="kitty")

    @pytest.mark.parametrize(
        ("content", "message"),
        [
            (
                "openers:\n  win: {from: edit}\n",
                r"openers.win: 'from: edit' names no opener \(known: win\)",
            ),
            (
                "openers:\n  a: {from: b}\n  b: {from: c}\n  c: x\n",
                "openers.a: 'from: b' must name an opener with a command, but b has 'from: c'",
            ),
            ("openers:\n  a: {from: a}\n", "openers.a: .* but a has 'from: a'"),
            ("openers:\n  a: {from: b}\n  b: {from: a}\n", "openers.a: .* but b has 'from: a'"),
            (
                "openers:\n  win: {command: x, wrap: kity}\nwrappers:\n  kitty: k\n",
                r"openers.win: unknown wrapper 'kity' \(available: kitty\)",
            ),
            (
                "openers:\n  win: {command: x, wrap: kitty}\n",
                r"openers.win: unknown wrapper 'kitty' \(available: none defined\)",
            ),
        ],
    )
    def test_dangling_references(self, tmp_path: Path, content: str, message: str) -> None:
        project = make_project(tmp_path)
        (project / ".workforest.yaml").write_text(content)
        with pytest.raises(ConfigError, match=message):
            load_config(project)

    def test_target_removed_by_higher_layer(self, tmp_path: Path) -> None:
        write_user_config("openers:\n  edit: $EDITOR\n  win: {from: edit}\n")
        project = make_project(tmp_path)
        (project / ".workforest.yaml").write_text("openers:\n  edit: null\n")
        with pytest.raises(ConfigError, match="'from: edit' names no opener"):
            load_config(project)


class TestValidation:
    def test_unknown_key(self, tmp_path: Path) -> None:
        project = make_project(tmp_path)
        (project / ".workforest.yaml").write_text("worktree_dir: typo\n")
        with pytest.raises(ConfigError, match="unknown key 'worktree_dir'"):
            load_config(project)

    def test_wrong_scalar_type(self, tmp_path: Path) -> None:
        project = make_project(tmp_path)
        (project / ".workforest.yaml").write_text("opener: [not, a, string]\n")
        with pytest.raises(ConfigError, match="'opener' must be a string"):
            load_config(project)

    def test_wrong_list_type(self, tmp_path: Path) -> None:
        project = make_project(tmp_path)
        (project / ".workforest.yaml").write_text("symlinks: {a: b}\n")
        with pytest.raises(ConfigError, match="'symlinks' must be a list of strings"):
            load_config(project)

    def test_wrong_map_type(self, tmp_path: Path) -> None:
        project = make_project(tmp_path)
        (project / ".workforest.yaml").write_text("scripts: [a, b]\n")
        with pytest.raises(ConfigError, match="'scripts' must be a mapping"):
            load_config(project)
        (project / ".workforest.yaml").write_text("scripts:\n  test: [a, b]\n")
        with pytest.raises(ConfigError, match="'scripts' must be a mapping of string to string"):
            load_config(project)

    def test_non_mapping_top_level(self, tmp_path: Path) -> None:
        project = make_project(tmp_path)
        (project / ".workforest.yaml").write_text("- just\n- a list\n")
        with pytest.raises(ConfigError, match="top level must be a mapping"):
            load_config(project)

    def test_invalid_yaml(self, tmp_path: Path) -> None:
        project = make_project(tmp_path)
        (project / ".workforest.yaml").write_text("opener: [unclosed\n")
        with pytest.raises(ConfigError, match="invalid YAML"):
            load_config(project)

    def test_invalid_json(self, tmp_path: Path) -> None:
        project = make_project(tmp_path)
        (project / ".workforest.json").write_text("{nope")
        with pytest.raises(ConfigError, match="invalid JSON"):
            load_config(project)

    def test_empty_file_is_fine(self, tmp_path: Path) -> None:
        project = make_project(tmp_path)
        (project / ".workforest.yaml").write_text("")
        assert load_config(project).opener == ""

    def test_error_names_the_file(self, tmp_path: Path) -> None:
        project = make_project(tmp_path)
        bad = project / ".workforest.yaml"
        bad.write_text("nonsense_key: 1\n")
        with pytest.raises(ConfigError, match=str(bad)):
            load_config(project)


class TestWorktreesDirResolution:
    def test_default_template(self, tmp_path: Path) -> None:
        main = tmp_path / "dev" / "api"
        resolved = resolve_worktrees_dir(Config(), main)
        assert resolved == tmp_path / "dev" / "worktrees" / "api"

    def test_env_vars_expand(self, tmp_path: Path) -> None:
        main = tmp_path / "dev" / "api"
        cfg = Config(worktrees_dir="$HOME/forests/$WF_NAME")
        resolved = resolve_worktrees_dir(cfg, main)
        assert resolved == Path(os.environ["HOME"]) / "forests" / "api"

    def test_relative_result_is_anchored_to_main(self, tmp_path: Path) -> None:
        main = tmp_path / "dev" / "api"
        cfg = Config(worktrees_dir="wt")
        assert resolve_worktrees_dir(cfg, main) == main / "wt"

    def test_undefined_variable_is_config_error(self, tmp_path: Path) -> None:
        cfg = Config(worktrees_dir="$WF_NO_SUCH_VAR/x")
        with pytest.raises(ConfigError, match="worktrees_dir"):
            resolve_worktrees_dir(cfg, tmp_path / "api")

    def test_reference_examples_validate(self, tmp_path: Path) -> None:
        """The shipped example configs must always pass our own validation."""
        examples = Path(__file__).parent.parent / "src" / "workforest" / "examples"
        project = make_project(tmp_path)
        (project / ".workforest.yaml").write_text((examples / ".workforest.yaml").read_text())
        user_dir = Path(os.environ["XDG_CONFIG_HOME"]) / "workforest"
        user_dir.mkdir(parents=True)
        (user_dir / "config.yaml").write_text((examples / "config.yaml").read_text())
        cfg = load_config(project)
        assert cfg.symlinks  # project example sets them
        assert resolve_worktrees_dir(cfg, project).name == "api"
