//! Configuration: schema, layered loading, merging, template resolution.
//!
//! Layers (low → high): built-in defaults → system → user →
//! project-shared (main worktree root) → project-local (.vscode/ then
//! .idea/) → CLI flags (applied by commands, not here). Nothing in the
//! environment changes the result.
//!
//! Reading lives here and in `yaml.rs`; `dump.rs` writes the merged result
//! for `wf config`; `edit.rs` changes one file in place, comments and
//! formatting kept.

pub mod dump;
pub mod edit;
pub mod value;
pub mod yaml;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use crate::errors::{Error, Result};
use crate::output;
use crate::util::{self, repr};
pub use value::{Number, Value};

pub const SYSTEM_CONFIG_DIR: &str = "/etc/workforest";
pub const GLOBAL_BASENAMES: [&str; 3] = ["config.yaml", "config.yml", "config.json"];
pub const PROJECT_BASENAMES: [&str; 3] =
    [".workforest.yaml", ".workforest.yml", ".workforest.json"];
pub const PROJECT_LOCAL_DIRS: [&str; 2] = [".vscode", ".idea"];
pub const DEFAULT_WORKTREES_DIR: &str = "$WF_MAIN/../worktrees/$WF_NAME";
pub const DEFAULT_STOP_TIMEOUT: f64 = 30.0;

/// A `wrappers` entry, and what an opener resolves to: a shell command and
/// where it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub command: String,
    /// Spawn detached instead of running in the user's terminal.
    pub background: bool,
}

/// An `openers` entry: a shell command of its own, or another opener's
/// (`from`), optionally through a wrapper. Exactly one of `command`/`from`
/// is set; `from` targets carry a `command` themselves (one level).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OpenerSpec {
    /// Always a shell command, never a name.
    pub command: Option<String>,
    /// An `openers` name.
    pub from: Option<String>,
    /// A `wrappers` name; the wrapper then decides where it runs.
    pub wrap: Option<String>,
    /// None: the `from` target's setting (own command: false).
    pub background: Option<bool>,
}

impl OpenerSpec {
    pub fn command(command: impl Into<String>) -> Self {
        Self { command: Some(command.into()), ..Self::default() }
    }
}

/// A `scripts` entry: what `wf run NAME` runs — a shell command, or a
/// group of other entries by name, `bulk` (in parallel) or `pipeline` (in
/// order); exactly one of the three — where, whether only one instance may
/// run per project, whether it is offered by name, and what to run once it
/// has ended.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScriptSpec {
    pub command: Option<String>,
    /// Members run at once; done when all are.
    pub bulk: Option<Vec<String>>,
    /// Members run in order; stops at the first failure.
    pub pipeline: Option<Vec<String>>,
    /// Detach, with output to a log file, instead of holding the terminal.
    pub background: bool,
    /// Starting it stops every running instance in the project.
    pub exclusive: bool,
    /// Left out of completions and the editor lists; still runs by name.
    pub hidden: bool,
    /// Runs after the command ends, however it ended.
    pub cleanup: Option<String>,
    /// Seconds between SIGTERM and SIGKILL; None: the global one.
    pub stop_timeout: Option<Number>,
}

impl ScriptSpec {
    pub fn command(command: impl Into<String>) -> Self {
        Self { command: Some(command.into()), ..Self::default() }
    }

    /// The names a group runs; empty for a command.
    pub fn members(&self) -> &[String] {
        self.bulk.as_deref().or(self.pipeline.as_deref()).unwrap_or_default()
    }
}

/// The `make` section: which of the makefile's targets `wf make` offers by
/// name, and which of them may run only once at a time. Hiding a target
/// keeps it out of completions and the editor lists; `wf make` runs it all
/// the same.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MakeSpec {
    /// Offer no target at all.
    pub hidden: bool,
    /// Offer every target but these.
    pub hide_scripts: Vec<String>,
    /// Offer only these (wins over hide_scripts).
    pub show_scripts: Vec<String>,
    /// Starting one stops its running instances.
    pub exclusive_scripts: Vec<String>,
}

const MAKE_KEYS: [&str; 4] = ["hidden", "hide_scripts", "show_scripts", "exclusive_scripts"];

/// Which `map` an entry belongs to, and so which keys it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Opener,
    Wrapper,
    Script,
}

impl EntryKind {
    fn keys(self) -> &'static [&'static str] {
        match self {
            EntryKind::Opener => &["command", "from", "wrap", "background"],
            EntryKind::Wrapper => &["command", "background"],
            EntryKind::Script => &[
                "command",
                "bulk",
                "pipeline",
                "background",
                "exclusive",
                "hidden",
                "cleanup",
                "stop_timeout",
            ],
        }
    }
}

/// Kinds: `Map` (name → entry) and `Section` (a fixed set of keys) are
/// both mappings, so a null value deletes the inherited key during merge.
/// A map entry is a mapping or a string, shorthand for `{command: <string>}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Str,
    Number,
    List,
    Map(EntryKind),
    Section,
}

const SCHEMA: [(&str, Kind); 9] = [
    ("worktrees_dir", Kind::Str),
    ("opener", Kind::Str),
    ("openers", Kind::Map(EntryKind::Opener)),
    ("wrappers", Kind::Map(EntryKind::Wrapper)),
    ("symlinks", Kind::List),
    ("setup_scripts", Kind::List),
    ("scripts", Kind::Map(EntryKind::Script)),
    ("stop_timeout", Kind::Number),
    ("make", Kind::Section),
];

fn kind_of(key: &str) -> Option<Kind> {
    SCHEMA.iter().find(|(name, _)| *name == key).map(|(_, kind)| *kind)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSource {
    /// "system", "user", "project", or "project-local".
    pub layer: &'static str,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub worktrees_dir: String,
    pub opener: String,
    pub openers: IndexMap<String, OpenerSpec>,
    pub wrappers: IndexMap<String, CommandSpec>,
    pub symlinks: Vec<String>,
    pub setup_scripts: Vec<String>,
    pub scripts: IndexMap<String, ScriptSpec>,
    /// Seconds a stopped script gets between SIGTERM and SIGKILL.
    pub stop_timeout: Number,
    pub make: MakeSpec,
    pub sources: Vec<ConfigSource>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            worktrees_dir: DEFAULT_WORKTREES_DIR.into(),
            opener: String::new(),
            openers: IndexMap::new(),
            wrappers: IndexMap::new(),
            symlinks: Vec::new(),
            setup_scripts: Vec::new(),
            scripts: IndexMap::new(),
            stop_timeout: Number::Float(DEFAULT_STOP_TIMEOUT),
            make: MakeSpec::default(),
            sources: Vec::new(),
        }
    }
}

fn s(text: &str) -> Value {
    Value::Str(text.to_string())
}

fn strings(items: &[String]) -> Value {
    Value::List(items.iter().map(|item| s(item)).collect())
}

fn named<T>(entries: &IndexMap<String, T>, data: impl Fn(&T) -> Value) -> Value {
    Value::Map(entries.iter().map(|(name, spec)| (s(name), data(spec))).collect())
}

/// The bare command when that is all there is, else the mapping.
fn shorthand(pairs: Vec<(Value, Value)>) -> Value {
    match pairs.as_slice() {
        [(Value::Str(key), command)] if key == "command" => command.clone(),
        _ => Value::Map(pairs),
    }
}

impl CommandSpec {
    /// The config-file form of the entry.
    fn data(&self) -> Value {
        let mut pairs = vec![(s("command"), s(&self.command))];
        if self.background {
            pairs.push((s("background"), Value::Bool(true)));
        }
        shorthand(pairs)
    }
}

impl OpenerSpec {
    /// The config-file form: unset keys left out. An explicit
    /// `background: false` is an override of the `from` target and is kept.
    fn data(&self) -> Value {
        let mut pairs = Vec::new();
        for (key, value) in [("command", &self.command), ("from", &self.from), ("wrap", &self.wrap)]
        {
            if let Some(value) = value {
                pairs.push((s(key), s(value)));
            }
        }
        if let Some(background) = self.background {
            pairs.push((s("background"), Value::Bool(background)));
        }
        shorthand(pairs)
    }
}

impl ScriptSpec {
    /// The config-file form: unset keys and default flags left out.
    fn data(&self) -> Value {
        let mut pairs = Vec::new();
        if let Some(command) = &self.command {
            pairs.push((s("command"), s(command)));
        }
        if let Some(bulk) = &self.bulk {
            pairs.push((s("bulk"), strings(bulk)));
        }
        if let Some(pipeline) = &self.pipeline {
            pairs.push((s("pipeline"), strings(pipeline)));
        }
        for (key, flag) in [
            ("background", self.background),
            ("exclusive", self.exclusive),
            ("hidden", self.hidden),
        ] {
            if flag {
                pairs.push((s(key), Value::Bool(true)));
            }
        }
        if let Some(cleanup) = &self.cleanup {
            pairs.push((s("cleanup"), s(cleanup)));
        }
        if let Some(stop_timeout) = self.stop_timeout {
            pairs.push((s("stop_timeout"), Value::Number(stop_timeout)));
        }
        shorthand(pairs)
    }
}

impl Config {
    /// The merged configuration in config-file form, keys in schema order.
    pub fn as_value(&self) -> Value {
        Value::Map(vec![
            (s("worktrees_dir"), s(&self.worktrees_dir)),
            (s("opener"), s(&self.opener)),
            (s("openers"), named(&self.openers, OpenerSpec::data)),
            (s("wrappers"), named(&self.wrappers, CommandSpec::data)),
            (s("symlinks"), strings(&self.symlinks)),
            (s("setup_scripts"), strings(&self.setup_scripts)),
            (s("scripts"), named(&self.scripts, ScriptSpec::data)),
            (s("stop_timeout"), Value::Number(self.stop_timeout)),
            (
                s("make"),
                Value::Map(vec![
                    (s("hidden"), Value::Bool(self.make.hidden)),
                    (s("hide_scripts"), strings(&self.make.hide_scripts)),
                    (s("show_scripts"), strings(&self.make.show_scripts)),
                    (s("exclusive_scripts"), strings(&self.make.exclusive_scripts)),
                ]),
            ),
        ])
    }
}

fn read_text(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|error| {
        Error::config(format!("{}: {}", path.display(), util::os_error_text(&error)))
    })
}

fn parse_file(path: &Path) -> Result<Vec<(Value, Value)>> {
    let text = read_text(path)?;
    let data = if path.extension().is_some_and(|extension| extension == "json") {
        serde_json::from_str::<serde_json::Value>(&text)
            .map(Value::from_json)
            .map_err(|error| Error::config(format!("{}: invalid JSON: {error}", path.display())))?
    } else {
        yaml::load(&text)
            .map_err(|error| Error::config(format!("{}: invalid YAML: {error}", path.display())))?
    };
    match data {
        Value::Null => Ok(Vec::new()),
        Value::Map(pairs) => Ok(pairs),
        other => Err(Error::config(format!(
            "{}: top level must be a mapping, got {}",
            path.display(),
            other.type_name()
        ))),
    }
}

/// An unknown key is a warning, never an error: a file written for a newer
/// workforest — or one with a typo in it — must not take every command
/// down with it. The key is dropped and the rest loads; `location` is the
/// file, plus the entry (`make`, `openers.NAME`) below the top level.
fn warn_unknown_key(location: &str, key: &Value, known: &[&str]) {
    output::warn_once(&format!(
        "{location}: unknown key {}, ignored (known keys: {})",
        key.repr(),
        known.join(", ")
    ));
}

fn is_positive_number(value: &Value) -> bool {
    matches!(value, Value::Number(number) if number.as_f64() > 0.0)
}

fn is_name_list(value: &Value) -> bool {
    value
        .as_string_list()
        .is_some_and(|names| !names.is_empty() && names.iter().all(|name| !name.trim().is_empty()))
}

fn get<'a>(entry: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    entry.iter().find(|(name, _)| name == key).map(|(_, value)| value)
}

/// Check one file's keys and values; returns it without the unknown keys,
/// which are warned about and ignored at every level.
fn validate(data: Vec<(Value, Value)>, path: &Path) -> Result<Vec<(String, Value)>> {
    let file = path.display().to_string();
    let mut known: Vec<&str> = SCHEMA.iter().map(|(name, _)| *name).collect();
    known.sort_unstable();
    let mut checked = Vec::new();
    for (key, value) in data {
        let Some((key, kind)) =
            key.as_str().and_then(|name| Some((name.to_string(), kind_of(name)?)))
        else {
            warn_unknown_key(&file, &key, &known);
            continue;
        };
        let value = match kind {
            Kind::Str => {
                if value.as_str().is_none() {
                    return Err(Error::config(format!(
                        "{file}: {} must be a string, got {}",
                        repr(&key),
                        value.type_name()
                    )));
                }
                value
            }
            Kind::Number => {
                if !is_positive_number(&value) {
                    return Err(Error::config(format!(
                        "{file}: {} must be a number of seconds above 0",
                        repr(&key)
                    )));
                }
                value
            }
            Kind::List => {
                if value.as_string_list().is_none() {
                    return Err(Error::config(format!(
                        "{file}: {} must be a list of strings",
                        repr(&key)
                    )));
                }
                value
            }
            Kind::Map(entry_kind) => {
                let entries = value.as_string_map().ok_or_else(|| {
                    Error::config(format!("{file}: {} must be a mapping", repr(&key)))
                })?;
                let mut out = Vec::new();
                for (name, entry) in entries {
                    let location = format!("{file}: {key}.{name}");
                    out.push((s(&name), validate_entry(entry, entry_kind, &location)?));
                }
                Value::Map(out)
            }
            Kind::Section => validate_section(value, &key, &file)?,
        };
        checked.push((key, value));
    }
    Ok(checked)
}

/// The `make` section — the only one: a fixed set of keys, each shaped
/// like its MakeSpec default, a flag or a list of names; a null value
/// resets the key to that default during merge. Returns the section
/// without its unknown keys.
fn validate_section(value: Value, key: &str, file: &str) -> Result<Value> {
    let entries = value
        .as_string_map()
        .ok_or_else(|| Error::config(format!("{file}: {} must be a mapping", repr(key))))?;
    let mut checked = Vec::new();
    for (name, entry) in entries {
        if !MAKE_KEYS.contains(&name.as_str()) {
            warn_unknown_key(&format!("{file}: {key}"), &s(&name), &MAKE_KEYS);
            continue;
        }
        if entry != Value::Null {
            if name == "hidden" {
                if !matches!(entry, Value::Bool(_)) {
                    return Err(Error::config(format!(
                        "{file}: {key}.{name} must be true or false"
                    )));
                }
            } else if entry.as_string_list().is_none() {
                return Err(Error::config(format!(
                    "{file}: {key}.{name} must be a list of strings"
                )));
            }
        }
        checked.push((s(&name), entry));
    }
    Ok(Value::Map(checked))
}

/// Check one `map` entry; returns it without its unknown keys.
fn validate_entry(entry: Value, kind: EntryKind, location: &str) -> Result<Value> {
    let fail = |message: &str| Err(Error::config(format!("{location}: {message}")));
    let known = kind.keys();
    let pairs = match entry {
        Value::Null => return Ok(Value::Null),
        Value::Str(text) => {
            if text.trim().is_empty() {
                return fail("must not be empty");
            }
            return Ok(Value::Str(text));
        }
        Value::Map(pairs) => pairs,
        _ => {
            return fail(&format!("must be a shell command or a mapping ({})", known.join(", ")));
        }
    };
    let mut entry: Vec<(String, Value)> = Vec::new();
    for (key, value) in pairs {
        match key.as_str().filter(|name| known.contains(name)) {
            Some(name) => entry.push((name.to_string(), value)),
            None => warn_unknown_key(location, &key, known),
        }
    }
    for field in ["command", "from"] {
        if get(&entry, field).is_some_and(|value| value.as_str().is_none()) {
            return fail(&format!("{} must be a string", repr(field)));
        }
    }
    if get(&entry, "command").and_then(Value::as_str).is_some_and(|text| text.trim().is_empty()) {
        return fail("'command' must not be empty");
    }
    for field in ["bulk", "pipeline"] {
        if get(&entry, field).is_some_and(|value| !is_name_list(value)) {
            return fail(&format!("{} must be a non-empty list of script names", repr(field)));
        }
    }
    if get(&entry, "wrap").is_some_and(|value| !matches!(value, Value::Str(_) | Value::Null)) {
        return fail("'wrap' must be a string");
    }
    for field in ["background", "exclusive", "hidden"] {
        if get(&entry, field).is_some_and(|value| !matches!(value, Value::Bool(_))) {
            return fail(&format!("{} must be true or false", repr(field)));
        }
    }
    if get(&entry, "cleanup")
        .is_some_and(|value| value.as_str().is_none_or(|text| text.trim().is_empty()))
    {
        return fail("'cleanup' must be a non-empty string");
    }
    if get(&entry, "stop_timeout").is_some_and(|value| !is_positive_number(value)) {
        return fail("'stop_timeout' must be a number of seconds above 0");
    }
    let has = |field: &str| get(&entry, field).is_some();
    match kind {
        EntryKind::Opener => {
            if has("command") == has("from") {
                return fail("exactly one of 'command' and 'from' is required");
            }
        }
        EntryKind::Script => {
            if ["command", "bulk", "pipeline"].into_iter().filter(|field| has(field)).count() != 1 {
                return fail("exactly one of 'command', 'bulk', and 'pipeline' is required");
            }
        }
        EntryKind::Wrapper => {
            if !has("command") {
                return fail("'command' is required");
            }
        }
    }
    if has("wrap") && has("background") {
        return fail(
            "'background' and 'wrap' are mutually exclusive (the wrapper decides where the command runs)",
        );
    }
    Ok(Value::Map(entry.into_iter().map(|(key, value)| (s(&key), value)).collect()))
}

/// Cross-entry checks on the merged result: a `from` names an opener that
/// has a command of its own (one level — no chains, so no cycles, and a
/// target removed by a higher layer is caught here), a `wrap` names a
/// wrapper, and a group's members name scripts, once each, without forming
/// a cycle (a member removed by a higher layer is caught here).
fn validate_references(config: &Config) -> Result<()> {
    let sorted = |names: Vec<&String>| {
        let mut names: Vec<&str> = names.into_iter().map(String::as_str).collect();
        names.sort_unstable();
        if names.is_empty() { "none defined".to_string() } else { names.join(", ") }
    };
    for (name, spec) in &config.openers {
        let location = format!("openers.{name}");
        if let Some(from) = &spec.from {
            let Some(target) = config.openers.get(from) else {
                return Err(Error::config(format!(
                    "{location}: 'from: {from}' names no opener (known: {})",
                    sorted(config.openers.keys().collect())
                )));
            };
            if target.command.is_none() {
                return Err(Error::config(format!(
                    "{location}: 'from: {from}' must name an opener with a command, \
                     but {from} has 'from: {}'",
                    target.from.as_deref().unwrap_or_default()
                )));
            }
        }
        if let Some(wrap) = spec.wrap.as_deref().filter(|wrap| !wrap.is_empty())
            && !config.wrappers.contains_key(wrap)
        {
            return Err(Error::config(format!(
                "{location}: unknown wrapper {} (available: {})",
                repr(wrap),
                sorted(config.wrappers.keys().collect())
            )));
        }
    }
    for name in config.scripts.keys() {
        validate_members(&config.scripts, name, &[name.as_str()])?;
    }
    Ok(())
}

fn validate_members(
    scripts: &IndexMap<String, ScriptSpec>,
    name: &str,
    trail: &[&str],
) -> Result<()> {
    let location = format!("scripts.{name}");
    let members = scripts[name].members();
    for member in members {
        if !scripts.contains_key(member) {
            let mut known: Vec<&str> = scripts.keys().map(String::as_str).collect();
            known.sort_unstable();
            return Err(Error::config(format!(
                "{location}: member {} names no script (known: {})",
                repr(member),
                known.join(", ")
            )));
        }
        if members.iter().filter(|other| *other == member).count() > 1 {
            return Err(Error::config(format!(
                "{location}: member {} is listed more than once",
                repr(member)
            )));
        }
        let mut chain = trail.to_vec();
        chain.push(member);
        if trail.contains(&member.as_str()) {
            return Err(Error::config(format!(
                "{location}: groups form a cycle: {}",
                chain.join(" -> ")
            )));
        }
        validate_members(scripts, member, &chain)?;
    }
    Ok(())
}

/// The merged layers before entries are normalized: scalars and lists as
/// the last file to set them left them, mappings name → entry.
struct Merged {
    worktrees_dir: String,
    opener: String,
    symlinks: Vec<String>,
    setup_scripts: Vec<String>,
    stop_timeout: Number,
    maps: IndexMap<&'static str, IndexMap<String, Value>>,
}

impl Merged {
    fn defaults() -> Self {
        let defaults = Config::default();
        Self {
            worktrees_dir: defaults.worktrees_dir,
            opener: defaults.opener,
            symlinks: defaults.symlinks,
            setup_scripts: defaults.setup_scripts,
            stop_timeout: defaults.stop_timeout,
            maps: SCHEMA
                .iter()
                .filter(|(_, kind)| matches!(kind, Kind::Map(_) | Kind::Section))
                .map(|(name, _)| (*name, IndexMap::new()))
                .collect(),
        }
    }

    /// Scalars/lists replace; mappings merge per key with null deleting.
    fn merge(&mut self, overlay: Vec<(String, Value)>) {
        for (key, value) in overlay {
            match (key.as_str(), value) {
                ("worktrees_dir", Value::Str(text)) => self.worktrees_dir = text,
                ("opener", Value::Str(text)) => self.opener = text,
                ("symlinks", value) => self.symlinks = value.as_string_list().unwrap_or_default(),
                ("setup_scripts", value) => {
                    self.setup_scripts = value.as_string_list().unwrap_or_default();
                }
                ("stop_timeout", Value::Number(number)) => self.stop_timeout = number,
                (name, value) => {
                    let Some(combined) = self.maps.get_mut(name) else {
                        continue;
                    };
                    for (entry_name, entry) in value.as_string_map().unwrap_or_default() {
                        if entry == Value::Null {
                            combined.shift_remove(&entry_name);
                        } else {
                            combined.insert(entry_name, entry);
                        }
                    }
                }
            }
        }
    }

    fn entries<T>(&self, key: &str, normalize: impl Fn(&Value) -> T) -> IndexMap<String, T> {
        self.maps[key].iter().map(|(name, entry)| (name.clone(), normalize(entry))).collect()
    }
}

/// A validated entry as (key → value) lookups; a string is `command`.
struct Entry<'a>(&'a Value);

impl Entry<'_> {
    fn get(&self, key: &str) -> Option<&Value> {
        match self.0 {
            Value::Str(_) if key == "command" => Some(self.0),
            Value::Map(pairs) => {
                pairs.iter().find(|(name, _)| name.as_str() == Some(key)).map(|(_, value)| value)
            }
            _ => None,
        }
    }

    fn text(&self, key: &str) -> Option<String> {
        self.get(key).and_then(Value::as_str).map(str::to_string)
    }

    fn flag(&self, key: &str) -> Option<bool> {
        match self.get(key) {
            Some(Value::Bool(flag)) => Some(*flag),
            _ => None,
        }
    }

    fn names(&self, key: &str) -> Option<Vec<String>> {
        self.get(key).and_then(Value::as_string_list)
    }
}

fn normalize_opener(entry: &Value) -> OpenerSpec {
    let entry = Entry(entry);
    OpenerSpec {
        command: entry.text("command"),
        from: entry.text("from"),
        wrap: entry.text("wrap"),
        background: entry.flag("background"),
    }
}

fn normalize_wrapper(entry: &Value) -> CommandSpec {
    let entry = Entry(entry);
    CommandSpec {
        command: entry.text("command").unwrap_or_default(),
        background: entry.flag("background").unwrap_or(false),
    }
}

fn normalize_script(entry: &Value) -> ScriptSpec {
    let entry = Entry(entry);
    ScriptSpec {
        command: entry.text("command"),
        bulk: entry.names("bulk"),
        pipeline: entry.names("pipeline"),
        background: entry.flag("background").unwrap_or(false),
        exclusive: entry.flag("exclusive").unwrap_or(false),
        hidden: entry.flag("hidden").unwrap_or(false),
        cleanup: entry.text("cleanup"),
        stop_timeout: match entry.get("stop_timeout") {
            Some(Value::Number(number)) => Some(*number),
            _ => None,
        },
    }
}

/// Where the layers below the project's are looked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    pub system_dir: PathBuf,
    pub user_dir: PathBuf,
}

impl Roots {
    /// `/etc/workforest`, and `workforest` under `$XDG_CONFIG_HOME` (or
    /// `~/.config`).
    pub fn standard() -> Self {
        let base = match util::env_nonempty("XDG_CONFIG_HOME") {
            Some(xdg) => PathBuf::from(xdg),
            None => util::home_dir().join(".config"),
        };
        Self { system_dir: SYSTEM_CONFIG_DIR.into(), user_dir: base.join("workforest") }
    }
}

fn first_existing(directory: &Path, basenames: &[&str]) -> Option<PathBuf> {
    basenames.iter().map(|basename| directory.join(basename)).find(|candidate| candidate.is_file())
}

/// The config files in effect, lowest layer first.
pub fn layer_files(roots: &Roots, main_worktree: Option<&Path>) -> Vec<ConfigSource> {
    let mut layers = Vec::new();
    if let Some(path) = first_existing(&roots.system_dir, &GLOBAL_BASENAMES) {
        layers.push(ConfigSource { layer: "system", path });
    }
    if let Some(path) = first_existing(&roots.user_dir, &GLOBAL_BASENAMES) {
        layers.push(ConfigSource { layer: "user", path });
    }
    if let Some(main) = main_worktree {
        if let Some(path) = first_existing(main, &PROJECT_BASENAMES) {
            layers.push(ConfigSource { layer: "project", path });
        }
        if let Some(path) = PROJECT_LOCAL_DIRS
            .iter()
            .find_map(|local| first_existing(&main.join(local), &PROJECT_BASENAMES))
        {
            layers.push(ConfigSource { layer: "project-local", path });
        }
    }
    layers
}

/// Load and merge all layers; `main_worktree = None` skips project layers.
pub fn load_config(main_worktree: Option<&Path>) -> Result<Config> {
    load_config_in(&Roots::standard(), main_worktree)
}

pub fn load_config_in(roots: &Roots, main_worktree: Option<&Path>) -> Result<Config> {
    let mut merged = Merged::defaults();
    let mut sources = Vec::new();
    for source in layer_files(roots, main_worktree) {
        merged.merge(validate(parse_file(&source.path)?, &source.path)?);
        sources.push(source);
    }
    let make = Entry(&Value::Map(
        merged.maps["make"].iter().map(|(key, value)| (s(key), value.clone())).collect(),
    ))
    .into_make();
    let config = Config {
        openers: merged.entries("openers", normalize_opener),
        wrappers: merged.entries("wrappers", normalize_wrapper),
        scripts: merged.entries("scripts", normalize_script),
        worktrees_dir: merged.worktrees_dir,
        opener: merged.opener,
        symlinks: merged.symlinks,
        setup_scripts: merged.setup_scripts,
        stop_timeout: merged.stop_timeout,
        make,
        sources,
    };
    validate_references(&config)?;
    Ok(config)
}

impl Entry<'_> {
    fn into_make(self) -> MakeSpec {
        MakeSpec {
            hidden: self.flag("hidden").unwrap_or(false),
            hide_scripts: self.names("hide_scripts").unwrap_or_default(),
            show_scripts: self.names("show_scripts").unwrap_or_default(),
            exclusive_scripts: self.names("exclusive_scripts").unwrap_or_default(),
        }
    }
}

/// `$name`, `${name}` and `$$` in a template, substituted; a `$` that
/// starts none of them, or a name with no value, is an error.
fn substitute(
    template: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> std::result::Result<String, String> {
    let is_start = |c: char| c == '_' || c.is_ascii_alphabetic();
    let is_part = |c: char| c == '_' || c.is_ascii_alphanumeric();
    let mut out = String::new();
    let mut rest = template;
    while let Some(dollar) = rest.find('$') {
        out.push_str(&rest[..dollar]);
        let offset = template.len() - rest.len() + dollar;
        let after = &rest[dollar + 1..];
        let name_len = |text: &str| {
            if text.starts_with(is_start) {
                text.find(|c| !is_part(c)).unwrap_or(text.len())
            } else {
                0
            }
        };
        if let Some(literal) = after.strip_prefix('$') {
            out.push('$');
            rest = literal;
            continue;
        }
        let (name, consumed) = if name_len(after) > 0 {
            (&after[..name_len(after)], name_len(after))
        } else if let Some(braced) = after.strip_prefix('{')
            && name_len(braced) > 0
            && braced[name_len(braced)..].starts_with('}')
        {
            (&braced[..name_len(braced)], name_len(braced) + 2)
        } else {
            let before = &template[..offset];
            let line = before.matches('\n').count() + 1;
            let column = before.rfind('\n').map_or(before, |newline| &before[newline + 1..]);
            let column = column.chars().count() + 1;
            return Err(format!("Invalid placeholder in string: line {line}, col {column}"));
        };
        out.push_str(&lookup(name).ok_or_else(|| repr(name))?);
        rest = &after[consumed..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The WF_* family as template variables.
pub fn template_vars(main_worktree: &Path) -> [(&'static str, String); 2] {
    [
        ("WF_MAIN", main_worktree.to_string_lossy().into_owned()),
        ("WF_NAME", util::file_name(main_worktree)),
    ]
}

/// Expand $WF_* and environment variables, then normalize the path.
pub fn resolve_worktrees_dir(config: &Config, main_worktree: &Path) -> Result<PathBuf> {
    resolve_worktrees_dir_with(config, main_worktree, &|name| {
        env::var_os(name).map(|value| value.to_string_lossy().into_owned())
    })
}

fn resolve_worktrees_dir_with(
    config: &Config,
    main_worktree: &Path,
    environment: &dyn Fn(&str) -> Option<String>,
) -> Result<PathBuf> {
    let variables = template_vars(main_worktree);
    let lookup = |name: &str| {
        variables
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, value)| value.clone())
            .or_else(|| environment(name))
    };
    let expanded = substitute(&config.worktrees_dir, &lookup).map_err(|error| {
        Error::config(format!("worktrees_dir {}: {error}", repr(&config.worktrees_dir)))
    })?;
    let mut path = util::expand_user(&expanded);
    if !path.is_absolute() {
        path = main_worktree.join(path);
    }
    Ok(util::normalize(&path))
}

#[cfg(test)]
mod tests;
