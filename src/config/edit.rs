//! Changing one config file in place: set or remove a key, and leave the
//! rest of the file — comments, ordering, quoting, blank lines, keys this
//! version does not know — exactly as its author wrote it.
//!
//! No command uses this yet; it is what config editing from the editor
//! plugins will be built on, and the reason reading and writing do not
//! share a parse-and-dump round trip. YAML goes through a lossless syntax
//! tree (`yaml-edit`); the new value's own text comes from `dump.rs`, so
//! it is quoted by the same rules our reader applies. JSON has no comments
//! to keep and is re-serialized with its key order intact.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use yaml_edit::{Mapping, YamlFile};

use super::value::Value;
use super::{dump, yaml};
use crate::errors::{Error, Result};
use crate::util;

/// One config file being edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFile {
    path: PathBuf,
    text: String,
}

fn is_json(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "json")
}

/// `{a: {b: value}}` for the key path `[a, b]`.
fn nested(keys: &[&str], value: &Value) -> Value {
    keys.iter()
        .rev()
        .fold(value.clone(), |inner, key| Value::Map(vec![(Value::Str(key.to_string()), inner)]))
}

/// What the document holds at a key path, by our own reading of it.
fn lookup<'a>(mut value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    for key in keys {
        let Value::Map(pairs) = value else {
            return None;
        };
        value =
            pairs.iter().find(|(name, _)| name.as_str() == Some(key)).map(|(_, value)| value)?;
    }
    Some(value)
}

impl ConfigFile {
    /// The file as it is on disk; one that does not exist yet starts empty.
    pub fn open(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(self::error(path, &util::os_error_text(&error))),
        };
        Ok(Self::from_text(path, text))
    }

    pub fn from_text(path: &Path, text: impl Into<String>) -> Self {
        Self { path: path.to_path_buf(), text: text.into() }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    fn fail(&self, message: &str) -> Error {
        error(&self.path, message)
    }

    fn parsed(&self, text: &str) -> Result<Value> {
        if is_json(&self.path) {
            if text.trim().is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str::<serde_json::Value>(text)
                .map(Value::from_json)
                .map_err(|error| self.fail(&format!("invalid JSON: {error}")));
        }
        yaml::load(text).map_err(|error| self.fail(&format!("invalid YAML: {error}")))
    }

    /// Set the value at a key path (`["scripts", "test"]`), creating the
    /// mappings on the way that are not there yet.
    pub fn set(&mut self, keys: &[&str], value: &Value) -> Result<()> {
        let (Some((last, parents)), false) = (keys.split_last(), keys.is_empty()) else {
            return Err(self.fail("no key to set"));
        };
        let current = self.parsed(&self.text)?;
        if !matches!(current, Value::Null | Value::Map(_)) {
            return Err(
                self.fail(&format!("top level must be a mapping, got {}", current.type_name()))
            );
        }
        if lookup(&current, keys) == Some(value) {
            return Ok(()); // already so: not a byte changes
        }
        if is_json(&self.path) {
            let text = self.set_json(keys, value)?;
            return self.accept(text, keys, Some(value));
        }
        if current == Value::Null {
            // Nothing but comments (or nothing at all): there is no
            // document to edit yet, so the first key starts one.
            let mut text = self.text.clone();
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&dump::dump(&nested(keys, value)));
            return self.accept(text, keys, Some(value));
        }
        // Block style first, as a person would write it; where the tree
        // editor cannot place a block (it misindents one at the top
        // level), the same value inline, which has no indentation to get
        // wrong.
        let block = self.set_yaml(parents, last, value, dump::dump)?;
        if self.accept(block, keys, Some(value)).is_ok() {
            return Ok(());
        }
        let inline = self.set_yaml(parents, last, value, |value| dump::flow(value) + "\n")?;
        self.accept(inline, keys, Some(value))
    }

    /// Remove the key at a key path; false when it was not there.
    pub fn unset(&mut self, keys: &[&str]) -> Result<bool> {
        let Some((last, parents)) = keys.split_last() else {
            return Err(self.fail("no key to remove"));
        };
        let current = self.parsed(&self.text)?;
        if lookup(&current, keys).is_none() {
            return Ok(false);
        }
        let text = if is_json(&self.path) {
            let mut root = current.to_json();
            let mut target = &mut root;
            for key in parents {
                target = &mut target[*key];
            }
            if let Some(object) = target.as_object_mut() {
                object.shift_remove(*last);
            }
            util::json_pretty(&root) + "\n"
        } else {
            let file = self.tree()?;
            let mapping = self.descend(&file, parents, false)?;
            mapping.remove(*last);
            file.to_string()
        };
        self.accept(text, keys, None)?;
        Ok(true)
    }

    fn set_json(&self, keys: &[&str], value: &Value) -> Result<String> {
        let mut root = match self.parsed(&self.text)? {
            Value::Null => serde_json::Value::Object(Default::default()),
            current => current.to_json(),
        };
        let mut target = &mut root;
        for key in keys {
            if !target.is_object() {
                return Err(self.fail(&format!("{} is not a mapping", keys.join("."))));
            }
            target = &mut target[*key];
            if target.is_null() && *key != keys[keys.len() - 1] {
                *target = serde_json::Value::Object(Default::default());
            }
        }
        *target = value.to_json();
        Ok(util::json_pretty(&root) + "\n")
    }

    fn tree(&self) -> Result<YamlFile> {
        YamlFile::from_str(&self.text).map_err(|error| self.fail(&format!("invalid YAML: {error}")))
    }

    /// The mapping at a key path, optionally creating what is missing.
    fn descend(&self, file: &YamlFile, keys: &[&str], create: bool) -> Result<Mapping> {
        let not_a_mapping =
            |upto: usize| self.fail(&format!("{} is not a mapping", keys[..upto].join(".")));
        let mut mapping = file
            .document()
            .and_then(|document| document.as_mapping())
            .ok_or_else(|| self.fail("top level must be a mapping"))?;
        for (index, key) in keys.iter().enumerate() {
            if !mapping.contains_key(*key) {
                if !create {
                    return Err(not_a_mapping(index + 1));
                }
                // The rest of the path does not exist: one insertion makes
                // all of it.
                return Err(Error::new(format!("\0missing:{index}")));
            }
            mapping = mapping.get_mapping(*key).ok_or_else(|| not_a_mapping(index + 1))?;
        }
        Ok(mapping)
    }

    fn set_yaml(
        &self,
        parents: &[&str],
        last: &str,
        value: &Value,
        render: impl Fn(&Value) -> String,
    ) -> Result<String> {
        let file = self.tree()?;
        // Where the path leaves what the file has, the remainder is
        // inserted as one nested value at that point.
        let (mapping, key, value) = match self.descend(&file, parents, true) {
            Ok(mapping) => (mapping, last, value.clone()),
            Err(error) => match error.message.strip_prefix("\0missing:") {
                Some(index) => {
                    let index: usize = index.parse().unwrap_or(0);
                    let mut rest = parents[index + 1..].to_vec();
                    rest.push(last);
                    (
                        self.descend(&file, &parents[..index], false)?,
                        parents[index],
                        nested(&rest, value),
                    )
                }
                None => return Err(error),
            },
        };
        let fragment = YamlFile::from_str(&render(&value))
            .ok()
            .and_then(|fragment| fragment.document())
            .ok_or_else(|| self.fail("cannot write this value"))?;
        if let Some(node) = fragment.as_mapping() {
            mapping.set(key, node);
        } else if let Some(node) = fragment.as_sequence() {
            mapping.set(key, node);
        } else if let Some(node) = fragment.as_scalar() {
            mapping.set(key, node);
        } else {
            return Err(self.fail("cannot write this value"));
        }
        Ok(file.to_string())
    }

    /// Take the edited text only if, read back, it says what was asked
    /// for — a mangled file is the one outcome worse than a refused edit.
    fn accept(&mut self, text: String, keys: &[&str], expected: Option<&Value>) -> Result<()> {
        let reread = self.parsed(&text).map_err(|_| self.fail("the edit would break the file"))?;
        if lookup(&reread, keys) != expected {
            return Err(self.fail(&format!(
                "cannot edit {} without rewriting the file; edit it by hand",
                keys.join(".")
            )));
        }
        self.text = text;
        Ok(())
    }

    /// Write the file, whole or not at all.
    pub fn save(&self) -> Result<()> {
        let io_error = |error: io::Error| self.fail(&util::os_error_text(&error));
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        let temporary = self.path.with_file_name(format!(".{}.tmp", util::file_name(&self.path)));
        fs::write(&temporary, &self.text).map_err(io_error)?;
        fs::rename(&temporary, &self.path).map_err(io_error)
    }
}

fn error(path: &Path, message: &str) -> Error {
    Error::config(format!("{}: {message}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Number;
    use crate::testing::Sandbox;

    fn s(text: &str) -> Value {
        Value::Str(text.into())
    }

    fn yaml_file(text: &str) -> ConfigFile {
        ConfigFile::from_text(Path::new("/x/.workforest.yaml"), text)
    }

    const SHIPPED: [&str; 3] = [
        include_str!("../workforest/examples/.workforest.yaml"),
        include_str!("../workforest/examples/config.yaml"),
        include_str!("../workforest/templates/project.yaml"),
    ];

    #[test]
    fn setting_what_is_already_set_changes_nothing() {
        let text = "# mine\nopener:   'vim'   # keep the spacing\n";
        let mut file = yaml_file(text);
        file.set(&["opener"], &s("vim")).unwrap();
        assert_eq!(file.text(), text);
    }

    #[test]
    fn a_scalar_changes_and_everything_around_it_stays() {
        let mut file = yaml_file(
            "# header\n\nopener: vim  # my editor\n\n# scripts below\nscripts:\n  test: make test   # fast\n  lint: make lint\n",
        );
        file.set(&["opener"], &s("code")).unwrap();
        file.set(&["scripts", "test"], &s("make check")).unwrap();
        assert_eq!(
            file.text(),
            "# header\n\nopener: code  # my editor\n\n# scripts below\nscripts:\n  test: make check   # fast\n  lint: make lint\n"
        );
    }

    #[test]
    fn new_keys_are_added_and_values_quoted_by_our_own_rules() {
        let mut file = yaml_file("opener: vim\nscripts:\n  test: make test\n");
        file.set(&["scripts", "on"], &s("yes")).unwrap();
        file.set(&["stop_timeout"], &Value::Number(Number::Int(45))).unwrap();
        file.set(&["worktrees_dir"], &s("$WF_MAIN/../wt: x")).unwrap();
        let reread = yaml::load(file.text()).unwrap();
        assert_eq!(lookup(&reread, &["scripts", "on"]), Some(&s("yes")));
        assert_eq!(lookup(&reread, &["stop_timeout"]), Some(&Value::Number(Number::Int(45))));
        assert_eq!(lookup(&reread, &["worktrees_dir"]), Some(&s("$WF_MAIN/../wt: x")));
        assert_eq!(lookup(&reread, &["scripts", "test"]), Some(&s("make test")));
        assert!(
            file.text().starts_with("opener: vim\nscripts:\n  test: make test\n"),
            "{}",
            file.text()
        );
    }

    #[test]
    fn missing_parents_are_created() {
        let mut file = yaml_file("opener: vim\n");
        file.set(&["make", "hidden"], &Value::Bool(true)).unwrap();
        file.set(
            &["scripts", "dev"],
            &Value::Map(vec![
                (s("command"), s("npm run dev")),
                (s("exclusive"), Value::Bool(true)),
            ]),
        )
        .unwrap();
        let reread = yaml::load(file.text()).unwrap();
        assert_eq!(lookup(&reread, &["make", "hidden"]), Some(&Value::Bool(true)));
        assert_eq!(lookup(&reread, &["scripts", "dev", "command"]), Some(&s("npm run dev")));
        assert_eq!(
            file.text(),
            "opener: vim\nmake: {hidden: true}\nscripts: {dev: {command: npm run dev, exclusive: true}}\n"
        );
        file.set(&["scripts", "dev", "cleanup"], &s("make: clean")).unwrap();
        file.set(
            &["scripts", "api"],
            &Value::Map(vec![(s("bulk"), Value::List(vec![s("a"), s("b")]))]),
        )
        .unwrap();
        let reread = yaml::load(file.text()).unwrap();
        assert_eq!(lookup(&reread, &["scripts", "dev", "cleanup"]), Some(&s("make: clean")));
        assert_eq!(
            lookup(&reread, &["scripts", "api", "bulk"]),
            Some(&Value::List(vec![s("a"), s("b")]))
        );
    }

    #[test]
    fn a_block_goes_in_as_a_block_where_the_file_already_nests() {
        let mut file = yaml_file("scripts:\n  test: make test\n");
        let dev =
            Value::Map(vec![(s("command"), s("npm run dev")), (s("exclusive"), Value::Bool(true))]);
        file.set(&["scripts", "dev"], &dev).unwrap();
        assert_eq!(
            file.text(),
            "scripts:\n  test: make test\n  dev:\n    command: npm run dev\n    exclusive: true\n"
        );
    }

    #[test]
    fn lists_and_mappings_are_values_too() {
        let mut file = yaml_file("symlinks:\n  - node_modules  # big\n  - .env\nopener: vim\n");
        let links = Value::List(vec![s(".venv"), s(".env")]);
        file.set(&["symlinks"], &links).unwrap();
        let reread = yaml::load(file.text()).unwrap();
        assert_eq!(lookup(&reread, &["symlinks"]), Some(&links));
        assert_eq!(lookup(&reread, &["opener"]), Some(&s("vim")));
    }

    #[test]
    fn unknown_keys_survive_a_write_untouched() {
        let mut file = yaml_file("future_key: {a: 1}  # from a newer workforest\nopener: vim\n");
        file.set(&["opener"], &s("code")).unwrap();
        file.set(&["symlinks"], &Value::List(vec![s(".env")])).unwrap();
        assert!(
            file.text()
                .starts_with("future_key: {a: 1}  # from a newer workforest\nopener: code\n")
        );
    }

    #[test]
    fn unset_removes_one_key_and_reports_whether_it_was_there() {
        let mut file = yaml_file(
            "# top\nopener: vim  # mine\nscripts:\n  test: make test\n  lint: make lint  # keep\n",
        );
        assert!(file.unset(&["scripts", "test"]).unwrap());
        assert!(!file.unset(&["scripts", "test"]).unwrap());
        assert!(!file.unset(&["nope", "deeper"]).unwrap());
        assert_eq!(
            file.text(),
            "# top\nopener: vim  # mine\nscripts:\n  lint: make lint  # keep\n"
        );
        assert!(file.unset(&["opener"]).unwrap());
        assert_eq!(file.text(), "# top\nscripts:\n  lint: make lint  # keep\n");
    }

    #[test]
    fn a_file_of_comments_gets_its_first_key_appended() {
        for text in SHIPPED {
            if yaml::load(text).unwrap() != Value::Null {
                continue;
            }
            let mut file = yaml_file(text);
            file.set(&["scripts", "test"], &s("make test")).unwrap();
            assert_eq!(file.text(), format!("{text}scripts:\n  test: make test\n"));
        }
        let mut empty = yaml_file("");
        empty.set(&["opener"], &s("vim")).unwrap();
        assert_eq!(empty.text(), "opener: vim\n");
        let mut unterminated = yaml_file("# note");
        unterminated.set(&["opener"], &s("vim")).unwrap();
        assert_eq!(unterminated.text(), "# note\nopener: vim\n");
    }

    #[test]
    fn the_shipped_examples_keep_every_other_byte() {
        for text in SHIPPED {
            if yaml::load(text).unwrap() == Value::Null {
                continue;
            }
            let mut file = yaml_file(text);
            file.set(&["stop_timeout"], &Value::Number(Number::Int(45))).unwrap();
            let edited = file.text().to_string();
            assert!(file.unset(&["stop_timeout"]).unwrap());
            // Whatever was there before is still there, line for line.
            let kept: Vec<&str> =
                edited.lines().filter(|line| text.lines().any(|old| old == *line)).collect();
            assert!(
                kept.len() >= text.lines().count() - 1,
                "an edit rewrote more than its own line"
            );
            assert_eq!(yaml::load(file.text()).unwrap(), {
                let mut original = yaml::load(text).unwrap();
                if let Value::Map(pairs) = &mut original {
                    pairs.retain(|(key, _)| key.as_str() != Some("stop_timeout"));
                }
                original
            });
        }
    }

    #[test]
    fn refusals() {
        let mut list = yaml_file("- a\n- b\n");
        assert!(
            list.set(&["opener"], &s("vim"))
                .unwrap_err()
                .message
                .contains("top level must be a mapping")
        );
        let mut scalar_parent = yaml_file("opener: vim\n");
        let error = scalar_parent.set(&["opener", "x"], &s("y")).unwrap_err();
        assert!(error.message.contains("opener is not a mapping"), "{error}");
        assert_eq!(scalar_parent.text(), "opener: vim\n");
        assert!(
            yaml_file("a: [unclosed\n")
                .set(&["a"], &s("b"))
                .unwrap_err()
                .message
                .contains("invalid YAML")
        );
        assert!(yaml_file("a: 1\n").set(&[], &s("b")).is_err());
        assert!(yaml_file("a: 1\n").unset(&[]).is_err());
        assert_eq!(scalar_parent.set(&["x"], &s("y")).err(), None);
    }

    #[test]
    fn json_files_keep_their_key_order() {
        let path = Path::new("/x/.workforest.json");
        let mut file = ConfigFile::from_text(
            path,
            "{\"scripts\": {\"b\": \"x\", \"a\": \"y\"}, \"opener\": \"vim\"}",
        );
        file.set(&["scripts", "a"], &s("z")).unwrap();
        file.set(&["make", "hidden"], &Value::Bool(true)).unwrap();
        assert!(file.unset(&["opener"]).unwrap());
        assert!(!file.unset(&["opener"]).unwrap());
        assert_eq!(
            file.text(),
            "{\n  \"scripts\": {\n    \"b\": \"x\",\n    \"a\": \"z\"\n  },\n  \"make\": {\n    \"hidden\": true\n  }\n}\n"
        );
        let mut empty = ConfigFile::from_text(path, "");
        empty.set(&["opener"], &s("vim")).unwrap();
        assert_eq!(empty.text(), "{\n  \"opener\": \"vim\"\n}\n");
        assert!(
            ConfigFile::from_text(path, "{nope")
                .set(&["a"], &s("b"))
                .unwrap_err()
                .message
                .contains("invalid JSON")
        );
        let mut scalar = ConfigFile::from_text(path, "{\"opener\": \"vim\"}");
        assert!(scalar.set(&["opener", "x"], &s("y")).is_err());
    }

    #[test]
    fn open_edit_save() {
        let sandbox = Sandbox::new();
        let path = sandbox.path().join("new").join("config.yaml");
        let mut file = ConfigFile::open(&path).unwrap();
        assert_eq!(file.text(), "");
        file.set(&["opener"], &s("vim")).unwrap();
        file.save().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "opener: vim\n");
        assert_eq!(
            fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1,
            "no temporary file left"
        );

        let mut again = ConfigFile::open(&path).unwrap();
        again.set(&["opener"], &s("code")).unwrap();
        again.save().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "opener: code\n");
        assert!(ConfigFile::open(sandbox.path()).is_err(), "a directory is not a config file");
    }
}
