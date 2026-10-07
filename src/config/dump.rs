//! Writing a [`Value`] as block-style YAML, for `wf config`.
//!
//! The text is what PyYAML's `safe_dump(sort_keys=False)` produces, down
//! to quoting and the folding of long lines at 80 columns: the dump is
//! meant to be pasted into a config file, and it is output people diff.

use super::value::{Value, plain_is_ambiguous};

const BEST_WIDTH: usize = 80;
const BREAKS: [char; 4] = ['\n', '\u{85}', '\u{2028}', '\u{2029}'];

fn is_break(ch: char) -> bool {
    BREAKS.contains(&ch)
}

/// Which styles a scalar may be written in.
struct Analysis {
    multiline: bool,
    allow_block_plain: bool,
    allow_single_quoted: bool,
}

fn analyze(scalar: &str) -> Analysis {
    let chars: Vec<char> = scalar.chars().collect();
    if chars.is_empty() {
        return Analysis { multiline: false, allow_block_plain: true, allow_single_quoted: true };
    }
    let is_space = |ch: char| "\0 \t\r\n\u{85}\u{2028}\u{2029}".contains(ch);
    let mut block_indicators = scalar.starts_with("---") || scalar.starts_with("...");
    let (mut line_breaks, mut special) = (false, false);
    let (mut leading, mut trailing, mut break_space, mut space_break) =
        (false, false, false, false);
    let (mut previous_space, mut previous_break) = (false, false);
    let mut preceded_by_whitespace = true;
    for (index, &ch) in chars.iter().enumerate() {
        let followed_by_whitespace = chars.get(index + 1).is_none_or(|next| is_space(*next));
        if index == 0 {
            if "#,[]{}&*!|>'\"%@`".contains(ch) || (ch == '-' && followed_by_whitespace) {
                block_indicators = true;
            }
            if "?:".contains(ch) && followed_by_whitespace {
                block_indicators = true;
            }
        } else if (ch == ':' && followed_by_whitespace) || (ch == '#' && preceded_by_whitespace) {
            block_indicators = true;
        }
        if is_break(ch) {
            line_breaks = true;
        }
        // Without allow_unicode, only printable ASCII goes out as written.
        if !(ch == '\n' || (' '..='~').contains(&ch)) {
            special = true;
        }
        let last = index == chars.len() - 1;
        if ch == ' ' {
            leading |= index == 0;
            trailing |= last;
            break_space |= previous_break;
            (previous_space, previous_break) = (true, false);
        } else if is_break(ch) {
            leading |= index == 0;
            trailing |= last;
            space_break |= previous_space;
            (previous_space, previous_break) = (false, true);
        } else {
            (previous_space, previous_break) = (false, false);
        }
        preceded_by_whitespace = is_space(ch);
    }
    let mut allow_block_plain = !(leading || trailing || line_breaks || block_indicators);
    let mut allow_single_quoted = true;
    if break_space || space_break || special {
        allow_block_plain = false;
        allow_single_quoted = false;
    }
    Analysis { multiline: line_breaks, allow_block_plain, allow_single_quoted }
}

struct Emitter {
    out: String,
    column: usize,
    indent: Option<usize>,
    whitespace: bool,
    indention: bool,
}

impl Emitter {
    fn write(&mut self, data: &str) {
        self.column += data.chars().count();
        self.out.push_str(data);
    }

    fn write_line_break(&mut self) {
        self.out.push('\n');
        self.column = 0;
        self.whitespace = true;
        self.indention = true;
    }

    fn write_indent(&mut self) {
        let indent = self.indent.unwrap_or(0);
        if !self.indention || self.column > indent || (self.column == indent && !self.whitespace) {
            self.write_line_break();
        }
        if self.column < indent {
            self.whitespace = true;
            let padding = " ".repeat(indent - self.column);
            self.write(&padding);
        }
    }

    fn write_indicator(
        &mut self,
        indicator: &str,
        need_whitespace: bool,
        whitespace: bool,
        indention: bool,
    ) {
        if !self.whitespace && need_whitespace {
            self.write(" ");
        }
        self.whitespace = whitespace;
        self.indention = self.indention && indention;
        self.write(indicator);
    }

    fn write_plain(&mut self, text: &str, split: bool) {
        if text.is_empty() {
            return;
        }
        if !self.whitespace {
            self.write(" ");
        }
        self.whitespace = false;
        self.indention = false;
        let chars: Vec<char> = text.chars().collect();
        let (mut spaces, mut start) = (false, 0);
        for end in 0..=chars.len() {
            let ch = chars.get(end).copied();
            if spaces {
                if ch != Some(' ') {
                    if start + 1 == end && self.column > BEST_WIDTH && split {
                        self.write_indent();
                        self.whitespace = false;
                        self.indention = false;
                    } else {
                        self.write(&chars[start..end].iter().collect::<String>());
                    }
                    start = end;
                }
            } else if ch.is_none() || ch == Some(' ') {
                self.write(&chars[start..end].iter().collect::<String>());
                start = end;
            }
            spaces = ch == Some(' ');
        }
    }

    fn write_single_quoted(&mut self, text: &str, split: bool) {
        self.write_indicator("'", true, false, false);
        let chars: Vec<char> = text.chars().collect();
        let (mut spaces, mut breaks, mut start) = (false, false, 0);
        for end in 0..=chars.len() {
            let ch = chars.get(end).copied();
            if spaces {
                if ch != Some(' ') {
                    if start + 1 == end
                        && self.column > BEST_WIDTH
                        && split
                        && start != 0
                        && end != chars.len()
                    {
                        self.write_indent();
                    } else {
                        self.write(&chars[start..end].iter().collect::<String>());
                    }
                    start = end;
                }
            } else if breaks {
                if ch.is_none_or(|ch| !is_break(ch)) {
                    if chars[start] == '\n' {
                        self.write_line_break();
                    }
                    for _ in start..end {
                        self.write_line_break();
                    }
                    self.write_indent();
                    start = end;
                }
            } else if ch.is_none_or(|ch| ch == ' ' || ch == '\'' || is_break(ch)) && start < end {
                self.write(&chars[start..end].iter().collect::<String>());
                start = end;
            }
            if ch == Some('\'') {
                self.write("''");
                start = end + 1;
            }
            spaces = ch == Some(' ');
            breaks = ch.is_some_and(is_break);
        }
        self.write_indicator("'", false, false, false);
    }

    fn write_double_quoted(&mut self, text: &str, split: bool) {
        self.write_indicator("\"", true, false, false);
        let chars: Vec<char> = text.chars().collect();
        let mut start = 0;
        for end in 0..=chars.len() {
            let ch = chars.get(end).copied();
            if ch.is_none_or(|ch| "\"\\".contains(ch) || !(' '..='~').contains(&ch)) {
                if start < end {
                    self.write(&chars[start..end].iter().collect::<String>());
                    start = end;
                }
                if let Some(ch) = ch {
                    let escaped = match ch {
                        '\0' => "\\0".to_string(),
                        '\x07' => "\\a".to_string(),
                        '\x08' => "\\b".to_string(),
                        '\t' => "\\t".to_string(),
                        '\n' => "\\n".to_string(),
                        '\x0b' => "\\v".to_string(),
                        '\x0c' => "\\f".to_string(),
                        '\r' => "\\r".to_string(),
                        '\x1b' => "\\e".to_string(),
                        '"' => "\\\"".to_string(),
                        '\\' => "\\\\".to_string(),
                        '\u{85}' => "\\N".to_string(),
                        '\u{a0}' => "\\_".to_string(),
                        '\u{2028}' => "\\L".to_string(),
                        '\u{2029}' => "\\P".to_string(),
                        ch if (ch as u32) <= 0xff => format!("\\x{:02X}", ch as u32),
                        ch if (ch as u32) <= 0xffff => format!("\\u{:04X}", ch as u32),
                        ch => format!("\\U{:08X}", ch as u32),
                    };
                    self.write(&escaped);
                    start = end + 1;
                }
            }
            if 0 < end
                && end + 1 < chars.len()
                && (ch == Some(' ') || start >= end)
                && self.column + end.saturating_sub(start) > BEST_WIDTH
                && split
            {
                let mut data: String = chars[start.min(end)..end].iter().collect();
                data.push('\\');
                if start < end {
                    start = end;
                }
                self.write(&data);
                self.write_indent();
                self.whitespace = false;
                self.indention = false;
                if chars[start] == ' ' {
                    self.write("\\");
                }
            }
        }
        self.write_indicator("\"", false, false, false);
    }

    /// A string in the least-quoted style that reads back as itself.
    fn string(&mut self, text: &str, simple_key: bool) {
        let analysis = analyze(text);
        let split = !simple_key;
        let never_plain = simple_key && (text.is_empty() || analysis.multiline);
        if !plain_is_ambiguous(text) && !never_plain && analysis.allow_block_plain {
            self.write_plain(text, split);
        } else if analysis.allow_single_quoted && !(simple_key && analysis.multiline) {
            self.write_single_quoted(text, split);
        } else {
            self.write_double_quoted(text, split);
        }
    }

    /// One level in: the first level starts at the margin (or, for what is
    /// written inline, one step from it); an `indentless` sequence stays
    /// where its key is. Returns the level to go back to.
    fn increase_indent(&mut self, inline: bool, indentless: bool) -> Option<usize> {
        let outer = self.indent;
        self.indent = match outer {
            None => Some(if inline { 2 } else { 0 }),
            Some(indent) if !indentless => Some(indent + 2),
            same => same,
        };
        outer
    }

    /// A scalar, with continuation lines one level in from where it sits.
    fn scalar(&mut self, value: &Value, simple_key: bool) {
        let outer = self.increase_indent(true, false);
        match value {
            Value::Str(text) => self.string(text, simple_key),
            Value::Null => self.write_plain("null", !simple_key),
            Value::Bool(flag) => {
                self.write_plain(if *flag { "true" } else { "false" }, !simple_key)
            }
            Value::Number(number) => self.write_plain(&number.text(), !simple_key),
            Value::Timestamp { .. } | Value::List(_) | Value::Map(_) => {}
        }
        self.indent = outer;
    }

    fn node(&mut self, value: &Value, in_mapping: bool) {
        match value {
            Value::Map(pairs) if pairs.is_empty() => {
                self.write_indicator("{", true, true, false);
                self.write_indicator("}", false, false, false);
            }
            Value::List(items) if items.is_empty() => {
                self.write_indicator("[", true, true, false);
                self.write_indicator("]", false, false, false);
            }
            Value::Map(pairs) => {
                let outer = self.increase_indent(false, false);
                for (key, entry) in pairs {
                    self.write_indent();
                    self.scalar(key, true);
                    self.write_indicator(":", false, false, false);
                    self.node(entry, true);
                }
                self.indent = outer;
            }
            Value::List(items) => {
                // A sequence that is a mapping value sits at the key's own
                // indentation.
                let outer = self.increase_indent(false, in_mapping && !self.indention);
                for item in items {
                    self.write_indent();
                    self.write_indicator("-", true, false, true);
                    self.node(item, false);
                }
                self.indent = outer;
            }
            scalar => self.scalar(scalar, false),
        }
    }
}

/// A string inside a flow collection: plain when nothing in it could be
/// taken for structure, else quoted.
fn flow_string(text: &str) -> String {
    let analysis = analyze(text);
    let structural = text.contains([',', '[', ']', '{', '}', ':', '?']);
    if !plain_is_ambiguous(text) && analysis.allow_block_plain && !structural {
        text.to_string()
    } else if analysis.allow_single_quoted && !analysis.multiline {
        format!("'{}'", text.replace('\'', "''"))
    } else {
        // A JSON string is a YAML double-quoted one.
        crate::util::json_compact(&text)
    }
}

/// The value on one line, collections in flow style (`{a: [b, c]}`).
pub fn flow(value: &Value) -> String {
    match value {
        Value::Str(text) => flow_string(text),
        Value::Null | Value::Timestamp { .. } => "null".into(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.text(),
        Value::List(items) => {
            format!("[{}]", items.iter().map(flow).collect::<Vec<_>>().join(", "))
        }
        Value::Map(pairs) => format!(
            "{{{}}}",
            pairs
                .iter()
                .map(|(key, value)| format!("{}: {}", flow(key), flow(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The value as a YAML document, ending in a newline.
pub fn dump(value: &Value) -> String {
    let mut emitter =
        Emitter { out: String::new(), column: 0, indent: None, whitespace: true, indention: true };
    emitter.node(value, false);
    emitter.write_line_break();
    emitter.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::yaml;

    fn s(text: &str) -> Value {
        Value::Str(text.into())
    }

    fn map(pairs: &[(&str, Value)]) -> Value {
        Value::Map(pairs.iter().map(|(key, value)| (s(key), value.clone())).collect())
    }

    fn strings(items: &[&str]) -> Value {
        Value::List(items.iter().map(|item| s(item)).collect())
    }

    // Expected texts below are what `yaml.safe_dump(data, sort_keys=False)`
    // printed for the same data (PyYAML 6.0.3).

    #[test]
    fn the_default_configuration() {
        let value = crate::config::Config::default().as_value();
        assert_eq!(
            dump(&value),
            "worktrees_dir: $WF_MAIN/../worktrees/$WF_NAME\n\
             opener: ''\n\
             openers: {}\n\
             wrappers: {}\n\
             symlinks: []\n\
             setup_scripts: []\n\
             scripts: {}\n\
             stop_timeout: 30.0\n\
             make:\n\
             \x20 hidden: false\n\
             \x20 hide_scripts: []\n\
             \x20 show_scripts: []\n\
             \x20 exclusive_scripts: []\n"
        );
    }

    #[test]
    fn nested_maps_and_indentless_lists() {
        let value = map(&[
            ("symlinks", strings(&["node_modules", ".env"])),
            (
                "scripts",
                map(&[
                    ("test", s("npm test")),
                    (
                        "dev",
                        map(&[
                            ("bulk", strings(&["backend", "frontend"])),
                            ("background", Value::Bool(true)),
                        ]),
                    ),
                ]),
            ),
            ("stop_timeout", Value::Number(crate::config::Number::Int(5))),
        ]);
        assert_eq!(
            dump(&value),
            "symlinks:\n- node_modules\n- .env\nscripts:\n  test: npm test\n  dev:\n    bulk:\n    - backend\n    - frontend\n    background: true\nstop_timeout: 5\n"
        );
    }

    #[test]
    fn strings_are_quoted_only_when_they_must_be() {
        let cases = [
            ("plain words", "plain words"),
            ("$EDITOR \"$WF_TARGET\"", "$EDITOR \"$WF_TARGET\""),
            ("true", "'true'"),
            ("12", "'12'"),
            ("", "''"),
            ("~", "'~'"),
            ("a: b", "'a: b'"),
            ("a #b", "'a #b'"),
            ("a#b", "a#b"),
            ("- x", "'- x'"),
            ("-x", "-x"),
            ("it's", "it's"),
            ("'quoted'", "'''quoted'''"),
            ("\"quoted\"", "'\"quoted\"'"),
            ("[x]", "'[x]'"),
            ("*star", "'*star'"),
            ("x*", "x*"),
            (" lead", "' lead'"),
            ("trail ", "'trail '"),
            ("a:b", "a:b"),
            ("key:", "'key:'"),
            ("? x", "'? x'"),
            ("---x", "'---x'"),
            ("tab\there", "\"tab\\there\""),
            ("bell\x07", "\"bell\\a\""),
            ("%x", "'%x'"),
            ("@x", "'@x'"),
            ("`x`", "'`x`'"),
            ("=", "'='"),
        ];
        for (text, expected) in cases {
            assert_eq!(dump(&map(&[("k", s(text))])), format!("k: {expected}\n"), "{text:?}");
        }
    }

    #[test]
    fn non_ascii_is_escaped() {
        let text = ["caf", "\u{e9}", " ", "\u{2014}", " ", "\u{1F600}"].concat();
        let expected = ["k: \"caf", "\\xE9 ", "\\u2014 ", "\\U0001F600\"\n"].concat();
        assert_eq!(dump(&map(&[("k", s(&text))])), expected);
    }

    #[test]
    fn multiline_strings() {
        assert_eq!(dump(&map(&[("k", s("a\nb"))])), "k: 'a\n\n  b'\n");
        assert_eq!(dump(&map(&[("k", s("a\n"))])), "k: 'a\n\n  '\n");
        assert_eq!(dump(&map(&[("k", s("a \nb"))])), "k: \"a \\nb\"\n");
        assert_eq!(dump(&map(&[("k", s("a\n b"))])), "k: \"a\\n b\"\n");
    }

    #[test]
    fn long_lines_fold_at_a_space_past_80_columns() {
        let command = "dropdb app_dev --if-exists && createdb app_dev && npm run db:migrate && npm run db:seed -- --all";
        assert_eq!(
            dump(&map(&[("scripts", map(&[("reset-db", s(command))]))])),
            "scripts:\n  reset-db: dropdb app_dev --if-exists && createdb app_dev && npm run db:migrate &&\n    npm run db:seed -- --all\n"
        );
        let quoted = "echo 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbb cccc': x";
        assert_eq!(
            dump(&map(&[("k", s(quoted))])),
            "k: 'echo ''aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n  bbbb cccc'': x'\n"
        );
        let list = map(&[("setup_scripts", strings(&[command]))]);
        assert_eq!(
            dump(&list),
            "setup_scripts:\n- dropdb app_dev --if-exists && createdb app_dev && npm run db:migrate && npm run\n  db:seed -- --all\n"
        );
    }

    #[test]
    fn long_double_quoted_strings_fold_with_a_backslash() {
        let text = format!("{}\t{}", "word ".repeat(18), "tail end");
        let dumped = dump(&map(&[("k", s(&text))]));
        assert_eq!(
            dumped,
            "k: \"word word word word word word word word word word word word word word word word\\\n  \\ word word \\ttail end\"\n"
        );
        assert_eq!(yaml::load(&dumped).unwrap(), map(&[("k", s(&text))]));
    }

    #[test]
    fn keys_are_quoted_like_values_and_never_folded() {
        assert_eq!(
            dump(&map(&[("make:build", s("x")), ("on", s("y")), ("a b", s("z"))])),
            "make:build: x\n'on': y\na b: z\n"
        );
    }

    #[test]
    fn scalars_and_empty_roots() {
        assert_eq!(dump(&Value::Map(vec![])), "{}\n");
        assert_eq!(dump(&Value::List(vec![])), "[]\n");
        assert_eq!(dump(&strings(&["a", "b"])), "- a\n- b\n");
        assert_eq!(
            dump(&map(&[("a", Value::Null), ("b", Value::List(vec![map(&[("c", s("d"))])]))])),
            "a: null\nb:\n- c: d\n"
        );
        let one = Value::Number(crate::config::Number::Int(1));
        let nested = map(&[
            ("l", Value::List(vec![strings(&["a", "b"]), Value::List(vec![])])),
            ("m", Value::List(vec![map(&[("a", one.clone()), ("b", Value::List(vec![one]))])])),
            ("e", map(&[("x", Value::Map(vec![]))])),
        ]);
        assert_eq!(dump(&nested), "l:\n- - a\n  - b\n- []\nm:\n- a: 1\n  b:\n  - 1\ne:\n  x: {}\n");
    }

    #[test]
    fn flow_style_is_one_line_and_reads_back_the_same() {
        let value = map(&[
            ("command", s("npm run dev")),
            ("list", strings(&["a", "b, c", "it's", "yes", "x: y", "multi\nline", ""])),
            ("nested", map(&[("flag", Value::Bool(true)), ("none", Value::Null)])),
            ("n", Value::Number(crate::config::Number::Float(2.5))),
        ]);
        let text = flow(&value);
        assert_eq!(
            text,
            "{command: npm run dev, list: [a, 'b, c', it's, 'yes', 'x: y', \"multi\\nline\", ''], nested: {flag: true, none: null}, n: 2.5}"
        );
        assert_eq!(yaml::load(&text).unwrap(), value);
        assert_eq!(flow(&Value::List(vec![])), "[]");
    }

    #[test]
    fn what_is_dumped_reads_back_the_same() {
        let value = map(&[
            ("opener", s("yes")),
            ("list", strings(&["a: b", "  padded  ", "multi\nline\n\ntext", "it's \"both\""])),
            (
                "nested",
                map(&[(
                    "deep",
                    map(&[("flag", Value::Bool(false)), ("text", s("#not a comment"))]),
                )]),
            ),
        ]);
        assert_eq!(yaml::load(&dump(&value)).unwrap(), value);
    }
}
