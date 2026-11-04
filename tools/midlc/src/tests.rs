use crate::ast::*;
use crate::parse;

const SAMPLE: &str = r#"
/// A sample.
@available(added=1)
library test.sample;
using test.other;

const LIMIT u32 = 0x40;
const NEGATIVE i32 = -5;
alias Deadline = i64;

/// A point.
type Point = struct {
    x i32;
    @available(added=2, removed=3)
    tag string<16>?;
    rights handle<Event, READ | WRITE>;
};

type Color = strict enum : u8 { RED = 1; GREEN = 2; };
type Mode = bits : u32 { A = 0x1; B = 0x2; };
type Shape = flexible union { 1: point Point; 2: points vector<Point, LIMIT>; };
type Extra = table { 1: name string<8>; };

protocol Drawing {
    @1 Draw(shape Shape, at Deadline) -> (count u32) error test.other.Status;
    strict @2 Clear();
    @3 flexible -> Done(count u32,);
};
"#;

#[test]
fn parses_every_kind_of_declaration() {
    let file = parse("sample.midl", SAMPLE).unwrap();
    assert_eq!(file.library.text(), "test.sample");
    assert_eq!(file.attrs.doc, ["A sample."]);
    assert_eq!(file.attrs.available.unwrap().added, Some(1));
    assert_eq!(file.usings[0].text(), "test.other");
    let names: Vec<_> = file.decls.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "LIMIT", "NEGATIVE", "Deadline", "Point", "Color", "Mode", "Shape", "Extra", "Drawing"
        ]
    );
    let DeclKind::Struct(members) = &file.decls[3].kind else {
        panic!("Point is a structure")
    };
    assert_eq!(file.decls[3].attrs.doc, ["A point."]);
    assert!(members[1].ty.optional);
    assert_eq!(members[1].attrs.available.unwrap().removed, Some(3));
    assert!(matches!(&members[2].ty.params[1], Param::Union(r) if r.len() == 2));
    assert!(matches!(
        &file.decls[4].kind,
        DeclKind::Enum { strictness: Strictness::Strict, members, .. } if members.len() == 2
    ));
    assert!(matches!(
        &file.decls[5].kind,
        DeclKind::Bits {
            strictness: Strictness::Flexible,
            ..
        }
    ));
    let DeclKind::Protocol(methods) = &file.decls[8].kind else {
        panic!("Drawing is a protocol")
    };
    assert!(matches!(
        &methods[0].kind,
        MethodKind::TwoWay { error: Some(_), .. }
    ));
    assert_eq!(methods[1].strictness, Strictness::Strict);
    assert!(matches!(&methods[2].kind, MethodKind::Event(a) if a.len() == 1));
}

#[test]
fn rejects_malformed_files() {
    let cases = [
        ("const A u32 = 1;", 1, 1),
        ("library a;\ntype A = struct { x u32 }", 2, 25),
        ("library a;\n@deprecated library b;", 2, 2),
        ("library a;\ntype A = strict struct {};", 2, 17),
        ("library a;\nprotocol P { strict @1 strict F(); };", 2, 31),
    ];
    for (text, line, col) in cases {
        let err = parse("bad.midl", text).unwrap_err();
        assert_eq!((err.span.line, err.span.col), (line, col), "{text}: {err}");
    }
}

mod checks {
    use crate::compile;
    use crate::model::*;

    const OTHER: &str = "library test.other;\ntype Status = struct { code i32; };\n";

    fn check_one(text: &str) -> Result<Model, String> {
        compile(&[("t.midl", text), ("other.midl", OTHER)], None).map_err(|e| e.to_string())
    }

    fn fails(text: &str, message: &str) {
        let err = check_one(&format!("library t;\nusing test.other;\n{text}")).unwrap_err();
        assert!(err.contains(message), "{text}: {err}");
    }

    #[test]
    fn resolves_types_and_aliases() {
        let text = "library t;\nusing test.other;\nalias Deadline = i64;\ntype P = struct { s string<16>?; h handle<Event, READ | WRITE>; d Deadline; o test.other.Status; };\ntype U = union { 1: p P; 2: v vector<P, 4>; };\nprotocol Q { @1 F(u U) -> (n u32) error i32; };\n";
        let model = check_one(text).unwrap();
        let names: Vec<_> = model.libraries.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["test.other", "t"]);
        let Some(Decl {
            kind: DeclKind::Struct(fields),
            ..
        }) = model.decl("t.P")
        else {
            panic!("P is a structure");
        };
        assert_eq!(
            fields[0].ty,
            Type::String {
                max: 16,
                optional: true
            }
        );
        assert_eq!(
            fields[1].ty,
            Type::Handle {
                object: Some(5),
                rights: Some(4 | 8),
                optional: false
            }
        );
        assert_eq!(fields[2].ty, Type::Int(Int::I64), "the alias is expanded");
        assert_eq!(
            fields[3].ty,
            Type::Named {
                name: "test.other.Status".into(),
                optional: false
            }
        );
    }

    #[test]
    fn rejects_unresolved_and_misused_names() {
        fails("type A = struct { b B; };", "`B` is not declared");
        fails("type A = struct {}; type A = struct {};", "declared twice");
        fails(
            "type A = struct { x test.missing.B; };",
            "not named by `using`",
        );
        fails("alias A = B; alias B = A;", "refers to itself");
        fails("type A = struct { x u32?; };", "cannot be optional");
        fails(
            "type A = struct { x handle<Event, FLY>; };",
            "unknown right",
        );
        fails("type A = struct { x client_end<A>; };", "not a protocol");
        fails("protocol P { @1 F(); @1 G(); };", "unique");
    }
}
