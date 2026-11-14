//! The built-in commands that turn program output into values and work
//! on values: `lines`, `words`, `split`, `table`, `parse`, `match`,
//! `replace`, `trim`, `upper`, `lower`, `len`, `contains`, `where`,
//! `each`, `sort-by`, `group-by`, `first`, `skip`, `uniq`, `get`,
//! `from json` and `to json`. In their blocks, `$it` is the current
//! item.

use super::builtins::Arg;
use super::{Flow, Shell, System};
use crate::ast::Block;
use crate::value::{self, Value, order, read, regex, scalar_text};
use std::cmp::Ordering;

pub const COMMANDS: [&str; 22] = [
    "lines", "words", "split", "table", "parse", "match", "replace", "trim", "upper", "lower",
    "len", "contains", "where", "each", "sort-by", "group-by", "first", "skip", "uniq", "get",
    "from", "to",
];

/// A command's arguments: values and blocks in order.
struct Args {
    name: &'static str,
    args: Vec<Arg>,
}

impl Args {
    fn value(&self, i: usize) -> Option<&Value> {
        match self.args.get(i) {
            Some(Arg::Value(v)) => Some(v),
            _ => None,
        }
    }

    fn text(&self, i: usize) -> Option<String> {
        self.value(i).map(scalar_text)
    }

    fn block(&self, i: usize) -> Option<&Block> {
        match self.args.get(i) {
            Some(Arg::Block(b)) => Some(b),
            _ => None,
        }
    }
}

impl<S: System> Shell<S> {
    /// The value of `block` with `$it` set to `item`.
    fn with_it(&mut self, block: &Block, item: Value) -> Result<Value, Flow> {
        self.scoped(false, |s| {
            s.define("it", item);
            s.block(block)
        })
    }

    fn need<T>(&self, got: Option<T>, what: &str) -> Result<T, Flow> {
        got.ok_or_else(|| self.fail(what.into()))
    }
}

/// The text of `input`: a string, or a program's output.
fn input_text<S: System>(
    shell: &Shell<S>,
    name: &str,
    input: Option<Value>,
) -> Result<String, Flow> {
    match input {
        Some(Value::String(text)) => Ok(text),
        Some(other) => Err(shell.fail(format!("`{name}` takes text, not a {}", other.kind()))),
        None => Err(shell.fail(format!("`{name}` takes its text from a pipe"))),
    }
}

/// The items of `input`: a list, or the lines of text.
fn input_items<S: System>(
    shell: &Shell<S>,
    name: &str,
    input: Option<Value>,
) -> Result<Vec<Value>, Flow> {
    match input {
        Some(Value::List(items)) => Ok(items),
        Some(Value::String(text)) => Ok(lines(&text)),
        Some(other) => Err(shell.fail(format!("`{name}` takes a list, not a {}", other.kind()))),
        None => Err(shell.fail(format!("`{name}` takes its list from a pipe"))),
    }
}

/// The items of `input` as `input_items` gives them, with a single
/// record as a list that holds it.
fn list_or_record<S: System>(
    shell: &Shell<S>,
    name: &str,
    input: Option<Value>,
) -> Result<Vec<Value>, Flow> {
    match input {
        Some(record @ Value::Record(_)) => Ok(vec![record]),
        other => input_items(shell, name, other),
    }
}

fn lines(text: &str) -> Vec<Value> {
    text.lines().map(|l| Value::String(l.into())).collect()
}

/// `f` applied to a string, or to each string of a list.
fn map_strings<S: System>(
    shell: &Shell<S>,
    name: &str,
    input: Option<Value>,
    f: impl Fn(&str) -> Value,
) -> Result<Value, Flow> {
    match input {
        Some(Value::String(text)) => Ok(f(&text)),
        Some(Value::List(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => Ok(f(text)),
                other => Err(shell.fail(format!("`{name}` takes strings, not a {}", other.kind()))),
            })
            .collect::<Result<_, _>>()
            .map(Value::List),
        Some(other) => Err(shell.fail(format!("`{name}` takes a string, not a {}", other.kind()))),
        None => Err(shell.fail(format!("`{name}` takes its string from a pipe"))),
    }
}

/// Runs text or value command `name`.
pub fn run<S: System>(
    shell: &mut Shell<S>,
    name: &'static str,
    args: Vec<Arg>,
    input: Option<Value>,
) -> Result<Value, Flow> {
    let a = Args { name, args };
    match name {
        "lines" => Ok(Value::List(lines(&input_text(shell, name, input)?))),
        "words" => {
            let text = input_text(shell, name, input)?;
            Ok(Value::List(
                text.split_whitespace()
                    .map(|w| Value::String(w.into()))
                    .collect(),
            ))
        }
        "split" => {
            let separator = shell.need(a.text(0), "`split` takes a separator")?;
            let text = input_text(shell, name, input)?;
            Ok(Value::List(
                text.split(separator.as_str())
                    .map(|w| Value::String(w.into()))
                    .collect(),
            ))
        }
        "table" => table(shell, &input_text(shell, name, input)?),
        "parse" => {
            let pattern = shell.need(a.text(0), "`parse` takes a pattern such as \"{a} {b}\"")?;
            let re = parse_pattern(shell, &pattern)?;
            let mut rows = Vec::new();
            for line in input_items(shell, name, input)? {
                let line = scalar_text(&line);
                if let Some(caps) = re.captures(&line) {
                    let fields = re
                        .capture_names()
                        .flatten()
                        .map(|n| (n.to_owned(), Value::String(caps[n].to_owned())))
                        .collect();
                    rows.push(Value::Record(fields));
                }
            }
            Ok(Value::List(rows))
        }
        "match" => {
            let pattern = shell.need(a.text(0), "`match` takes a regular expression")?;
            let re = regex(&pattern).map_err(|e| shell.fail(e))?;
            let mut rows = Vec::new();
            for line in input_items(shell, name, input)? {
                let line = scalar_text(&line);
                let Some(caps) = re.captures(&line) else {
                    continue;
                };
                let mut fields = vec![("line".to_owned(), Value::String(line.clone()))];
                for (i, group) in re.capture_names().enumerate().skip(1) {
                    let key = group.map_or_else(|| i.to_string(), str::to_owned);
                    let value = caps
                        .get(i)
                        .map_or(Value::Null, |m| Value::String(m.as_str().into()));
                    fields.push((key, value));
                }
                rows.push(Value::Record(fields));
            }
            Ok(Value::List(rows))
        }
        "replace" => {
            let (pattern, with) = match (a.text(0), a.text(1)) {
                (Some(p), Some(w)) => (p, w),
                _ => {
                    return Err(shell
                        .fail("`replace` takes a regular expression and its replacement".into()));
                }
            };
            let re = regex(&pattern).map_err(|e| shell.fail(e))?;
            map_strings(shell, name, input, |s| {
                Value::String(re.replace_all(s, with.as_str()).into_owned())
            })
        }
        "trim" => map_strings(shell, name, input, |s| Value::String(s.trim().into())),
        "upper" => map_strings(shell, name, input, |s| Value::String(s.to_uppercase())),
        "lower" => map_strings(shell, name, input, |s| Value::String(s.to_lowercase())),
        "len" => match input {
            Some(Value::String(s)) => Ok(Value::Int(s.chars().count() as i64)),
            Some(Value::List(l)) => Ok(Value::Int(l.len() as i64)),
            Some(Value::Record(r)) => Ok(Value::Int(r.len() as i64)),
            Some(other) => Err(shell.fail(format!(
                "`len` takes a string, list or record, not a {}",
                other.kind()
            ))),
            None => Err(shell.fail("`len` takes its value from a pipe".into())),
        },
        "contains" => {
            let needle = shell.need(a.value(0).cloned(), "`contains` takes what to look for")?;
            match input {
                Some(Value::String(s)) => Ok(Value::Bool(s.contains(&scalar_text(&needle)))),
                Some(Value::List(l)) => Ok(Value::Bool(l.iter().any(|v| value::equal(v, &needle)))),
                Some(other) => Err(shell.fail(format!(
                    "`contains` takes a string or list, not a {}",
                    other.kind()
                ))),
                None => Err(shell.fail("`contains` takes its value from a pipe".into())),
            }
        }
        "where" => {
            let block = shell.need(a.block(0).cloned(), "`where` takes a block")?;
            let mut kept = Vec::new();
            for item in list_or_record(shell, name, input)? {
                let keep = shell.with_it(&block, item.clone())?;
                if keep.truth().map_err(|e| shell.fail(e))? {
                    kept.push(item);
                }
            }
            Ok(Value::List(kept))
        }
        "each" => {
            let block = shell.need(a.block(0).cloned(), "`each` takes a block")?;
            let mut mapped = Vec::new();
            for item in list_or_record(shell, name, input)? {
                mapped.push(shell.with_it(&block, item)?);
            }
            Ok(Value::List(mapped))
        }
        "sort-by" | "group-by" => {
            let items = list_or_record(shell, name, input)?;
            let mut keyed = Vec::new();
            for item in items {
                let key = match (a.block(0).cloned(), a.text(0)) {
                    (Some(block), _) => shell.with_it(&block, item.clone())?,
                    (None, Some(field)) => {
                        item.field(&field).cloned().map_err(|e| shell.fail(e))?
                    }
                    (None, None) if name == "sort-by" => item.clone(),
                    (None, None) => {
                        return Err(shell.fail("`group-by` takes a field or a block".into()));
                    }
                };
                keyed.push((key, item));
            }
            if name == "sort-by" {
                let mut failed = false;
                keyed.sort_by(|(x, _), (y, _)| {
                    order(x, y).unwrap_or_else(|| {
                        failed = true;
                        Ordering::Equal
                    })
                });
                if failed {
                    return Err(
                        shell.fail("`sort-by` found keys of kinds that do not compare".into())
                    );
                }
                return Ok(Value::List(keyed.into_iter().map(|(_, v)| v).collect()));
            }
            let mut groups: Vec<(String, Value)> = Vec::new();
            for (key, item) in keyed {
                let key = scalar_text(&key);
                match groups.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, Value::List(items))) => items.push(item),
                    _ => groups.push((key, Value::List(vec![item]))),
                }
            }
            Ok(Value::Record(groups))
        }
        "first" | "skip" => {
            let items = list_or_record(shell, name, input)?;
            let count = match a.text(0) {
                None => None,
                Some(n) => Some(
                    n.parse::<usize>()
                        .map_err(|_| shell.fail(format!("`{name}` takes a count, not `{n}`")))?,
                ),
            };
            Ok(match (name, count) {
                ("first", None) => items.into_iter().next().unwrap_or(Value::Null),
                ("first", Some(n)) => Value::List(items.into_iter().take(n).collect()),
                (_, n) => Value::List(items.into_iter().skip(n.unwrap_or(1)).collect()),
            })
        }
        "uniq" => {
            let mut kept: Vec<Value> = Vec::new();
            for item in list_or_record(shell, name, input)? {
                if !kept.iter().any(|k| value::equal(k, &item)) {
                    kept.push(item);
                }
            }
            Ok(Value::List(kept))
        }
        "get" => {
            let path = shell.need(a.text(0), "`get` takes a field")?;
            let get = |v: &Value| -> Result<Value, String> {
                let mut v = v.clone();
                for part in path.split('.') {
                    v = match part.parse::<usize>() {
                        Ok(i) => v.item(i)?.clone(),
                        Err(_) => v.field(part)?.clone(),
                    };
                }
                Ok(v)
            };
            match input {
                Some(Value::List(items)) if items.iter().all(|i| matches!(i, Value::Record(_))) => {
                    items
                        .iter()
                        .map(|i| get(i).map_err(|e| shell.fail(e)))
                        .collect::<Result<_, _>>()
                        .map(Value::List)
                }
                Some(v) => get(&v).map_err(|e| shell.fail(e)),
                None => Err(shell.fail("`get` takes its value from a pipe".into())),
            }
        }
        "from" | "to" => match a.text(0).as_deref() {
            Some("json") if name == "from" => {
                read::json(&input_text(shell, "from json", input)?).map_err(|e| shell.fail(e))
            }
            Some("json") => Ok(Value::String(value::json(&input.unwrap_or(Value::Null)))),
            _ => Err(shell.fail(format!("`{name}` knows only `json`"))),
        },
        _ => Err(shell.fail(format!("`{}` is not a command of its own", a.name))),
    }
}

/// A regular expression for `parse`'s pattern: its text literal and its
/// `{name}` parts named groups, the last of which takes the rest of the
/// line.
fn parse_pattern<S: System>(shell: &Shell<S>, pattern: &str) -> Result<regex::Regex, Flow> {
    let mut re = String::from("^");
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        re.push_str(&regex::escape(&rest[..open]));
        let close = rest[open..]
            .find('}')
            .ok_or_else(|| shell.fail("`parse`'s pattern has a `{` without `}`".into()))?;
        let name = &rest[open + 1..open + close];
        rest = &rest[open + close + 1..];
        let lazy = if rest.is_empty() { "" } else { "?" };
        re.push_str(&format!("(?P<{name}>.*{lazy})"));
    }
    re.push_str(&regex::escape(rest));
    re.push('$');
    regex(&re).map_err(|e| shell.fail(e))
}

/// The records of column-aligned `text` whose first line names the
/// columns: each column starts where its name does.
fn table<S: System>(shell: &Shell<S>, text: &str) -> Result<Value, Flow> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header = lines
        .next()
        .ok_or_else(|| shell.fail("`table` takes text with a header row".into()))?;
    let mut columns: Vec<(usize, String)> = Vec::new();
    let mut in_word = false;
    for (i, c) in header.char_indices() {
        match (c.is_whitespace(), in_word) {
            (false, false) => {
                columns.push((i, c.to_string()));
                in_word = true;
            }
            (false, true) => columns.last_mut().expect("a column").1.push(c),
            (true, _) => in_word = false,
        }
    }
    let rows = lines
        .map(|line| {
            let fields = columns
                .iter()
                .enumerate()
                .map(|(n, (start, key))| {
                    let end = columns.get(n + 1).map_or(line.len(), |(s, _)| *s);
                    let cell = line
                        .get((*start).min(line.len())..end.min(line.len()))
                        .unwrap_or("");
                    (key.clone(), Value::String(cell.trim().into()))
                })
                .collect();
            Value::Record(fields)
        })
        .collect();
    Ok(Value::List(rows))
}

#[cfg(test)]
mod tests;
