//! config: layering, merge semantics, validation, template resolution.

use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::errors::ErrorKind;
use crate::output::capture;
use crate::testing::Sandbox;

/// A sandbox with a system dir, a user dir and a project of its own.
struct Layers {
    sandbox: Sandbox,
    roots: Roots,
    project: PathBuf,
}

impl Layers {
    fn new() -> Self {
        let sandbox = Sandbox::new();
        let roots = Roots {
            system_dir: sandbox.path().join("etc-workforest"),
            user_dir: sandbox.path().join("home").join(".config").join("workforest"),
        };
        let project = sandbox.path().join("dev").join("api");
        fs::create_dir_all(&project).unwrap();
        Self { sandbox, roots, project }
    }

    fn write(directory: &Path, basename: &str, content: &str) -> PathBuf {
        fs::create_dir_all(directory).unwrap();
        let path = directory.join(basename);
        fs::write(&path, content).unwrap();
        path
    }

    fn system(&self, content: &str) -> PathBuf {
        Self::write(&self.roots.system_dir, "config.yaml", content)
    }

    fn user(&self, content: &str) -> PathBuf {
        Self::write(&self.roots.user_dir, "config.yaml", content)
    }

    fn project(&self, content: &str) -> PathBuf {
        Self::write(&self.project, ".workforest.yaml", content)
    }

    fn project_file(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.project.join(relative);
        Self::write(path.parent().unwrap(), &util::file_name(&path), content)
    }

    fn load(&self) -> Config {
        self.try_load().unwrap()
    }

    fn try_load(&self) -> Result<Config> {
        load_config_in(&self.roots, Some(&self.project))
    }

    fn load_global(&self) -> Config {
        load_config_in(&self.roots, None).unwrap()
    }

    /// The message of the config error loading this project content gives.
    fn project_error(&self, content: &str) -> String {
        self.project(content);
        let error = self.try_load().unwrap_err();
        assert_eq!(error.kind, ErrorKind::Config, "{error}");
        error.message
    }
}

fn opener(command: &str) -> OpenerSpec {
    OpenerSpec::command(command)
}

fn script(command: &str) -> ScriptSpec {
    ScriptSpec::command(command)
}

fn wrapper(command: &str, background: bool) -> CommandSpec {
    CommandSpec { command: command.into(), background }
}

fn names(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

fn entries<T: Clone>(pairs: &[(&str, T)]) -> IndexMap<String, T> {
    pairs.iter().map(|(name, spec)| (name.to_string(), spec.clone())).collect()
}

/// The merged config in config-file form, as JSON text for comparing.
fn data(config: &Config, key: &str) -> String {
    let value = config.as_value();
    let found = value.as_string_map().unwrap().swap_remove(key).unwrap();
    crate::util::json_compact(&found.to_json())
}

// --- defaults ---------------------------------------------------------------

#[test]
fn no_files_yields_defaults() {
    let config = Layers::new().load_global();
    assert_eq!(config, Config::default());
    assert_eq!(config.worktrees_dir, "$WF_MAIN/../worktrees/$WF_NAME");
    assert_eq!(config.opener, "");
    assert!(config.openers.is_empty() && config.wrappers.is_empty() && config.scripts.is_empty());
    assert!(config.symlinks.is_empty() && config.setup_scripts.is_empty());
    assert!(config.sources.is_empty());
    assert_eq!(config.stop_timeout, Number::Float(30.0));
}

#[test]
fn standard_roots_are_etc_and_the_xdg_config_dir() {
    let roots = Roots::standard();
    assert_eq!(roots.system_dir, Path::new("/etc/workforest"));
    assert!(roots.user_dir.ends_with("workforest"));
}

// --- layering ---------------------------------------------------------------

#[test]
fn precedence_system_user_project_local() {
    let layers = Layers::new();
    layers.system("opener: system\nwrappers:\n  win: from-system\n");
    layers.user("opener: user\n");
    layers.project("opener: project\n");
    layers.project_file(".vscode/.workforest.yaml", "opener: local\n");

    let config = layers.load();
    assert_eq!(config.opener, "local");
    // keys not set by higher layers survive from lower ones
    assert_eq!(config.wrappers, entries(&[("win", wrapper("from-system", false))]));
    let order: Vec<&str> = config.sources.iter().map(|source| source.layer).collect();
    assert_eq!(order, ["system", "user", "project", "project-local"]);
}

#[test]
fn project_layers_skipped_without_main() {
    let layers = Layers::new();
    layers.project("opener: project\n");
    assert_eq!(layers.load_global().opener, "");
}

#[test]
fn basename_order_yaml_yml_json() {
    let layers = Layers::new();
    layers.project_file(".workforest.yml", "opener: from-yml\n");
    layers.project_file(".workforest.json", "{\"opener\": \"from-json\"}");
    assert_eq!(layers.load().opener, "from-yml");
    layers.project("opener: from-yaml\n");
    assert_eq!(layers.load().opener, "from-yaml");
}

#[test]
fn vscode_wins_over_idea() {
    let layers = Layers::new();
    layers.project_file(".idea/.workforest.yaml", "opener: idea\n");
    layers.project_file(".vscode/.workforest.yaml", "opener: vscode\n");
    assert_eq!(layers.load().opener, "vscode");
    assert_eq!(layers.load().sources.len(), 1);
}

#[test]
fn idea_used_when_no_vscode() {
    let layers = Layers::new();
    layers.project_file(".idea/.workforest.yaml", "opener: idea\n");
    let config = layers.load();
    assert_eq!(config.opener, "idea");
    assert_eq!(config.sources.last().unwrap().layer, "project-local");
}

#[test]
fn local_overrides_only_its_keys() {
    let layers = Layers::new();
    layers.project("symlinks: [node_modules]\nopener: shared\n");
    layers.project_file(".idea/.workforest.yaml", "opener: mine\n");
    let config = layers.load();
    assert_eq!(config.opener, "mine");
    assert_eq!(config.symlinks, ["node_modules"]);
}

#[test]
fn json_project_config() {
    let layers = Layers::new();
    layers.project_file(
        ".workforest.json",
        "{\"scripts\": {\"test\": \"make test\"}, \"stop_timeout\": 2}",
    );
    let config = layers.load();
    assert_eq!(config.scripts, entries(&[("test", script("make test"))]));
    assert_eq!(config.stop_timeout, Number::Int(2));
}

// --- merge semantics --------------------------------------------------------

#[test]
fn lists_replace() {
    let layers = Layers::new();
    layers.user("symlinks: [a, b]\nsetup_scripts: [x]\n");
    layers.project("symlinks: []\n");
    assert!(layers.load().symlinks.is_empty());
    assert_eq!(layers.load().setup_scripts, ["x"]);
}

#[test]
fn mappings_merge_per_key() {
    let layers = Layers::new();
    layers.user("scripts:\n  sync: git fetch\n  build: make\n");
    layers.project("scripts:\n  build: npm run build\n");
    assert_eq!(
        layers.load().scripts,
        entries(&[("sync", script("git fetch")), ("build", script("npm run build"))])
    );
}

#[test]
fn null_deletes_mapping_entry() {
    let layers = Layers::new();
    layers.user("scripts:\n  sync: git fetch\n");
    layers.project("scripts:\n  sync: null\n");
    assert!(layers.load().scripts.is_empty());
}

#[test]
fn openers_merge_like_scripts() {
    let layers = Layers::new();
    layers.system("openers:\n  edit: $EDITOR \"$WF_TARGET\"\n");
    layers.user("openers:\n  git: lazygit\n");
    assert_eq!(
        layers.load_global().openers,
        entries(&[("edit", opener("$EDITOR \"$WF_TARGET\"")), ("git", opener("lazygit"))])
    );
}

#[test]
fn entry_mappings_replace_whole() {
    // An entry is one value: a higher layer's mapping replaces the lower
    // layer's mapping outright, never merges field by field.
    let layers = Layers::new();
    layers.user(
        "openers:\n  edit: $EDITOR\n  win: {from: edit, wrap: kitty}\nwrappers:\n  kitty: k\n",
    );
    layers.project("openers:\n  win: {command: x}\n");
    assert_eq!(layers.load().openers["win"], opener("x"));
}

#[test]
fn the_make_section_merges_per_key_and_null_resets() {
    let layers = Layers::new();
    layers.user("make:\n  hidden: true\n  hide_scripts: [a]\n  exclusive_scripts: [dev]\n");
    layers.project("make:\n  hidden: null\n  hide_scripts: [b, c]\n");
    assert_eq!(
        layers.load().make,
        MakeSpec {
            hidden: false,
            hide_scripts: names(&["b", "c"]),
            show_scripts: vec![],
            exclusive_scripts: names(&["dev"]),
        }
    );
}

// --- entries ----------------------------------------------------------------

#[test]
fn string_is_shorthand_for_command() {
    let layers = Layers::new();
    layers.user(
        "openers:\n  edit: '$EDITOR \"$WF_TARGET\"'\n\
         wrappers:\n  direnv: 'direnv exec \"$WF_WORKTREE\" $SHELL -c \"$WF_COMMAND\"'\n",
    );
    let config = layers.load_global();
    assert_eq!(config.openers, entries(&[("edit", opener("$EDITOR \"$WF_TARGET\""))]));
    assert_eq!(
        config.wrappers,
        entries(&[(
            "direnv",
            wrapper("direnv exec \"$WF_WORKTREE\" $SHELL -c \"$WF_COMMAND\"", false)
        )])
    );
}

#[test]
fn mapping_form() {
    let layers = Layers::new();
    layers.user(
        "openers:\n\
         \x20 edit: '$EDITOR \"$WF_TARGET\"'\n\
         \x20 code: {command: 'code \"$WF_WORKTREE\"', background: true}\n\
         \x20 win: {from: edit, wrap: kitty}\n\
         \x20 here: {from: code, background: false}\n\
         \x20 plain: {command: x, wrap: null}\n\
         wrappers:\n  kitty: {command: 'kitty $SHELL -c \"$WF_COMMAND\"', background: true}\n",
    );
    let config = layers.load_global();
    assert_eq!(
        config.wrappers,
        entries(&[("kitty", wrapper("kitty $SHELL -c \"$WF_COMMAND\"", true))])
    );
    let from = |target: &str| OpenerSpec { from: Some(target.into()), ..OpenerSpec::default() };
    assert_eq!(
        config.openers,
        entries(&[
            ("edit", opener("$EDITOR \"$WF_TARGET\"")),
            ("code", OpenerSpec { background: Some(true), ..opener("code \"$WF_WORKTREE\"") }),
            ("win", OpenerSpec { wrap: Some("kitty".into()), ..from("edit") }),
            ("here", OpenerSpec { background: Some(false), ..from("code") }),
            ("plain", opener("x")), // wrap: null is "no wrapper"
        ])
    );
}

#[test]
fn as_value_round_trips_to_config_form() {
    let layers = Layers::new();
    layers.user(
        "openers:\n  edit: '$EDITOR'\n  code: {command: code, background: true}\n\
         \x20 win: {from: edit, wrap: kitty}\n  here: {from: code, background: false}\n\
         \x20 git: lazygit\n\
         wrappers:\n  kitty: {command: kitty, background: true}\n  direnv: direnv\n",
    );
    let config = layers.load_global();
    assert_eq!(
        data(&config, "wrappers"),
        r#"{"kitty":{"command":"kitty","background":true},"direnv":"direnv"}"#
    );
    assert_eq!(
        data(&config, "openers"),
        r#"{"edit":"$EDITOR","code":{"command":"code","background":true},"win":{"from":"edit","wrap":"kitty"},"here":{"from":"code","background":false},"git":"lazygit"}"#
    );
    let keys: Vec<String> = config.as_value().as_string_map().unwrap().into_keys().collect();
    assert_eq!(
        keys,
        [
            "worktrees_dir",
            "opener",
            "openers",
            "wrappers",
            "symlinks",
            "setup_scripts",
            "scripts",
            "stop_timeout",
            "make"
        ]
    );
    assert_eq!(
        data(&config, "make"),
        r#"{"hidden":false,"hide_scripts":[],"show_scripts":[],"exclusive_scripts":[]}"#
    );
}

#[test]
fn script_entries() {
    let layers = Layers::new();
    layers.user(
        "scripts:\n  test: make test\n\
         \x20 dev: {command: npm run dev, exclusive: true, cleanup: fuser -k 3000/tcp}\n\
         \x20 seed: {command: make seed, cleanup: make unseed}\n\
         \x20 plain: {command: make, exclusive: false}\n\
         \x20 step: {command: make step, hidden: true}\n",
    );
    let config = layers.load_global();
    assert_eq!(
        config.scripts,
        entries(&[
            ("test", script("make test")),
            (
                "dev",
                ScriptSpec {
                    exclusive: true,
                    cleanup: Some("fuser -k 3000/tcp".into()),
                    ..script("npm run dev")
                }
            ),
            ("seed", ScriptSpec { cleanup: Some("make unseed".into()), ..script("make seed") }),
            ("plain", script("make")),
            ("step", ScriptSpec { hidden: true, ..script("make step") }),
        ])
    );
    // an explicit default collapses to the shorthand
    assert_eq!(
        data(&config, "scripts"),
        r#"{"test":"make test","dev":{"command":"npm run dev","exclusive":true,"cleanup":"fuser -k 3000/tcp"},"seed":{"command":"make seed","cleanup":"make unseed"},"plain":"make","step":{"command":"make step","hidden":true}}"#
    );
}

#[test]
fn script_background_and_stop_timeout() {
    let layers = Layers::new();
    layers.user(
        "stop_timeout: 5\nscripts:\n\
         \x20 api: {command: docker compose up, background: true, stop_timeout: 60}\n\
         \x20 web: {command: npm run dev, stop_timeout: 2.5}\n",
    );
    let config = layers.load_global();
    assert_eq!(config.stop_timeout, Number::Int(5));
    assert_eq!(
        config.scripts,
        entries(&[
            (
                "api",
                ScriptSpec {
                    background: true,
                    stop_timeout: Some(Number::Int(60)),
                    ..script("docker compose up")
                }
            ),
            ("web", ScriptSpec { stop_timeout: Some(Number::Float(2.5)), ..script("npm run dev") }),
        ])
    );
    assert_eq!(data(&config, "stop_timeout"), "5");
    assert_eq!(
        data(&config, "scripts"),
        r#"{"api":{"command":"docker compose up","background":true,"stop_timeout":60},"web":{"command":"npm run dev","stop_timeout":2.5}}"#
    );
}

#[test]
fn script_groups() {
    let layers = Layers::new();
    layers.user(
        "scripts:\n\
         \x20 migrate: npm run db:migrate\n\
         \x20 backend: {command: docker compose up, exclusive: true}\n\
         \x20 frontend: npm run dev\n\
         \x20 dev: {bulk: [backend, frontend], background: true, stop_timeout: 60}\n\
         \x20 fresh: {pipeline: [migrate, dev], cleanup: echo done}\n",
    );
    let config = layers.load_global();
    assert_eq!(
        config.scripts["dev"],
        ScriptSpec {
            bulk: Some(names(&["backend", "frontend"])),
            background: true,
            stop_timeout: Some(Number::Int(60)),
            ..ScriptSpec::default()
        }
    );
    assert_eq!(
        config.scripts["fresh"],
        ScriptSpec {
            pipeline: Some(names(&["migrate", "dev"])),
            cleanup: Some("echo done".into()),
            ..ScriptSpec::default()
        }
    );
    assert_eq!(config.scripts["dev"].members(), ["backend", "frontend"]);
    assert!(config.scripts["migrate"].members().is_empty());
    let scripts = data(&config, "scripts");
    assert!(
        scripts.contains(
            r#""dev":{"bulk":["backend","frontend"],"background":true,"stop_timeout":60}"#
        )
    );
    assert!(scripts.contains(r#""fresh":{"pipeline":["migrate","dev"],"cleanup":"echo done"}"#));
}

#[test]
fn member_removed_by_a_higher_layer_is_caught() {
    let layers = Layers::new();
    layers.user("scripts:\n  a: 'true'\n  dev: {bulk: [a]}\n");
    let message = layers.project_error("scripts:\n  a: null\n");
    assert_eq!(message, "scripts.dev: member 'a' names no script (known: dev)");
}

#[test]
fn script_mapping_overrides_string_per_key() {
    let layers = Layers::new();
    layers.user("scripts:\n  dev: npm run dev\n");
    layers.project("scripts:\n  dev: {command: make dev, exclusive: true}\n");
    assert_eq!(
        layers.load().scripts,
        entries(&[("dev", ScriptSpec { exclusive: true, ..script("make dev") })])
    );
}

#[test]
fn invalid_entries() {
    let cases = [
        (
            "openers:\n  win: [a, b]\n",
            "openers.win: must be a shell command or a mapping (command, from, wrap, background)",
        ),
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
        ("wrappers:\n  k: {from: x}\n", "wrappers.k: 'command' is required"),
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
            "openers.win: 'background' and 'wrap' are mutually exclusive (the wrapper decides where the command runs)",
        ),
        (
            "scripts:\n  test: {command: x, exclusive: sometimes}\n",
            "scripts.test: 'exclusive' must be true or false",
        ),
        (
            "scripts:\n  test: {command: x, hidden: maybe}\n",
            "scripts.test: 'hidden' must be true or false",
        ),
        (
            "scripts:\n  test: {command: x, cleanup: ''}\n",
            "scripts.test: 'cleanup' must be a non-empty string",
        ),
        (
            "scripts:\n  test: {command: x, cleanup: [a]}\n",
            "scripts.test: 'cleanup' must be a non-empty string",
        ),
        (
            "scripts:\n  test: {cleanup: x}\n",
            "scripts.test: exactly one of 'command', 'bulk', and 'pipeline' is required",
        ),
        (
            "scripts:\n  test: {command: x, bulk: [a]}\n",
            "scripts.test: exactly one of 'command', 'bulk', and 'pipeline' is required",
        ),
        (
            "scripts:\n  test: {bulk: [a], pipeline: [b]}\n",
            "scripts.test: exactly one of 'command', 'bulk', and 'pipeline' is required",
        ),
        (
            "scripts:\n  test: {bulk: []}\n",
            "scripts.test: 'bulk' must be a non-empty list of script names",
        ),
        (
            "scripts:\n  test: {pipeline: a}\n",
            "scripts.test: 'pipeline' must be a non-empty list of script names",
        ),
        (
            "scripts:\n  test: {bulk: [a, '']}\n",
            "scripts.test: 'bulk' must be a non-empty list of script names",
        ),
        (
            "scripts:\n  test: {command: x, background: 1}\n",
            "scripts.test: 'background' must be true or false",
        ),
        (
            "scripts:\n  test: {command: x, stop_timeout: 0}\n",
            "scripts.test: 'stop_timeout' must be a number of seconds above 0",
        ),
        (
            "scripts:\n  test: {command: x, stop_timeout: soon}\n",
            "scripts.test: 'stop_timeout' must be a number of seconds above 0",
        ),
        (
            "scripts:\n  test: {command: x, stop_timeout: true}\n",
            "scripts.test: 'stop_timeout' must be a number of seconds above 0",
        ),
        ("scripts:\n  test: ''\n", "scripts.test: must not be empty"),
        (
            "scripts:\n  test: 12\n",
            "scripts.test: must be a shell command or a mapping (command, bulk, pipeline, background, exclusive, hidden, cleanup, stop_timeout)",
        ),
        ("make: [a]\n", "'make' must be a mapping"),
        ("make:\n  hidden: sometimes\n", "make.hidden must be true or false"),
        ("make:\n  show_scripts: build\n", "make.show_scripts must be a list of strings"),
    ];
    for (content, message) in cases {
        let layers = Layers::new();
        let file = layers.project.join(".workforest.yaml");
        assert_eq!(
            layers.project_error(content),
            format!("{}: {message}", file.display()),
            "{content}"
        );
    }
}

#[test]
fn invalid_top_level_numbers() {
    for content in ["stop_timeout: -1\n", "stop_timeout: '30'\n", "stop_timeout: .nan\n"] {
        let layers = Layers::new();
        let message = layers.project_error(content);
        assert!(
            message.ends_with(": 'stop_timeout' must be a number of seconds above 0"),
            "{message}"
        );
    }
}

#[test]
fn group_references_are_checked_on_the_merged_config() {
    let cases = [
        (
            "scripts:\n  a: 'true'\n  dev: {bulk: [a, nope]}\n",
            "scripts.dev: member 'nope' names no script (known: a, dev)",
        ),
        (
            "scripts:\n  a: 'true'\n  dev: {pipeline: [a, a]}\n",
            "scripts.dev: member 'a' is listed more than once",
        ),
        ("scripts:\n  dev: {bulk: [dev]}\n", "scripts.dev: groups form a cycle: dev -> dev"),
        (
            "scripts:\n  a: {pipeline: [b]}\n  b: {bulk: [c]}\n  c: {pipeline: [a]}\n",
            "scripts.c: groups form a cycle: a -> b -> c -> a",
        ),
    ];
    for (content, message) in cases {
        assert_eq!(Layers::new().project_error(content), message, "{content}");
    }
}

// --- references -------------------------------------------------------------

#[test]
fn references_resolve_across_layers() {
    let layers = Layers::new();
    layers.system("openers:\n  edit: $EDITOR\n");
    layers.user("openers:\n  win: {from: edit, wrap: kitty}\nwrappers:\n  kitty: k\n");
    assert_eq!(
        layers.load_global().openers["win"],
        OpenerSpec {
            from: Some("edit".into()),
            wrap: Some("kitty".into()),
            ..OpenerSpec::default()
        }
    );
}

#[test]
fn dangling_references() {
    let cases = [
        (
            "openers:\n  win: {from: edit}\n",
            "openers.win: 'from: edit' names no opener (known: win)",
        ),
        (
            "openers:\n  a: {from: b}\n  b: {from: c}\n  c: x\n",
            "openers.a: 'from: b' must name an opener with a command, but b has 'from: c'",
        ),
        (
            "openers:\n  a: {from: a}\n",
            "openers.a: 'from: a' must name an opener with a command, but a has 'from: a'",
        ),
        (
            "openers:\n  a: {from: b}\n  b: {from: a}\n",
            "openers.a: 'from: b' must name an opener with a command, but b has 'from: a'",
        ),
        (
            "openers:\n  win: {command: x, wrap: kity}\nwrappers:\n  kitty: k\n",
            "openers.win: unknown wrapper 'kity' (available: kitty)",
        ),
        (
            "openers:\n  win: {command: x, wrap: kitty}\n",
            "openers.win: unknown wrapper 'kitty' (available: none defined)",
        ),
    ];
    for (content, message) in cases {
        assert_eq!(Layers::new().project_error(content), message, "{content}");
    }
}

#[test]
fn target_removed_by_higher_layer() {
    let layers = Layers::new();
    layers.user("openers:\n  edit: $EDITOR\n  win: {from: edit}\n");
    assert_eq!(
        layers.project_error("openers:\n  edit: null\n"),
        "openers.win: 'from: edit' names no opener (known: win)"
    );
}

// --- validation -------------------------------------------------------------

#[test]
fn unknown_key_is_a_warning() {
    let layers = Layers::new();
    let path = layers.project("worktree_dir: typo\nopener: vim\n");
    let (config, warnings) = capture(|| layers.load());
    assert_eq!(config.opener, "vim"); // the rest of the file loads
    assert_eq!(config.worktrees_dir, Config::default().worktrees_dir);
    assert_eq!(
        warnings,
        format!(
            "{}: unknown key 'worktree_dir', ignored (known keys: make, opener, openers, \
             scripts, setup_scripts, stop_timeout, symlinks, worktrees_dir, wrappers)\n",
            path.display()
        )
    );
}

#[test]
fn unknown_nested_key_is_a_warning() {
    let cases = [
        (
            "openers:\n  win: {command: x, mode: y}\n",
            "openers.win: unknown key 'mode', ignored (known keys: command, from, wrap, background)",
        ),
        (
            "wrappers:\n  k: {command: x, wrap: y}\n",
            "wrappers.k: unknown key 'wrap', ignored (known keys: command, background)",
        ),
        (
            "scripts:\n  test: {command: x, wrap: y}\n",
            "scripts.test: unknown key 'wrap', ignored (known keys: command, bulk, \
             pipeline, background, exclusive, hidden, cleanup, stop_timeout)",
        ),
        (
            "make:\n  nope: true\n",
            "make: unknown key 'nope', ignored (known keys: hidden, hide_scripts, \
             show_scripts, exclusive_scripts)",
        ),
        (
            "1: x\n",
            "unknown key 1, ignored (known keys: make, opener, openers, scripts, setup_scripts, stop_timeout, symlinks, worktrees_dir, wrappers)",
        ),
    ];
    for (content, warning) in cases {
        let layers = Layers::new();
        let path = layers.project(content);
        let (_, warnings) = capture(|| layers.load());
        assert_eq!(warnings, format!("{}: {warning}\n", path.display()), "{content}");
    }
}

#[test]
fn unknown_keys_are_dropped() {
    let layers = Layers::new();
    layers.project(
        "openers:\n  win: {command: x, mode: y}\n\
         wrappers:\n  k: {command: x, wrap: y}\n\
         scripts:\n  test: {command: x, wrap: y, exclusive: true}\n\
         make:\n  nope: true\n  hidden: true\n",
    );
    let (config, _) = capture(|| layers.load());
    assert_eq!(config.openers, entries(&[("win", opener("x"))]));
    assert_eq!(config.wrappers, entries(&[("k", wrapper("x", false))]));
    assert_eq!(config.scripts, entries(&[("test", ScriptSpec { exclusive: true, ..script("x") })]));
    assert_eq!(config.make, MakeSpec { hidden: true, ..MakeSpec::default() });
}

#[test]
fn each_file_warns_for_itself() {
    let layers = Layers::new();
    let user = layers.user("bogus: 1\n");
    let project = layers.project("bogus: 1\n");
    let ((), warnings) = capture(|| {
        layers.load();
        layers.load(); // a second load in the same process repeats nothing
    });
    let files: Vec<&str> = warnings.lines().map(|line| line.split(": ").next().unwrap()).collect();
    assert_eq!(files, [user.to_str().unwrap(), project.to_str().unwrap()]);
}

#[test]
fn an_unknown_key_does_not_excuse_an_invalid_one() {
    let layers = Layers::new();
    let (message, _) = capture(|| layers.project_error("bogus: 1\nopener: [not, a, string]\n"));
    assert!(message.ends_with(": 'opener' must be a string, got list"), "{message}");
}

#[test]
fn wrong_types_name_what_they_got() {
    let cases = [
        ("opener: [not, a, string]\n", "'opener' must be a string, got list"),
        ("opener: 1\n", "'opener' must be a string, got int"),
        ("opener:\n", "'opener' must be a string, got NoneType"),
        ("worktrees_dir: {a: b}\n", "'worktrees_dir' must be a string, got dict"),
        ("opener: 2024-01-31\n", "'opener' must be a string, got date"),
        ("symlinks: {a: b}\n", "'symlinks' must be a list of strings"),
        ("symlinks: [1]\n", "'symlinks' must be a list of strings"),
        ("scripts: [a, b]\n", "'scripts' must be a mapping"),
        ("scripts:\n  1: x\n", "'scripts' must be a mapping"),
        ("- just\n- a list\n", "top level must be a mapping, got list"),
        ("just text\n", "top level must be a mapping, got str"),
    ];
    for (content, message) in cases {
        let layers = Layers::new();
        let file = layers.project.join(".workforest.yaml");
        assert_eq!(
            layers.project_error(content),
            format!("{}: {message}", file.display()),
            "{content}"
        );
    }
}

#[test]
fn invalid_yaml_and_json() {
    let layers = Layers::new();
    let file = layers.project.join(".workforest.yaml");
    let message = layers.project_error("opener: [unclosed\n");
    assert!(message.starts_with(&format!("{}: invalid YAML: ", file.display())), "{message}");

    let layers = Layers::new();
    let file = layers.project_file(".workforest.json", "{nope");
    let message = layers.try_load().unwrap_err().message;
    assert!(message.starts_with(&format!("{}: invalid JSON: ", file.display())), "{message}");
}

#[test]
fn empty_files_are_fine() {
    let layers = Layers::new();
    layers.project("");
    layers.user("# nothing set\n");
    let config = layers.load();
    assert_eq!(config.opener, "");
    assert_eq!(config.sources.len(), 2);
}

#[test]
fn an_unreadable_file_is_a_config_error() {
    let layers = Layers::new();
    let path = layers.project("opener: vim\n");
    fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
    let error = layers.try_load().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(error.message.starts_with(&path.display().to_string()), "{error}");
}

// --- worktrees_dir ----------------------------------------------------------

fn with_dir(template: &str) -> Config {
    Config { worktrees_dir: template.into(), ..Config::default() }
}

fn no_environment(_: &str) -> Option<String> {
    None
}

#[test]
fn default_template() {
    let main = Path::new("/tmp/dev/api");
    let resolved = resolve_worktrees_dir_with(&Config::default(), main, &no_environment).unwrap();
    assert_eq!(resolved, Path::new("/tmp/dev/worktrees/api"));
    assert_eq!(resolve_worktrees_dir(&Config::default(), main).unwrap(), resolved);
}

#[test]
fn env_vars_expand_and_wf_names_win() {
    let main = Path::new("/tmp/dev/api");
    let environment = |name: &str| match name {
        "HOME" => Some("/home/me".to_string()),
        "WF_NAME" => Some("from-the-environment".to_string()),
        _ => None,
    };
    let resolve =
        |template: &str| resolve_worktrees_dir_with(&with_dir(template), main, &environment);
    assert_eq!(resolve("$HOME/forests/$WF_NAME").unwrap(), Path::new("/home/me/forests/api"));
    assert_eq!(resolve("${HOME}/f/${WF_NAME}s").unwrap(), Path::new("/home/me/f/apis"));
    assert_eq!(resolve("/x/$$HOME").unwrap(), Path::new("/x/$HOME"));
}

#[test]
fn relative_result_is_anchored_to_main() {
    let main = Path::new("/tmp/dev/api");
    let resolve =
        |template: &str| resolve_worktrees_dir_with(&with_dir(template), main, &no_environment);
    assert_eq!(resolve("wt").unwrap(), Path::new("/tmp/dev/api/wt"));
    assert_eq!(resolve("../wt/./x").unwrap(), Path::new("/tmp/dev/wt/x"));
    assert_eq!(resolve("~/wt").unwrap(), util::home_dir().join("wt"));
}

#[test]
fn undefined_variables_and_bad_placeholders_are_config_errors() {
    let main = Path::new("/tmp/api");
    let message = |template: &str| {
        let error =
            resolve_worktrees_dir_with(&with_dir(template), main, &no_environment).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Config);
        error.message
    };
    assert_eq!(message("$WF_NO_SUCH_VAR/x"), "worktrees_dir '$WF_NO_SUCH_VAR/x': 'WF_NO_SUCH_VAR'");
    assert_eq!(message("$ x"), "worktrees_dir '$ x': Invalid placeholder in string: line 1, col 1");
    assert_eq!(message("a$ "), "worktrees_dir 'a$ ': Invalid placeholder in string: line 1, col 2");
    assert_eq!(
        message("ab${1}"),
        "worktrees_dir 'ab${1}': Invalid placeholder in string: line 1, col 3"
    );
    assert_eq!(
        message("a\n$"),
        "worktrees_dir 'a\\n$': Invalid placeholder in string: line 2, col 1"
    );
    assert_eq!(
        message("${WF_MAIN"),
        "worktrees_dir '${WF_MAIN': Invalid placeholder in string: line 1, col 1"
    );
}

#[test]
fn reference_examples_validate() {
    // The shipped example configs must always pass our own validation.
    let layers = Layers::new();
    layers.project(include_str!("../workforest/examples/.workforest.yaml"));
    layers.user(include_str!("../workforest/examples/config.yaml"));
    let (config, warnings) = capture(|| layers.load());
    assert_eq!(warnings, "");
    assert!(!config.symlinks.is_empty()); // the project example sets them
    let resolved = resolve_worktrees_dir(&config, &layers.project).unwrap();
    assert_eq!(util::file_name(&resolved), "api");
    assert_eq!(resolved, layers.sandbox.path().join("dev").join("worktrees").join("api"));
}

#[test]
fn the_init_template_sets_nothing() {
    let layers = Layers::new();
    layers.project(include_str!("../workforest/templates/project.yaml"));
    let config = layers.load();
    assert_eq!(Config { sources: vec![], ..config }, Config::default());
}
