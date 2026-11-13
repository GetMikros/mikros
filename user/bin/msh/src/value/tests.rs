use super::*;
use crate::ast::{Binary as B, Unary as U};
use crate::parse::days_from_civil;

fn date(y: i64, m: i64, d: i64, offset: Option<i32>) -> Value {
    Value::Date(Date {
        seconds: days_from_civil(y, m, d) * 86_400 - i64::from(offset.unwrap_or(0)),
        nanos: 0,
        offset,
    })
}

fn ok(op: B, a: Value, b: Value) -> Value {
    binary(op, &a, &b).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn computes_numbers_by_kind() {
    use Value::*;
    assert_eq!(ok(B::Add, Int(2), Int(3)), Int(5));
    assert_eq!(ok(B::Add, Int(2), Float(0.5)), Float(2.5));
    assert_eq!(ok(B::Divide, Int(7), Int(2)), Int(3));
    assert_eq!(ok(B::Remainder, Int(7), Int(2)), Int(1));
    assert_eq!(
        binary(B::Divide, &Int(1), &Int(0)),
        Err("division by zero".into())
    );
    assert!(binary(B::Add, &Int(i64::MAX), &Int(1)).is_err());
    assert_eq!(unary(U::Negate, &Int(4)), Ok(Int(-4)));
    assert_eq!(unary(U::Not, &Bool(false)), Ok(Bool(true)));
    assert!(unary(U::Not, &Int(1)).is_err());
}

#[test]
fn computes_sizes_durations_and_dates_by_kind() {
    use Value::*;
    assert_eq!(ok(B::Greater, Size(10_000_000), Size(900_000)), Bool(true));
    assert_eq!(ok(B::Add, Size(1000), Size(24)), Size(1024));
    assert_eq!(ok(B::Multiply, Size(1000), Int(3)), Size(3000));
    assert_eq!(ok(B::Multiply, Int(3), Duration(5)), Duration(15));
    assert_eq!(ok(B::Divide, Duration(10), Float(4.0)), Duration(3));
    assert!(binary(B::Add, &Size(1), &Duration(1)).is_err());
    assert!(binary(B::Add, &Size(1), &Int(1)).is_err());
    assert!(binary(B::Divide, &Int(1), &Size(1)).is_err());
    let day = 86_400_000_000_000;
    let a = date(2026, 11, 10, None);
    let b = date(2026, 11, 12, None);
    assert_eq!(ok(B::Subtract, b.clone(), a.clone()), Duration(2 * day));
    assert_eq!(ok(B::Add, a.clone(), Duration(2 * day)), b);
    assert_eq!(ok(B::Less, a.clone(), b.clone()), Bool(true));
    assert!(binary(B::Add, &a, &b).is_err());
}

#[test]
fn joins_strings_and_lists_and_compares_values() {
    use Value::*;
    assert_eq!(
        ok(B::Add, String("a".into()), String("b".into())),
        String("ab".into())
    );
    assert_eq!(
        ok(B::Add, List(vec![Int(1)]), List(vec![Int(2)])),
        List(vec![Int(1), Int(2)])
    );
    assert_eq!(ok(B::Equal, Int(1), Float(1.0)), Bool(true));
    assert_eq!(ok(B::Equal, Int(1), String("1".into())), Bool(false));
    let r1 = Record(vec![("a".into(), Int(1)), ("b".into(), Int(2))]);
    let r2 = Record(vec![("b".into(), Int(2)), ("a".into(), Int(1))]);
    assert_eq!(ok(B::Equal, r1, r2), Bool(true));
    assert_eq!(
        ok(B::Less, String("a".into()), String("b".into())),
        Bool(true)
    );
    assert!(binary(B::Less, &Int(1), &String("b".into())).is_err());
    assert_eq!(ok(B::Default, Null, Int(3)), Int(3));
    assert_eq!(ok(B::Default, Int(1), Int(3)), Int(1));
}

#[test]
fn reads_fields_and_items() {
    use Value::*;
    let r = Record(vec![("name".into(), String("x".into()))]);
    assert_eq!(r.field("name"), Ok(&String("x".into())));
    assert!(r.field("size").is_err());
    let l = List(vec![Int(1), Int(2)]);
    assert_eq!(l.item(1), Ok(&Int(2)));
    assert!(l.item(2).is_err());
    assert!(Int(1).field("x").is_err());
}

#[test]
fn writes_scalars_as_literals() {
    use Value::*;
    assert_eq!(scalar_text(&Int(-3)), "-3");
    assert_eq!(scalar_text(&Float(2.0)), "2.0");
    assert_eq!(scalar_text(&Float(1.5)), "1.5");
    assert_eq!(scalar_text(&Size(10_000_000)), "10mb");
    assert_eq!(scalar_text(&Size(2048)), "2kib");
    assert_eq!(scalar_text(&Size(1500)), "1500b");
    assert_eq!(scalar_text(&Duration(90_000_000_000)), "90s");
    assert_eq!(scalar_text(&Duration(3_600_000_000_000)), "1h");
    assert_eq!(scalar_text(&date(2026, 11, 10, None)), "2026-11-10");
    assert_eq!(
        scalar_text(&date(2026, 11, 10, Some(3600))),
        "2026-11-10T00:00:00+01:00"
    );
    assert_eq!(
        scalar_text(&date(1970, 1, 1, Some(0))),
        "1970-01-01T00:00:00Z"
    );
    assert_eq!(scalar_text(&Null), "null");
    assert_eq!(scalar_text(&String("a b".into())), "a b");
}

#[test]
fn round_trips_dates_through_civil_days() {
    for days in [-800_000, -1, 0, 1, 19_000, 20_402, 2_000_000] {
        let (y, m, d) = civil_from_days(days);
        assert_eq!(days_from_civil(y, m, d), days);
    }
    assert_eq!(civil_from_days(20_402), (2025, 11, 10));
}

#[test]
fn writes_values_for_programs_and_the_prompt() {
    use Value::*;
    let list = List(vec![String("a".into()), Int(2)]);
    assert_eq!(program_text(&list), "a\n2\n");
    let rows = List(vec![
        Record(vec![
            ("name".into(), String("a".into())),
            ("size".into(), Size(1000)),
        ]),
        Record(vec![
            ("name".into(), String("bcd".into())),
            ("size".into(), Size(5)),
        ]),
    ]);
    assert_eq!(
        program_text(&rows),
        "{\"name\":\"a\",\"size\":1000}\n{\"name\":\"bcd\",\"size\":5}\n"
    );
    assert_eq!(prompt_text(&rows), "name  size\na     1kb\nbcd   5b\n");
    assert_eq!(prompt_text(&Int(3)), "3\n");
    assert_eq!(json(&String("a\"\n".into())), "\"a\\\"\\n\"");
}

#[test]
fn matches_regular_expressions() {
    use Value::*;
    let text = String("file-12.txt".into());
    assert_eq!(
        ok(B::Matches, text.clone(), String(r"-\d+\.".into())),
        Bool(true)
    );
    assert_eq!(
        ok(B::NotMatches, text.clone(), String("^x".into())),
        Bool(true)
    );
    assert!(
        binary(B::Matches, &text, &String("(".into()))
            .unwrap_err()
            .contains("not a regular expression")
    );
    assert!(binary(B::Matches, &Int(1), &String("1".into())).is_err());
}
