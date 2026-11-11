//! A script's options: its `param` statements, given as `--name value`
//! or `--name=value`, a `bool` also as `--name` alone, checked before the
//! script runs, and the other arguments in `$args`.

use super::{Flow, Shell, System};
use crate::ast::{Block, Expr, Literal, Param, ParamType, StatementKind};
use crate::parse::parse;
use crate::value::Value;

/// The literal `text` is, if it is one.
fn literal(text: &str) -> Option<Literal> {
    let block = parse(&format!("let x = {text}")).ok()?;
    match &block.statements.first()?.kind {
        StatementKind::Let {
            value: Expr::Literal(l),
            ..
        } => Some(l.clone()),
        StatementKind::Let {
            value: Expr::Unary(_, inner),
            ..
        } => match &**inner {
            Expr::Literal(Literal::Int(n)) => Some(Literal::Int(-n)),
            Expr::Literal(Literal::Float(f)) => Some(Literal::Float(-f)),
            _ => None,
        },
        _ => None,
    }
}

/// `text` as a value of type `kind`, or `None` when it does not parse as
/// one.
fn typed(kind: ParamType, text: &str) -> Option<Value> {
    let parsed = || literal(text).map(|l| Value::from(&l));
    match kind {
        ParamType::String | ParamType::Path => Some(Value::String(text.into())),
        ParamType::Bool => match text {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            _ => None,
        },
        ParamType::Int => parsed().filter(|v| matches!(v, Value::Int(_))),
        ParamType::Float => match parsed()? {
            Value::Int(n) => Some(Value::Float(n as f64)),
            v @ Value::Float(_) => Some(v),
            _ => None,
        },
        ParamType::Size => parsed().filter(|v| matches!(v, Value::Size(_))),
        ParamType::Duration => parsed().filter(|v| matches!(v, Value::Duration(_))),
        ParamType::Date => parsed().filter(|v| matches!(v, Value::Date(_))),
        ParamType::List => Some(Value::List(
            text.split(',')
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.into()))
                .collect(),
        )),
        ParamType::Any => Some(parsed().unwrap_or_else(|| Value::String(text.into()))),
    }
}

impl<S: System> Shell<S> {
    /// Sets the options of the script `block` from `args`, as its `param`
    /// statements declare them, and `$args` to the other arguments.
    pub fn options(&mut self, block: &Block, args: &[String]) -> Result<(), Flow> {
        let params: Vec<&Param> = block
            .statements
            .iter()
            .filter_map(|s| match &s.kind {
                StatementKind::Param(p) => Some(p),
                _ => None,
            })
            .collect();
        let mut given: Vec<(String, Value)> = Vec::new();
        let mut rest = Vec::new();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let Some(option) = arg.strip_prefix("--").filter(|o| !o.is_empty()) else {
                rest.push(Value::String(arg.clone()));
                continue;
            };
            let (name, inline) = match option.split_once('=') {
                Some((n, v)) => (n, Some(v.to_owned())),
                None => (option, None),
            };
            let param = params
                .iter()
                .find(|p| p.name == name)
                .ok_or_else(|| self.fail(format!("the script has no option `--{name}`")))?;
            let text = match (inline, param.kind) {
                (Some(v), _) => v,
                (None, ParamType::Bool) => "true".into(),
                (None, _) => args
                    .next()
                    .cloned()
                    .ok_or_else(|| self.fail(format!("the option `--{name}` needs a value")))?,
            };
            let value = typed(param.kind, &text).ok_or_else(|| {
                self.fail(
                    format!(
                        "the option `--{name}` takes a {:?}, not `{text}`",
                        param.kind
                    )
                    .to_lowercase(),
                )
            })?;
            given.push((name.to_owned(), value));
        }
        for param in params {
            let value = match given.iter().position(|(n, _)| *n == param.name) {
                Some(at) => given.remove(at).1,
                None => match &param.default {
                    Some(default) => self.eval(default)?,
                    None => {
                        return Err(self.fail(format!("the option `--{}` is missing", param.name)));
                    }
                },
            };
            self.set_global(&param.name, value);
        }
        self.set_global("args", Value::List(rest));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::eval::Flow;
    use crate::eval::tests::shell;
    use crate::parse::parse;
    use crate::value::{Value, scalar_text};

    fn options(script: &str, args: &[&str]) -> Result<Vec<String>, String> {
        let mut s = shell();
        let block = parse(script).unwrap();
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        match s.options(&block, &args) {
            Ok(()) => {}
            Err(Flow::Error(f)) => return Err(f.message),
            Err(other) => panic!("{other:?}"),
        }
        Ok(["count", "big", "fast", "args"]
            .iter()
            .map(|n| s.lookup(n).map_or_else(|| "-".into(), scalar_text))
            .collect())
    }

    const SCRIPT: &str = "param --count: int\nparam --big: size = 1mb\nparam --fast: bool = false";

    #[test]
    fn sets_options_and_arguments() {
        assert_eq!(
            options(SCRIPT, &["--count", "3", "a", "--fast", "b"]).unwrap(),
            ["3", "1mb", "true", "[\"a\",\"b\"]"]
        );
        assert_eq!(
            options(SCRIPT, &["--count=-2", "--big=2kib"]).unwrap(),
            ["-2", "2kib", "false", "[]"]
        );
    }

    #[test]
    fn rejects_missing_unknown_and_bad_options() {
        assert_eq!(
            options(SCRIPT, &[]).unwrap_err(),
            "the option `--count` is missing"
        );
        assert_eq!(
            options(SCRIPT, &["--count", "1", "--x", "1"]).unwrap_err(),
            "the script has no option `--x`"
        );
        assert_eq!(
            options(SCRIPT, &["--count", "many"]).unwrap_err(),
            "the option `--count` takes a int, not `many`"
        );
        assert!(
            options(SCRIPT, &["--count", "1", "--big", "3s"])
                .unwrap_err()
                .contains("--big")
        );
        assert_eq!(Value::Null, Value::Null);
    }
}
