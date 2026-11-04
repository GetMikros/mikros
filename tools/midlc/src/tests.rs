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

const OTHER: &str = "library test.other;\ntype Status = enum : i32 { OK = 0; FAILED = -1; };\n";

mod checks {
    use super::{OTHER, SAMPLE};
    use crate::compile;
    use crate::model::*;

    fn check_one(text: &str) -> Result<Model, String> {
        compile(&[("t.midl", text), ("other.midl", OTHER)], None).map_err(|e| e.to_string())
    }

    fn fails(text: &str, message: &str) {
        let err = check_one(&format!("library t;\nusing test.other;\n{text}")).unwrap_err();
        assert!(err.contains(message), "{text}: {err}");
    }

    #[test]
    fn resolves_the_sample() {
        let model = compile(&[("sample.midl", SAMPLE), ("other.midl", OTHER)], None).unwrap();
        let names: Vec<_> = model.libraries.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["test.other", "test.sample"]);
        let Some(Decl {
            kind: DeclKind::Struct(fields),
            ..
        }) = model.decl("test.sample.Point")
        else {
            panic!("Point is a structure");
        };
        assert_eq!(
            fields[1].ty,
            Type::String {
                max: 16,
                optional: true
            }
        );
        assert_eq!(
            fields[2].ty,
            Type::Handle {
                object: Some(5),
                rights: Some(4 | 8),
                optional: false
            }
        );
        let Some(Decl {
            kind: DeclKind::Union { members, .. },
            ..
        }) = model.decl("test.sample.Shape")
        else {
            panic!("Shape is a union");
        };
        assert!(matches!(&members[1].ty, Type::Vector { max: 64, .. }));
        let Some(Decl {
            kind: DeclKind::Protocol(methods),
            ..
        }) = model.decl("test.sample.Drawing")
        else {
            panic!("Drawing is a protocol");
        };
        let MethodKind::TwoWay { request, error, .. } = &methods[0].kind else {
            panic!("Draw is two-way");
        };
        assert_eq!(request[1].ty, Type::Int(Int::I64), "the alias is expanded");
        assert_eq!(
            error,
            &Some(Type::Named {
                name: "test.other.Status".into(),
                optional: false
            })
        );
    }

    #[test]
    fn keeps_what_a_version_has() {
        let at = |version| {
            let model = compile(
                &[("sample.midl", SAMPLE), ("other.midl", OTHER)],
                Some(version),
            )
            .unwrap();
            match model.decl("test.sample.Point") {
                Some(Decl {
                    kind: DeclKind::Struct(fields),
                    ..
                }) => fields.len(),
                _ => panic!("Point is a structure"),
            }
        };
        assert_eq!((at(1), at(2), at(3)), (2, 3, 2));
        let missing =
            "library t;\n@available(added=2)\ntype A = struct {};\ntype B = struct { a A; };\n";
        let err = compile(&[("t.midl", missing)], Some(1)).unwrap_err();
        assert!(
            err.message.contains("not declared or not available"),
            "{err}"
        );
    }

    #[test]
    fn rejects_invalid_libraries() {
        fails("type A = struct { b B; };", "`B` is not declared");
        fails("type A = struct {}; type A = struct {};", "declared twice");
        fails("type A = struct { x u32; x u32; };", "declared twice");
        fails(
            "type A = struct { x test.missing.B; };",
            "not named by `using`",
        );
        fails("type A = struct { a A; };", "contains itself");
        fails("alias A = B; alias B = A;", "refers to itself");
        fails("const A u32 = B; const B u32 = A;", "refers to itself");
        fails("type A = bits : i8 { X = 1; };", "unsigned integer");
        fails("type A = enum : u8 { X = 256; };", "does not fit");
        fails(
            "type A = bits : u8 { X = 1; Y = 2; Z = X | Y; W = Q; };",
            "`Q`",
        );
        fails("type A = union { 0: x u32; };", "at least 1");
        fails("type A = table { 1: x string<4>?; };", "cannot be optional");
        fails("type A = struct { x u32?; };", "cannot be optional");
        fails(
            "type A = struct { x array<u8, 4>?; };",
            "cannot be optional",
        );
        fails(
            "type A = struct { x handle<Socket>; };",
            "unknown object type",
        );
        fails(
            "type A = struct { x handle<Event, FLY>; };",
            "unknown right",
        );
        fails("type A = struct { x client_end<A>; };", "not a protocol");
        fails("protocol P { @1 F(); @1 G(); };", "unique");
        fails("protocol P { @2147483648 F(); };", "2^31");
        fails(
            "protocol P { @1 F() -> () error string<4>; };",
            "enumeration or i32",
        );
        fails("const N u8 = 300;", "does not fit");
        fails("const S string<2> = \"long\";", "longer");
        fails(
            "type A = struct { x string<N>; }; const N i32 = -1;",
            "non-negative",
        );
    }

    #[test]
    fn accepts_optional_members_and_constants_as_counts() {
        let text = "library t;\nusing test.other;\nconst N u32 = 4;\ntype A = struct { b B?; v vector<u8, N>; s test.other.Status; };\ntype B = struct { a A; };\nprotocol P { @1 F() -> () error i32; };\n";
        check_one(text).unwrap();
    }

    #[test]
    fn rejects_messages_over_the_channel_limits() {
        fails("protocol P { @1 F(v vector<u8, 65536>); };", "exceed");
        fails("protocol P { @1 F(v vector<handle, 65>); };", "exceed");
        fails(
            "type L = struct { next L?; }; protocol P { @1 F(l L); };",
            "exceed",
        );
        fails("protocol P { @1 F() -> (s string<65520>); };", "exceed");
        check_one("library t;\nprotocol P { @1 F(v vector<u8, 65232>, h vector<handle, 64>); };\n")
            .unwrap();
    }
}

mod layouts {
    use crate::compile;
    use crate::layout::*;
    use crate::model::*;

    #[test]
    fn lays_out_structures() {
        let text = "library t;\ntype S = struct { a u8; b u32; c u16; o S?; s string<9>; h handle; };\ntype E = enum : u16 { A = 1; };\n";
        let model = compile(&[("t.midl", text)], None).unwrap();
        let layout = Layout { model: &model };
        let Some(Decl {
            kind: DeclKind::Struct(fields),
            ..
        }) = model.decl("t.S")
        else {
            panic!("S is a structure");
        };
        let (shape, offsets) = layout.struct_shape(fields);
        assert_eq!(offsets, [0, 4, 8, 16, 24, 40]);
        assert_eq!(shape, Shape { size: 48, align: 8 });
        let e = Type::Named {
            name: "t.E".into(),
            optional: false,
        };
        assert_eq!(layout.shape(&e), Shape { size: 2, align: 2 });
        let message = layout.message_max(&fields[4..]);
        assert_eq!(
            message,
            Some(Max {
                bytes: 16 + 24 + 16,
                handles: 1
            })
        );
    }
}
