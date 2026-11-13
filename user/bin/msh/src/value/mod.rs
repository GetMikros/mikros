//! msh's values: their kinds, the operators on them by kind, how they
//! compare, and the text they are written as.

pub mod read;

use crate::ast::{Binary, Date, Literal, Unary};
use std::cmp::Ordering;
use std::fmt::Write;

/// A value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// In bytes.
    Size(i64),
    /// In nanoseconds.
    Duration(i64),
    Date(Date),
    String(String),
    List(Vec<Value>),
    /// Fields in the order they were written.
    Record(Vec<(String, Value)>),
}

impl From<&Literal> for Value {
    fn from(literal: &Literal) -> Value {
        match literal {
            Literal::Int(n) => Value::Int(*n),
            Literal::Float(f) => Value::Float(*f),
            Literal::Size(n) => Value::Size(*n),
            Literal::Duration(n) => Value::Duration(*n),
            Literal::Date(d) => Value::Date(*d),
            Literal::String(s) => Value::String(s.clone()),
            Literal::Bool(b) => Value::Bool(*b),
            Literal::Null => Value::Null,
        }
    }
}

impl Value {
    /// The kind's name, for messages.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Size(_) => "size",
            Value::Duration(_) => "duration",
            Value::Date(_) => "date",
            Value::String(_) => "string",
            Value::List(_) => "list",
            Value::Record(_) => "record",
        }
    }

    /// The field `name` of a record.
    pub fn field(&self, name: &str) -> Result<&Value, String> {
        match self {
            Value::Record(fields) => fields
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v)
                .ok_or_else(|| format!("the record has no field `{name}`")),
            other => Err(format!("a {} has no fields", other.kind())),
        }
    }

    /// Item `index` of a list.
    pub fn item(&self, index: usize) -> Result<&Value, String> {
        match self {
            Value::List(items) => items
                .get(index)
                .ok_or_else(|| format!("the list has no item {index}")),
            other => Err(format!("a {} has no items", other.kind())),
        }
    }

    /// The value of the boolean `self`, for conditions, `and`, `or` and
    /// `not`.
    pub fn truth(&self) -> Result<bool, String> {
        match self {
            Value::Bool(b) => Ok(*b),
            other => Err(format!(
                "a condition must be a bool, not a {}",
                other.kind()
            )),
        }
    }
}

fn mismatch(op: &str, a: &Value, b: &Value) -> String {
    format!("`{op}` does not take a {} and a {}", a.kind(), b.kind())
}

fn overflow() -> String {
    "the result is too large".into()
}

/// `op value`.
pub fn unary(op: Unary, value: &Value) -> Result<Value, String> {
    match (op, value) {
        (Unary::Not, v) => Ok(Value::Bool(!v.truth()?)),
        (Unary::Negate, Value::Int(n)) => n.checked_neg().map(Value::Int).ok_or_else(overflow),
        (Unary::Negate, Value::Float(f)) => Ok(Value::Float(-f)),
        (Unary::Negate, Value::Size(n)) => n.checked_neg().map(Value::Size).ok_or_else(overflow),
        (Unary::Negate, Value::Duration(n)) => {
            n.checked_neg().map(Value::Duration).ok_or_else(overflow)
        }
        (Unary::Negate, v) => Err(format!("`-` does not take a {}", v.kind())),
    }
}

/// A number as a float, for arithmetic that mixes ints and floats.
fn float(v: &Value) -> Option<f64> {
    match v {
        Value::Int(n) => Some(*n as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

/// `n` times or divided by the number `by`, for sizes and durations.
fn scale(n: i64, by: &Value, divide: bool) -> Result<i64, String> {
    match by {
        Value::Int(m) if divide => {
            if *m == 0 {
                return Err("division by zero".into());
            }
            n.checked_div(*m).ok_or_else(overflow)
        }
        Value::Int(m) => n.checked_mul(*m).ok_or_else(overflow),
        Value::Float(f) => {
            if divide && *f == 0.0 {
                return Err("division by zero".into());
            }
            let r = if divide { n as f64 / f } else { n as f64 * f };
            if r.is_finite() && r.abs() < 9.2e18 {
                Ok(r.round() as i64)
            } else {
                Err(overflow())
            }
        }
        _ => Err(String::new()),
    }
}

/// The instant of a date, in nanoseconds since 1970-01-01T00:00:00Z,
/// with a date in the system time zone taken as UTC: its offset is
/// applied when the date is shown.
fn instant(d: &Date) -> i128 {
    i128::from(d.seconds) * 1_000_000_000 + i128::from(d.nanos)
}

fn date_at(nanos: i128, offset: Option<i32>) -> Result<Date, String> {
    let seconds = i64::try_from(nanos.div_euclid(1_000_000_000)).map_err(|_| overflow())?;
    Ok(Date {
        seconds,
        nanos: nanos.rem_euclid(1_000_000_000) as u32,
        offset,
    })
}

/// `a op b` for the arithmetic, comparison and logical operators.
/// `and`, `or` and `??` evaluate their right side only when needed, so
/// the evaluator handles them; here they take both values.
pub fn binary(op: Binary, a: &Value, b: &Value) -> Result<Value, String> {
    use Value::*;
    let name = symbol(op);
    let int = |r: Option<i64>| r.map(Int).ok_or_else(overflow);
    Ok(match op {
        Binary::Add => match (a, b) {
            (Int(x), Int(y)) => int(x.checked_add(*y))?,
            (Size(x), Size(y)) => Size(x.checked_add(*y).ok_or_else(overflow)?),
            (Duration(x), Duration(y)) => Duration(x.checked_add(*y).ok_or_else(overflow)?),
            (Date(d), Duration(n)) | (Duration(n), Date(d)) => {
                Date(date_at(instant(d) + i128::from(*n), d.offset)?)
            }
            (String(x), String(y)) => String(format!("{x}{y}")),
            (List(x), List(y)) => List(x.iter().chain(y).cloned().collect()),
            _ => match (float(a), float(b)) {
                (Some(x), Some(y)) => Float(x + y),
                _ => return Err(mismatch(name, a, b)),
            },
        },
        Binary::Subtract => match (a, b) {
            (Int(x), Int(y)) => int(x.checked_sub(*y))?,
            (Size(x), Size(y)) => Size(x.checked_sub(*y).ok_or_else(overflow)?),
            (Duration(x), Duration(y)) => Duration(x.checked_sub(*y).ok_or_else(overflow)?),
            (Date(d), Duration(n)) => Date(date_at(instant(d) - i128::from(*n), d.offset)?),
            (Date(x), Date(y)) => {
                Duration(i64::try_from(instant(x) - instant(y)).map_err(|_| overflow())?)
            }
            _ => match (float(a), float(b)) {
                (Some(x), Some(y)) => Float(x - y),
                _ => return Err(mismatch(name, a, b)),
            },
        },
        Binary::Multiply | Binary::Divide => {
            let divide = op == Binary::Divide;
            match (a, b) {
                (Int(x), Int(y)) if divide => {
                    if *y == 0 {
                        return Err("division by zero".into());
                    }
                    int(x.checked_div(*y))?
                }
                (Int(x), Int(y)) => int(x.checked_mul(*y))?,
                (Size(n), by) if float(by).is_some() => Size(scale(*n, by, divide)?),
                (Duration(n), by) if float(by).is_some() => Duration(scale(*n, by, divide)?),
                (by, Size(n)) if !divide && float(by).is_some() => Size(scale(*n, by, false)?),
                (by, Duration(n)) if !divide && float(by).is_some() => {
                    Duration(scale(*n, by, false)?)
                }
                _ => match (float(a), float(b)) {
                    (Some(x), Some(y)) if divide => Float(x / y),
                    (Some(x), Some(y)) => Float(x * y),
                    _ => return Err(mismatch(name, a, b)),
                },
            }
        }
        Binary::Remainder => match (a, b) {
            (Int(_), Int(0)) => return Err("division by zero".into()),
            (Int(x), Int(y)) => int(x.checked_rem(*y))?,
            _ => match (float(a), float(b)) {
                (Some(x), Some(y)) => Float(x % y),
                _ => return Err(mismatch(name, a, b)),
            },
        },
        Binary::Equal => Bool(equal(a, b)),
        Binary::NotEqual => Bool(!equal(a, b)),
        Binary::Less | Binary::LessEqual | Binary::Greater | Binary::GreaterEqual => {
            let ordering = order(a, b).ok_or_else(|| mismatch(name, a, b))?;
            Bool(match op {
                Binary::Less => ordering == Ordering::Less,
                Binary::LessEqual => ordering != Ordering::Greater,
                Binary::Greater => ordering == Ordering::Greater,
                _ => ordering != Ordering::Less,
            })
        }
        Binary::And => Bool(a.truth()? && b.truth()?),
        Binary::Or => Bool(a.truth()? || b.truth()?),
        Binary::Default => match a {
            Null => b.clone(),
            _ => a.clone(),
        },
        Binary::Matches | Binary::NotMatches => match (a, b) {
            (String(text), String(pattern)) => {
                let matched = regex(pattern)?.is_match(text);
                Bool(matched == (op == Binary::Matches))
            }
            _ => return Err(mismatch(name, a, b)),
        },
    })
}

/// The regular expression `pattern`, or why it is not one.
pub fn regex(pattern: &str) -> Result<regex::Regex, String> {
    regex::Regex::new(pattern).map_err(|e| format!("`{pattern}` is not a regular expression: {e}"))
}

/// The operator's symbol, for messages.
pub fn symbol(op: Binary) -> &'static str {
    match op {
        Binary::Multiply => "*",
        Binary::Divide => "/",
        Binary::Remainder => "%",
        Binary::Add => "+",
        Binary::Subtract => "-",
        Binary::Default => "??",
        Binary::Equal => "==",
        Binary::NotEqual => "!=",
        Binary::Less => "<",
        Binary::LessEqual => "<=",
        Binary::Greater => ">",
        Binary::GreaterEqual => ">=",
        Binary::Matches => "=~",
        Binary::NotMatches => "!~",
        Binary::And => "and",
        Binary::Or => "or",
    }
}

/// Whether `a` and `b` are equal: of the same kind and value, with ints
/// and floats compared as numbers and dates as instants.
pub fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Date(x), Value::Date(y)) => instant(x) == instant(y),
        (Value::List(x), Value::List(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| equal(a, b))
        }
        (Value::Record(x), Value::Record(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.iter().any(|(k2, v2)| k == k2 && equal(v, v2)))
        }
        _ => match (float(a), float(b)) {
            (Some(x), Some(y)) => x == y,
            _ => a == b,
        },
    }
}

/// How `a` compares to `b`, for values of a kind that is ordered:
/// numbers, sizes, durations, dates and strings.
pub fn order(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Size(x), Value::Size(y)) | (Value::Duration(x), Value::Duration(y)) => {
            Some(x.cmp(y))
        }
        (Value::Date(x), Value::Date(y)) => Some(instant(x).cmp(&instant(y))),
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        _ => float(a)?.partial_cmp(&float(b)?),
    }
}

/// The units a size or duration is written with, from the largest.
const SIZE_UNITS: [(&str, i64); 11] = [
    ("pb", 1_000_000_000_000_000),
    ("tb", 1_000_000_000_000),
    ("gb", 1_000_000_000),
    ("mb", 1_000_000),
    ("kb", 1_000),
    ("pib", 1 << 50),
    ("tib", 1 << 40),
    ("gib", 1 << 30),
    ("mib", 1 << 20),
    ("kib", 1 << 10),
    ("b", 1),
];
const DURATION_UNITS: [(&str, i64); 8] = [
    ("wk", 604_800_000_000_000),
    ("day", 86_400_000_000_000),
    ("h", 3_600_000_000_000),
    ("min", 60_000_000_000),
    ("s", 1_000_000_000),
    ("ms", 1_000_000),
    ("us", 1_000),
    ("ns", 1),
];

/// `n` in the largest of `units` that divides it exactly.
fn with_unit(n: i64, units: &[(&str, i64)]) -> String {
    if n == 0 {
        return format!("0{}", units[units.len() - 1].0);
    }
    let (unit, per) = units
        .iter()
        .find(|(_, per)| n % per == 0)
        .expect("the smallest unit divides every value");
    format!("{}{unit}", n / per)
}

/// A float as a literal that reads back as a float.
fn float_literal(f: f64) -> String {
    let text = format!("{f:?}");
    match text.as_str() {
        "inf" | "-inf" | "NaN" => text,
        _ if text.contains(['.', 'e', 'E']) => text,
        _ => format!("{text}.0"),
    }
}

/// A date in ISO 8601: the date alone at midnight in the system time
/// zone, and otherwise with its time, and `Z` or its offset when it has
/// one.
fn date_literal(d: &Date) -> String {
    let local = d.seconds + i64::from(d.offset.unwrap_or(0));
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (y, m, day) = civil_from_days(days);
    let mut text = format!("{y:04}-{m:02}-{day:02}");
    if secs == 0 && d.nanos == 0 && d.offset.is_none() {
        return text;
    }
    let _ = write!(
        text,
        "T{:02}:{:02}:{:02}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    );
    if d.nanos != 0 {
        let fraction = format!("{:09}", d.nanos);
        let _ = write!(text, ".{}", fraction.trim_end_matches('0'));
    }
    match d.offset {
        None => {}
        Some(0) => text.push('Z'),
        Some(o) => {
            let sign = if o < 0 { '-' } else { '+' };
            let o = o.abs();
            let _ = write!(text, "{sign}{:02}:{:02}", o / 3600, o / 60 % 60);
        }
    }
    text
}

/// The date of `days` since 1970-01-01, the inverse of
/// `days_from_civil`.
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The text a scalar is written as: a string as it is, and any other
/// kind as its literal. Lists and records are written as JSON.
pub fn scalar_text(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => float_literal(*f),
        Value::Size(n) => with_unit(*n, &SIZE_UNITS),
        Value::Duration(n) => with_unit(*n, &DURATION_UNITS),
        Value::Date(d) => date_literal(d),
        Value::String(s) => s.clone(),
        Value::List(_) | Value::Record(_) => json(value),
    }
}

/// `value` as JSON: strings, numbers, booleans and null as themselves,
/// sizes and durations as their bytes and nanoseconds, dates as ISO 8601
/// strings, lists as arrays and records as objects.
pub fn json(value: &Value) -> String {
    let mut out = String::new();
    write_json(&mut out, value);
    out
}

fn write_json(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(n) | Value::Size(n) | Value::Duration(n) => {
            let _ = write!(out, "{n}");
        }
        Value::Float(f) if f.is_finite() => out.push_str(&float_literal(*f)),
        Value::Float(_) => out.push_str("null"),
        Value::Date(d) => json_string(out, &date_literal(d)),
        Value::String(s) => json_string(out, s),
        Value::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(out, item);
            }
            out.push(']');
        }
        Value::Record(fields) => {
            out.push('{');
            for (i, (key, item)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_string(out, key);
                out.push(':');
                write_json(out, item);
            }
            out.push('}');
        }
    }
}

fn json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Whether `value` is a record, or a non-empty list of records only.
fn records(value: &Value) -> Option<Vec<&[(String, Value)]>> {
    match value {
        Value::Record(fields) => Some(vec![fields]),
        Value::List(items) if !items.is_empty() => items
            .iter()
            .map(|item| match item {
                Value::Record(fields) => Some(fields.as_slice()),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

/// The text `value` is written as to a program: a list one item per
/// line, a record or a list of records one JSON object per line, and a
/// scalar as `scalar_text` says, each line ended by a newline, which a
/// string may already end with.
pub fn program_text(value: &Value) -> String {
    let mut out = String::new();
    match value {
        Value::List(items) if records(value).is_none() => {
            for item in items {
                out.push_str(&scalar_text(item));
                out.push('\n');
            }
        }
        _ => match records(value) {
            Some(rows) => {
                for row in rows {
                    let mut line = String::new();
                    write_json(&mut line, &Value::Record(row.to_vec()));
                    out.push_str(&line);
                    out.push('\n');
                }
            }
            None => {
                out.push_str(&scalar_text(value));
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
        },
    }
    out
}

/// The text `value` is shown as at the prompt: a record or a list of
/// records as a table with a header row and one column per field, and
/// any other value as to a program.
pub fn prompt_text(value: &Value) -> String {
    let Some(rows) = records(value) else {
        return program_text(value);
    };
    let mut columns: Vec<&str> = Vec::new();
    for row in &rows {
        for (key, _) in row.iter() {
            if !columns.contains(&key.as_str()) {
                columns.push(key);
            }
        }
    }
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            columns
                .iter()
                .map(|c| {
                    row.iter()
                        .find(|(k, _)| k == c)
                        .map(|(_, v)| scalar_text(v).replace('\n', " "))
                        .unwrap_or_default()
                })
                .collect()
        })
        .collect();
    let widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            cells
                .iter()
                .map(|r| r[i].chars().count())
                .chain([c.chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |items: Vec<&str>| {
        let mut text = String::new();
        for (i, item) in items.iter().enumerate() {
            if i + 1 == items.len() {
                text.push_str(item);
            } else {
                let _ = write!(text, "{item:<width$}  ", width = widths[i]);
            }
        }
        text.trim_end().to_owned() + "\n"
    };
    let mut out = line(columns.clone());
    for row in &cells {
        out.push_str(&line(row.iter().map(String::as_str).collect()));
    }
    out
}

#[cfg(test)]
mod tests;
