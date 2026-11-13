//! Whether a value matches a pattern of `match`, and what it binds.

use crate::ast::Pattern;
use crate::value::{Value, equal, order};
use std::cmp::Ordering;

/// Whether `value` matches `pattern`, with the variables it binds added
/// to `bound`. When it does not match, `bound` may hold some of them.
pub fn matches(pattern: &Pattern, value: &Value, bound: &mut Vec<(String, Value)>) -> bool {
    match pattern {
        Pattern::Any => true,
        Pattern::Bind(name) => {
            bound.push((name.clone(), value.clone()));
            true
        }
        Pattern::Literal(literal) => equal(&Value::from(literal), value),
        Pattern::Range(low, high) => {
            let (low, high) = (Value::from(low), Value::from(high));
            let ranged = matches!(
                value,
                Value::Int(_)
                    | Value::Float(_)
                    | Value::Size(_)
                    | Value::Duration(_)
                    | Value::Date(_)
            );
            ranged
                && order(&low, value).is_some_and(|o| o != Ordering::Greater)
                && order(value, &high).is_some_and(|o| o != Ordering::Greater)
        }
        Pattern::List { items, rest } => {
            let Value::List(values) = value else {
                return false;
            };
            let fits = match rest {
                Some(_) => values.len() >= items.len(),
                None => values.len() == items.len(),
            };
            if !fits || !items.iter().zip(values).all(|(p, v)| matches(p, v, bound)) {
                return false;
            }
            if let Some(rest) = rest {
                bound.push((rest.clone(), Value::List(values[items.len()..].to_vec())));
            }
            true
        }
        Pattern::Record(fields) => {
            let Value::Record(_) = value else {
                return false;
            };
            fields
                .iter()
                .all(|(key, p)| value.field(key).is_ok_and(|v| matches(p, v, bound)))
        }
        Pattern::Either(patterns) => patterns.iter().any(|p| {
            let mut tried = Vec::new();
            let ok = matches(p, value, &mut tried);
            if ok {
                bound.extend(tried);
            }
            ok
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Literal, StatementKind};
    use crate::parse::parse;

    /// The pattern of the only arm of `match $v { <pattern> => {} }`.
    fn pattern(source: &str) -> Pattern {
        let block = parse(&format!("match $v {{ {source} => {{ }} }}")).unwrap();
        match &block.statements[0].kind {
            StatementKind::Match { arms, .. } => arms[0].pattern.clone(),
            other => panic!("{other:?}"),
        }
    }

    fn check(source: &str, value: Value) -> Option<Vec<(String, Value)>> {
        let mut bound = Vec::new();
        matches(&pattern(source), &value, &mut bound).then_some(bound)
    }

    #[test]
    fn matches_literals_ranges_and_alternatives() {
        assert!(check("3", Value::Int(3)).is_some());
        assert!(check("3", Value::Float(3.0)).is_some());
        assert!(check("'a'", Value::String("b".into())).is_none());
        assert!(check("1..5", Value::Int(5)).is_some());
        assert!(check("1..5", Value::Int(6)).is_none());
        assert!(check("1kb..1mb", Value::Size(4096)).is_some());
        assert!(check("1..5", Value::String("3".into())).is_none());
        assert!(check("0 | 1", Value::Int(1)).is_some());
        assert!(check("_", Value::Null).is_some());
        assert_eq!(Value::from(&Literal::Null), Value::Null);
    }

    #[test]
    fn binds_names_lists_and_records() {
        let list = Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let bound = check("[first ...rest]", list.clone()).unwrap();
        assert_eq!(bound[0], ("first".into(), Value::Int(1)));
        assert_eq!(
            bound[1],
            (
                "rest".into(),
                Value::List(vec![Value::Int(2), Value::Int(3)])
            )
        );
        assert!(check("[a b]", list).is_none());
        let record = Value::Record(vec![
            ("name".into(), Value::String("x".into())),
            ("size".into(), Value::Int(3)),
        ]);
        let bound = check("{name: n}", record.clone()).unwrap();
        assert_eq!(bound, [("n".into(), Value::String("x".into()))]);
        assert!(check("{kind: k}", record).is_none());
        assert!(check("{name: n}", Value::Int(1)).is_none());
    }
}
