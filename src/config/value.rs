//! The data model config files parse into, and the scalar rules of the
//! YAML dialect they are written in.
//!
//! Config files are YAML 1.1 as PyYAML reads it — `yes`/`on` are booleans,
//! `~` is null, `010` is octal — and stay that way: what a file means must
//! not depend on which workforest reads it.

use indexmap::IndexMap;

/// A number that remembers whether it was written as an integer, so `60`
/// is shown as `60` and `30.0` as `30.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Number {
    Int(i64),
    Float(f64),
}

impl Number {
    pub fn as_f64(self) -> f64 {
        match self {
            Number::Int(value) => value as f64,
            Number::Float(value) => value,
        }
    }

    /// The text both YAML and JSON show for it.
    pub fn text(self) -> String {
        match self {
            Number::Int(value) => value.to_string(),
            Number::Float(value) if value.is_nan() => ".nan".into(),
            Number::Float(value) if value.is_infinite() => {
                if value > 0.0 {
                    ".inf".into()
                } else {
                    "-.inf".into()
                }
            }
            Number::Float(value) => format!("{value:?}"),
        }
    }

    pub fn to_json(self) -> serde_json::Value {
        match self {
            Number::Int(value) => value.into(),
            Number::Float(value) => serde_json::Number::from_f64(value)
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(Number),
    Str(String),
    /// A YAML timestamp: nothing in the schema takes one, so only its kind
    /// is kept, for the error that says so.
    Timestamp {
        date_only: bool,
    },
    List(Vec<Value>),
    Map(Vec<(Value, Value)>),
}

impl Value {
    /// The name error messages give the type.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "NoneType",
            Value::Bool(_) => "bool",
            Value::Number(Number::Int(_)) => "int",
            Value::Number(Number::Float(_)) => "float",
            Value::Str(_) => "str",
            Value::Timestamp { date_only: true } => "date",
            Value::Timestamp { date_only: false } => "datetime",
            Value::List(_) => "list",
            Value::Map(_) => "dict",
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(text) => Some(text),
            _ => None,
        }
    }

    /// The strings of a list made of strings only.
    pub fn as_string_list(&self) -> Option<Vec<String>> {
        match self {
            Value::List(items) => {
                items.iter().map(|item| item.as_str().map(str::to_string)).collect()
            }
            _ => None,
        }
    }

    /// The entries of a mapping whose keys are all strings.
    pub fn as_string_map(&self) -> Option<IndexMap<String, Value>> {
        match self {
            Value::Map(pairs) => pairs
                .iter()
                .map(|(key, value)| key.as_str().map(|key| (key.to_string(), value.clone())))
                .collect(),
            _ => None,
        }
    }

    /// How a message shows a key that should not be there.
    pub fn repr(&self) -> String {
        match self {
            Value::Null => "None".into(),
            Value::Bool(true) => "True".into(),
            Value::Bool(false) => "False".into(),
            Value::Number(number) => number.text(),
            Value::Str(text) => crate::util::repr(text),
            Value::Timestamp { .. } => "<timestamp>".into(),
            Value::List(items) => {
                format!("[{}]", items.iter().map(Value::repr).collect::<Vec<_>>().join(", "))
            }
            Value::Map(pairs) => format!(
                "{{{}}}",
                pairs
                    .iter()
                    .map(|(key, value)| format!("{}: {}", key.repr(), value.repr()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// The value as JSON, mapping keys in order; what JSON has no word
    /// for becomes null.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Null | Value::Timestamp { .. } => serde_json::Value::Null,
            Value::Bool(flag) => (*flag).into(),
            Value::Number(number) => number.to_json(),
            Value::Str(text) => text.as_str().into(),
            Value::List(items) => items.iter().map(Value::to_json).collect(),
            Value::Map(pairs) => serde_json::Value::Object(
                pairs
                    .iter()
                    .map(|(key, value)| {
                        (key.as_str().map_or_else(|| key.repr(), str::to_string), value.to_json())
                    })
                    .collect(),
            ),
        }
    }

    pub fn from_json(value: serde_json::Value) -> Value {
        match value {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(flag) => Value::Bool(flag),
            serde_json::Value::Number(number) => Value::Number(match number.as_i64() {
                Some(int) => Number::Int(int),
                None => Number::Float(number.as_f64().unwrap_or(f64::NAN)),
            }),
            serde_json::Value::String(text) => Value::Str(text),
            serde_json::Value::Array(items) => {
                Value::List(items.into_iter().map(Value::from_json).collect())
            }
            serde_json::Value::Object(map) => Value::Map(
                map.into_iter()
                    .map(|(key, value)| (Value::Str(key), Value::from_json(value)))
                    .collect(),
            ),
        }
    }
}

/// What an untagged plain scalar is.
#[derive(Debug, Clone, PartialEq)]
pub enum Plain {
    Null,
    Bool(bool),
    Number(Number),
    Timestamp {
        date_only: bool,
    },
    /// `<<`, the merge key.
    Merge,
    Str,
}

fn digits(text: &str, allowed: impl Fn(char) -> bool) -> bool {
    !text.is_empty() && text.chars().all(allowed)
}

fn unsigned(text: &str) -> &str {
    text.strip_prefix(['-', '+']).unwrap_or(text)
}

/// `[0-9][0-9_]*`
fn is_decimal(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_digit())
        && digits(text, |c| c.is_ascii_digit() || c == '_')
}

/// `(:[0-5]?[0-9])+`, the tail of a base-60 number.
fn is_sexagesimal_tail(text: &str) -> bool {
    !text.is_empty()
        && text.split(':').skip(1).all(|part| match part.as_bytes() {
            [d] => d.is_ascii_digit(),
            [t, d] => (b'0'..=b'5').contains(t) && d.is_ascii_digit(),
            _ => false,
        })
        && text.starts_with(':')
}

fn is_int(text: &str) -> bool {
    let body = unsigned(text);
    if let Some(binary) = body.strip_prefix("0b") {
        return digits(binary, |c| matches!(c, '0' | '1' | '_'));
    }
    if let Some(hex) = body.strip_prefix("0x") {
        return digits(hex, |c| c.is_ascii_hexdigit() || c == '_');
    }
    if body == "0" {
        return true;
    }
    if let Some(octal) = body.strip_prefix('0') {
        return digits(octal, |c| matches!(c, '0'..='7' | '_'));
    }
    if !body.starts_with(|c: char| matches!(c, '1'..='9')) {
        return false;
    }
    match body.find(':') {
        Some(colon) => is_decimal(&body[..colon]) && is_sexagesimal_tail(&body[colon..]),
        None => is_decimal(body),
    }
}

fn parse_int(text: &str) -> Number {
    let negative = text.starts_with('-');
    let body: String = unsigned(text).chars().filter(|c| *c != '_').collect();
    let parsed = if let Some(binary) = body.strip_prefix("0b") {
        i64::from_str_radix(binary, 2).ok()
    } else if let Some(hex) = body.strip_prefix("0x") {
        i64::from_str_radix(hex, 16).ok()
    } else if body.contains(':') {
        body.split(':').try_fold(0i64, |total, part| {
            total.checked_mul(60)?.checked_add(part.parse::<i64>().ok()?)
        })
    } else if body.len() > 1 && body.starts_with('0') {
        i64::from_str_radix(&body, 8).ok()
    } else {
        body.parse::<i64>().ok()
    };
    match parsed {
        Some(value) => Number::Int(if negative { -value } else { value }),
        // Beyond 64 bits: still a number, no longer an exact one.
        None => {
            let value = body.parse::<f64>().unwrap_or(f64::INFINITY);
            Number::Float(if negative { -value } else { value })
        }
    }
}

/// `[eE][-+][0-9]+`, or nothing.
fn is_exponent(text: &str) -> bool {
    text.is_empty()
        || text
            .strip_prefix(['e', 'E'])
            .and_then(|rest| rest.strip_prefix(['-', '+']))
            .is_some_and(|rest| digits(rest, |c| c.is_ascii_digit()))
}

fn split_exponent(text: &str) -> (&str, &str) {
    text.find(['e', 'E']).map_or((text, ""), |index| text.split_at(index))
}

fn is_float(text: &str) -> bool {
    let body = unsigned(text);
    if matches!(body, ".inf" | ".Inf" | ".INF") {
        return true;
    }
    if matches!(text, ".nan" | ".NaN" | ".NAN") {
        return true;
    }
    // `.5`, `.5e+3` — unsigned only
    if let Some(fraction) = text.strip_prefix('.') {
        let (mantissa, exponent) = split_exponent(fraction);
        return is_decimal(mantissa) && is_exponent(exponent);
    }
    let Some((whole, fraction)) = body.split_once('.') else {
        return false;
    };
    // `1:30.5`
    if let Some(colon) = whole.find(':') {
        return is_decimal(&whole[..colon])
            && is_sexagesimal_tail(&whole[colon..])
            && fraction.chars().all(|c| c.is_ascii_digit() || c == '_');
    }
    let (mantissa, exponent) = split_exponent(fraction);
    is_decimal(whole)
        && mantissa.chars().all(|c| c.is_ascii_digit() || c == '_')
        && is_exponent(exponent)
}

fn parse_float(text: &str) -> Number {
    let negative = text.starts_with('-');
    let body: String =
        unsigned(text).chars().filter(|c| *c != '_').collect::<String>().to_lowercase();
    let value = if body == ".inf" {
        f64::INFINITY
    } else if body == ".nan" {
        f64::NAN
    } else if body.contains(':') {
        body.split(':').fold(0.0, |total, part| total * 60.0 + part.parse::<f64>().unwrap_or(0.0))
    } else {
        body.parse::<f64>().unwrap_or(f64::NAN)
    };
    Number::Float(if negative { -value } else { value })
}

fn all_digits(text: &str, count: std::ops::RangeInclusive<usize>) -> bool {
    count.contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit())
}

/// `YYYY-MM-DD`, or a date and a time of day with an optional zone.
fn timestamp(text: &str) -> Option<bool> {
    let mut parts = text.splitn(3, '-');
    let (year, month, rest) = (parts.next()?, parts.next()?, parts.next()?);
    if !all_digits(year, 4..=4) {
        return None;
    }
    if all_digits(month, 2..=2) && all_digits(rest, 2..=2) {
        return Some(true);
    }
    let day_end = rest.find(|c: char| !c.is_ascii_digit())?;
    let (day, time) = rest.split_at(day_end);
    if !all_digits(month, 1..=2) || !all_digits(day, 1..=2) {
        return None;
    }
    let time = match time.strip_prefix(['T', 't']) {
        Some(time) => time,
        None => {
            let trimmed = time.trim_start_matches([' ', '\t']);
            if trimmed.len() == time.len() {
                return None;
            }
            trimmed
        }
    };
    let mut clock = time.splitn(3, ':');
    let (hour, minute, rest) = (clock.next()?, clock.next()?, clock.next()?);
    if !all_digits(hour, 1..=2) || !all_digits(minute, 2..=2) || !all_digits(rest.get(..2)?, 2..=2)
    {
        return None;
    }
    let mut rest = &rest[2..];
    if let Some(fraction) = rest.strip_prefix('.') {
        rest = fraction.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let zone = rest.trim_start_matches([' ', '\t']);
    let zone_ok = zone.is_empty()
        || zone == "Z"
        || zone.strip_prefix(['-', '+']).is_some_and(|offset| {
            let (hours, minutes) =
                offset.split_once(':').map_or((offset, None), |(h, m)| (h, Some(m)));
            all_digits(hours, 1..=2) && minutes.is_none_or(|m| all_digits(m, 2..=2))
        });
    zone_ok.then_some(false)
}

/// Resolve an untagged plain scalar the way YAML 1.1 does.
pub fn resolve_plain(text: &str) -> Plain {
    match text {
        "" | "~" | "null" | "Null" | "NULL" => return Plain::Null,
        "yes" | "Yes" | "YES" | "true" | "True" | "TRUE" | "on" | "On" | "ON" => {
            return Plain::Bool(true);
        }
        "no" | "No" | "NO" | "false" | "False" | "FALSE" | "off" | "Off" | "OFF" => {
            return Plain::Bool(false);
        }
        "<<" => return Plain::Merge,
        _ => {}
    }
    if !text.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '-' | '+' | '.')) {
        return Plain::Str;
    }
    if is_float(text) {
        return Plain::Number(parse_float(text));
    }
    if is_int(text) {
        return Plain::Number(parse_int(text));
    }
    match timestamp(text) {
        Some(date_only) => Plain::Timestamp { date_only },
        None => Plain::Str,
    }
}

/// Whether a string written plain would be read back as something else —
/// including `=`, which YAML 1.1 reserves — so that it must be quoted.
pub fn plain_is_ambiguous(text: &str) -> bool {
    text == "=" || resolve_plain(text) != Plain::Str
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(text: &str) -> Plain {
        Plain::Number(Number::Int(text.parse().unwrap()))
    }

    #[test]
    fn nulls_and_booleans_are_yaml_1_1() {
        for text in ["", "~", "null", "Null", "NULL"] {
            assert_eq!(resolve_plain(text), Plain::Null, "{text:?}");
        }
        for text in ["yes", "Yes", "YES", "true", "True", "TRUE", "on", "On", "ON"] {
            assert_eq!(resolve_plain(text), Plain::Bool(true), "{text:?}");
        }
        for text in ["no", "No", "NO", "false", "False", "FALSE", "off", "Off", "OFF"] {
            assert_eq!(resolve_plain(text), Plain::Bool(false), "{text:?}");
        }
        for text in ["y", "n", "nULL", "tRUE", "None", "yes please"] {
            assert_eq!(resolve_plain(text), Plain::Str, "{text:?}");
        }
        assert_eq!(resolve_plain("<<"), Plain::Merge);
    }

    #[test]
    fn integers() {
        assert_eq!(resolve_plain("0"), int("0"));
        assert_eq!(resolve_plain("60"), int("60"));
        assert_eq!(resolve_plain("-1"), int("-1"));
        assert_eq!(resolve_plain("+12"), int("12"));
        assert_eq!(resolve_plain("1_000"), int("1000"));
        assert_eq!(resolve_plain("010"), int("8"));
        assert_eq!(resolve_plain("0x1F"), int("31"));
        assert_eq!(resolve_plain("0b101"), int("5"));
        assert_eq!(resolve_plain("1:30"), int("90"));
        assert_eq!(resolve_plain("-1:00:00"), int("-3600"));
        for text in ["08", "1e3", "0o17", "1:60", "12a", "-", "+", "1:", "0x", "3000/tcp"] {
            assert_eq!(resolve_plain(text), Plain::Str, "{text:?}");
        }
        assert_eq!(
            resolve_plain("99999999999999999999"),
            Plain::Number(Number::Float(1e20)),
            "too big for 64 bits"
        );
    }

    #[test]
    fn floats() {
        let float = |value: f64| Plain::Number(Number::Float(value));
        assert_eq!(resolve_plain("2.5"), float(2.5));
        assert_eq!(resolve_plain("30.0"), float(30.0));
        assert_eq!(resolve_plain("1."), float(1.0));
        assert_eq!(resolve_plain("-0.5"), float(-0.5));
        assert_eq!(resolve_plain(".5"), float(0.5));
        assert_eq!(resolve_plain("1.5e+3"), float(1500.0));
        assert_eq!(resolve_plain("1_0.2_5"), float(10.25));
        assert_eq!(resolve_plain("1:30.5"), float(90.5));
        assert_eq!(resolve_plain(".inf"), float(f64::INFINITY));
        assert_eq!(resolve_plain("-.INF"), float(f64::NEG_INFINITY));
        assert!(matches!(resolve_plain(".nan"), Plain::Number(Number::Float(v)) if v.is_nan()));
        for text in ["1.5e3", "1.2.3", ".", "..", "-.5", ".e+1", "1.x", "+.nan", "./run"] {
            assert_eq!(resolve_plain(text), Plain::Str, "{text:?}");
        }
    }

    #[test]
    fn timestamps() {
        assert_eq!(resolve_plain("2024-01-31"), Plain::Timestamp { date_only: true });
        for text in [
            "2024-1-3 10:20:30",
            "2024-01-31T10:20:30Z",
            "2024-01-31t10:20:30.5 +01:00",
            "2024-01-31 10:20:30 -5",
        ] {
            assert_eq!(resolve_plain(text), Plain::Timestamp { date_only: false }, "{text:?}");
        }
        for text in [
            "2024-01",
            "2024-1-3",
            "24-01-31",
            "2024-01-31T10:20",
            "2024-01-31 10:20:30 UTC",
            "2024-01-31x",
        ] {
            assert_eq!(resolve_plain(text), Plain::Str, "{text:?}");
        }
    }

    #[test]
    fn ambiguity_is_what_needs_quotes() {
        for text in ["true", "", "12", "1.5", "~", "=", "<<", "2024-01-31"] {
            assert!(plain_is_ambiguous(text), "{text:?}");
        }
        for text in ["make test", "v1", "$EDITOR"] {
            assert!(!plain_is_ambiguous(text), "{text:?}");
        }
    }

    #[test]
    fn numbers_keep_their_spelling() {
        assert_eq!(Number::Int(60).text(), "60");
        assert_eq!(Number::Float(30.0).text(), "30.0");
        assert_eq!(Number::Float(2.5).text(), "2.5");
        assert_eq!(Number::Float(f64::INFINITY).text(), ".inf");
        assert_eq!(Number::Float(f64::NEG_INFINITY).text(), "-.inf");
        assert_eq!(Number::Float(f64::NAN).text(), ".nan");
        assert_eq!(Number::Int(3).as_f64(), 3.0);
        assert_eq!(Number::Int(60).to_json().to_string(), "60");
        assert_eq!(Number::Float(30.0).to_json().to_string(), "30.0");
        assert_eq!(Number::Float(f64::NAN).to_json(), serde_json::Value::Null);
    }

    #[test]
    fn type_names_and_reprs() {
        let map = Value::Map(vec![(Value::Str("a".into()), Value::Number(Number::Int(1)))]);
        let list = Value::List(vec![Value::Null, Value::Bool(true), Value::Bool(false)]);
        let cases = [
            (Value::Null, "NoneType", "None"),
            (Value::Bool(true), "bool", "True"),
            (Value::Number(Number::Int(1)), "int", "1"),
            (Value::Number(Number::Float(1.5)), "float", "1.5"),
            (Value::Str("x".into()), "str", "'x'"),
            (Value::Timestamp { date_only: true }, "date", "<timestamp>"),
            (Value::Timestamp { date_only: false }, "datetime", "<timestamp>"),
            (list, "list", "[None, True, False]"),
            (map, "dict", "{'a': 1}"),
        ];
        for (value, name, repr) in cases {
            assert_eq!(value.type_name(), name);
            assert_eq!(value.repr(), repr);
        }
    }

    #[test]
    fn typed_views() {
        let list = Value::List(vec![Value::Str("a".into()), Value::Str("b".into())]);
        assert_eq!(list.as_string_list(), Some(vec!["a".to_string(), "b".to_string()]));
        assert_eq!(Value::List(vec![Value::Null]).as_string_list(), None);
        assert_eq!(Value::Null.as_string_list(), None);
        let map = Value::Map(vec![(Value::Str("a".into()), Value::Null)]);
        assert_eq!(map.as_string_map().unwrap().keys().collect::<Vec<_>>(), ["a"]);
        assert_eq!(Value::Map(vec![(Value::Null, Value::Null)]).as_string_map(), None);
        assert_eq!(Value::Null.as_string_map(), None);
        assert_eq!(Value::Null.as_str(), None);
    }

    #[test]
    fn json_round_trip_keeps_order_and_number_kinds() {
        let text = r#"{"b":[1,2.5,null,true,"x"],"a":{}}"#;
        let json: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(Value::from_json(json).to_json().to_string(), text);
        let odd =
            Value::Map(vec![(Value::Number(Number::Int(1)), Value::Timestamp { date_only: true })]);
        assert_eq!(odd.to_json().to_string(), r#"{"1":null}"#);
    }

    #[test]
    fn json_converts_in_order() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"b": [1, 2.5, null, true, "x"], "a": {}}"#).unwrap();
        assert_eq!(
            Value::from_json(json),
            Value::Map(vec![
                (
                    Value::Str("b".into()),
                    Value::List(vec![
                        Value::Number(Number::Int(1)),
                        Value::Number(Number::Float(2.5)),
                        Value::Null,
                        Value::Bool(true),
                        Value::Str("x".into()),
                    ])
                ),
                (Value::Str("a".into()), Value::Map(vec![])),
            ])
        );
    }
}
