//! Reading a YAML document into a [`Value`].
//!
//! The parser gives syntax events; what a scalar *means* is decided here,
//! by the YAML 1.1 rules in `value.rs`. Anchors, aliases and merge keys
//! (`<<`) work; tags outside the standard scalar ones are refused, as a
//! safe loader does.

use std::collections::HashMap;

use saphyr_parser::{Event, Parser, ScalarStyle, Tag};

use super::value::{Number, Plain, Value, resolve_plain};

/// What stands between the events and a finished value.
enum Frame {
    List { anchor: usize, items: Vec<Value> },
    Map { anchor: usize, pairs: Vec<(Value, Value, bool)>, key: Option<(Value, bool)> },
}

fn core_tag(tag: &Option<std::borrow::Cow<'_, Tag>>) -> Result<Option<String>, String> {
    let Some(tag) = tag else {
        return Ok(None);
    };
    if tag.handle == "tag:yaml.org,2002:" || tag.handle == "!!" {
        return Ok(Some(tag.suffix.clone()));
    }
    if format!("{}{}", tag.handle, tag.suffix) == "!" {
        return Ok(Some("str".into())); // the non-specific tag: "as written"
    }
    Err(format!("could not determine a constructor for the tag '{}{}'", tag.handle, tag.suffix))
}

fn scalar(text: &str, style: ScalarStyle, tag: Option<String>) -> Result<(Value, bool), String> {
    let resolved = match tag.as_deref() {
        None if style == ScalarStyle::Plain => resolve_plain(text),
        None | Some("str") => Plain::Str,
        Some("null") => Plain::Null,
        Some(kind @ ("bool" | "int" | "float" | "timestamp")) => {
            let plain = resolve_plain(text);
            let fits = matches!(
                (kind, &plain),
                ("bool", Plain::Bool(_))
                    | ("int", Plain::Number(Number::Int(_)))
                    | ("float", Plain::Number(_))
                    | ("timestamp", Plain::Timestamp { .. })
            );
            if !fits {
                return Err(format!("cannot read {text:?} as a {kind}"));
            }
            match plain {
                Plain::Number(number) if kind == "float" => {
                    Plain::Number(Number::Float(number.as_f64()))
                }
                other => other,
            }
        }
        Some(other) => {
            return Err(format!(
                "could not determine a constructor for the tag 'tag:yaml.org,2002:{other}'"
            ));
        }
    };
    Ok(match resolved {
        Plain::Null => (Value::Null, false),
        Plain::Bool(flag) => (Value::Bool(flag), false),
        Plain::Number(number) => (Value::Number(number), false),
        Plain::Timestamp { date_only } => (Value::Timestamp { date_only }, false),
        Plain::Merge => (Value::Str(text.to_string()), true),
        Plain::Str => (Value::Str(text.to_string()), false),
    })
}

/// The pairs a `<<` value contributes: a mapping's own, or — for a list of
/// mappings — every one's, the earlier ones winning.
fn merged_pairs(value: Value) -> Result<Vec<(Value, Value)>, String> {
    match value {
        Value::Map(pairs) => Ok(pairs),
        Value::List(items) => {
            let mut all = Vec::new();
            for item in items.into_iter().rev() {
                match item {
                    Value::Map(pairs) => all.extend(pairs),
                    _ => return Err("expected a mapping for merging".into()),
                }
            }
            Ok(all)
        }
        _ => Err("expected a mapping or list of mappings for merging".into()),
    }
}

/// Later duplicates replace earlier ones in place, as a dict does; what a
/// merge key brought in ranks below the mapping's own keys.
fn finish_map(pairs: Vec<(Value, Value, bool)>) -> Result<Value, String> {
    let mut merged = Vec::new();
    let mut own = Vec::new();
    for (key, value, is_merge) in pairs {
        if is_merge {
            merged.extend(merged_pairs(value)?);
        } else {
            own.push((key, value));
        }
    }
    let mut out: Vec<(Value, Value)> = Vec::new();
    for (key, value) in merged.into_iter().chain(own) {
        if matches!(key, Value::List(_) | Value::Map(_)) {
            return Err("found unhashable key".into());
        }
        match out.iter_mut().find(|(existing, _)| *existing == key) {
            Some(slot) => slot.1 = value,
            None => out.push((key, value)),
        }
    }
    Ok(Value::Map(out))
}

/// Parse one YAML document; an empty one is `Null`.
pub fn load(text: &str) -> Result<Value, String> {
    let mut parser = Parser::new_from_str(text);
    let mut stack: Vec<Frame> = Vec::new();
    let mut anchors: HashMap<usize, Value> = HashMap::new();
    let mut root: Option<Value> = None;
    let mut documents = 0;

    while let Some(event) = parser.next_event() {
        let (event, span) = event.map_err(|error| {
            let marker = error.marker();
            format!("{} at line {}, column {}", error.info(), marker.line(), marker.col() + 1)
        })?;
        let at = |message: String| {
            format!("{message} at line {}, column {}", span.start.line(), span.start.col() + 1)
        };
        // A finished node: (value, is a merge key, anchor id).
        let mut done: Option<(Value, bool, usize)> = None;
        match event {
            Event::DocumentStart(_) => {
                documents += 1;
                if documents > 1 {
                    return Err(at("expected a single document in the stream".into()));
                }
            }
            Event::Scalar(text, style, anchor, tag) => {
                let (value, is_merge) =
                    scalar(&text, style, core_tag(&tag).map_err(&at)?).map_err(&at)?;
                done = Some((value, is_merge, anchor));
            }
            Event::Alias(id) => {
                let value =
                    anchors.get(&id).cloned().ok_or_else(|| at("found undefined alias".into()))?;
                done = Some((value, false, 0));
            }
            Event::SequenceStart(anchor, tag) => {
                if !matches!(core_tag(&tag).map_err(&at)?.as_deref(), None | Some("seq")) {
                    return Err(at("unsupported tag on a sequence".into()));
                }
                stack.push(Frame::List { anchor, items: Vec::new() });
            }
            Event::MappingStart(anchor, tag) => {
                if !matches!(core_tag(&tag).map_err(&at)?.as_deref(), None | Some("map")) {
                    return Err(at("unsupported tag on a mapping".into()));
                }
                stack.push(Frame::Map { anchor, pairs: Vec::new(), key: None });
            }
            Event::SequenceEnd => {
                if let Some(Frame::List { anchor, items }) = stack.pop() {
                    done = Some((Value::List(items), false, anchor));
                }
            }
            Event::MappingEnd => {
                if let Some(Frame::Map { anchor, pairs, .. }) = stack.pop() {
                    done = Some((finish_map(pairs).map_err(&at)?, false, anchor));
                }
            }
            Event::Nothing | Event::StreamStart | Event::StreamEnd | Event::DocumentEnd => {}
        }
        let Some((value, is_merge, anchor)) = done else {
            continue;
        };
        if anchor != 0 {
            anchors.insert(anchor, value.clone());
        }
        match stack.last_mut() {
            None => root = Some(value),
            Some(Frame::List { items, .. }) => items.push(value),
            Some(Frame::Map { pairs, key, .. }) => match key.take() {
                None => *key = Some((value, is_merge)),
                Some((name, merge)) => pairs.push((name, value, merge)),
            },
        }
    }
    Ok(root.unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> Value {
        Value::Str(text.into())
    }

    fn int(value: i64) -> Value {
        Value::Number(Number::Int(value))
    }

    fn map(pairs: &[(&str, Value)]) -> Value {
        Value::Map(pairs.iter().map(|(key, value)| (s(key), value.clone())).collect())
    }

    #[test]
    fn empty_documents_are_null() {
        for text in ["", "\n", "# only a comment\n", "---\n", "~\n"] {
            assert_eq!(load(text), Ok(Value::Null), "{text:?}");
        }
    }

    #[test]
    fn scalars_resolve_only_when_plain() {
        let value = load(
            "a: yes\nb: 'yes'\nc: \"12\"\nd: 12\ne: 2.5\nf: ~\ng: |\n  on\nh: plain text\ni: 2024-01-31\n",
        )
        .unwrap();
        assert_eq!(
            value,
            map(&[
                ("a", Value::Bool(true)),
                ("b", s("yes")),
                ("c", s("12")),
                ("d", int(12)),
                ("e", Value::Number(Number::Float(2.5))),
                ("f", Value::Null),
                ("g", s("on\n")),
                ("h", s("plain text")),
                ("i", Value::Timestamp { date_only: true }),
            ])
        );
    }

    #[test]
    fn collections_nest_and_keep_order() {
        let value = load(
            "scripts:\n  b: [x, y]\n  a: {command: make, hidden: true}\nlist:\n  - 1\n  - - 2\n",
        )
        .unwrap();
        assert_eq!(
            value,
            map(&[
                (
                    "scripts",
                    map(&[
                        ("b", Value::List(vec![s("x"), s("y")])),
                        ("a", map(&[("command", s("make")), ("hidden", Value::Bool(true))])),
                    ])
                ),
                ("list", Value::List(vec![int(1), Value::List(vec![int(2)])])),
            ])
        );
        assert_eq!(load("- a\n- b\n"), Ok(Value::List(vec![s("a"), s("b")])));
        assert_eq!(load("just text\n"), Ok(s("just text")));
    }

    #[test]
    fn a_duplicate_key_keeps_its_first_place_and_last_value() {
        assert_eq!(load("a: 1\nb: 2\na: 3\n"), Ok(map(&[("a", int(3)), ("b", int(2))])));
    }

    #[test]
    fn non_string_keys_survive() {
        assert_eq!(
            load("1: a\ntrue: b\n"),
            Ok(Value::Map(vec![(int(1), s("a")), (Value::Bool(true), s("b"))]))
        );
        assert!(load("[a]: b\n").unwrap_err().contains("unhashable"));
    }

    #[test]
    fn anchors_aliases_and_merge_keys() {
        let value = load(
            "base: &base {command: make, hidden: true}\n\
             one:\n  <<: *base\n  command: other\n\
             two:\n  <<: [{a: 1}, {a: 2, b: 3}]\n\
             again: *base\n",
        )
        .unwrap();
        let base = map(&[("command", s("make")), ("hidden", Value::Bool(true))]);
        assert_eq!(
            value,
            map(&[
                ("base", base.clone()),
                ("one", map(&[("command", s("other")), ("hidden", Value::Bool(true))])),
                ("two", map(&[("a", int(1)), ("b", int(3))])),
                ("again", base),
            ])
        );
        assert!(load("a:\n  <<: text\n").unwrap_err().contains("merging"));
        assert!(load("a:\n  <<: [text]\n").unwrap_err().contains("merging"));
        // a quoted "<<" is an ordinary key
        assert_eq!(load("'<<': x\n"), Ok(map(&[("<<", s("x"))])));
    }

    #[test]
    fn standard_tags_only() {
        assert_eq!(
            load(
                "a: !!str 12\nb: !!int '12'\nc: !!float 3\nd: !!null ''\ne: ! 5\nf: !!bool 'yes'\n"
            ),
            Ok(map(&[
                ("a", s("12")),
                ("b", int(12)),
                ("c", Value::Number(Number::Float(3.0))),
                ("d", Value::Null),
                ("e", s("5")),
                ("f", Value::Bool(true)),
            ]))
        );
        assert_eq!(
            load("a: !!seq [x]\nb: !!map {k: v}\n").unwrap().as_string_map().unwrap().len(),
            2
        );
        for text in [
            "a: !custom x\n",
            "a: !!python/object:os.system x\n",
            "a: !!int nope\n",
            "a: !!set [x]\n",
            "a: !!seq {k: v}\n",
        ] {
            assert!(load(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn syntax_errors_say_where() {
        let error = load("opener: [unclosed\n").unwrap_err();
        assert!(error.contains("line "), "{error}");
        assert!(load("a: *nope\n").is_err());
    }

    #[test]
    fn one_document_only() {
        assert!(load("a: 1\n---\nb: 2\n").unwrap_err().contains("single document"));
        assert_eq!(load("---\na: 1\n...\n"), Ok(map(&[("a", int(1))])));
    }
}
