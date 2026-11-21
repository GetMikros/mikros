//! Generates the code of the Wayland protocols that the compositor
//! serves from their protocol files: those of other projects, which
//! `cargo xtask` stages in target/wayland-protocols, and the Mikros ones
//! in protocols/. Each interface becomes a module with its `Request`s,
//! decoded from the arguments that `wayland::wire::decode` reads, a
//! `send_` function for each event, and a module of constants for each
//! enum. It also compiles the renderer's shaders to SPIR-V.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::{env, fs};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let staged = manifest.join("../../../target/wayland-protocols");
    let mut files = xml_files(&staged).unwrap_or_else(|| {
        panic!(
            "no protocol files in {}: `cargo xtask build`, `test` and `check` stage them",
            staged.display()
        )
    });
    files.extend(xml_files(&manifest.join("protocols")).unwrap_or_default());
    let mut out = String::new();
    for file in &files {
        println!("cargo:rerun-if-changed={}", file.display());
        let text = fs::read_to_string(file).expect("a readable protocol file");
        let doc =
            roxmltree::Document::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        for interface in children(doc.root_element(), "interface") {
            generate(&mut out, interface);
        }
    }
    let path = PathBuf::from(env::var("OUT_DIR").expect("cargo sets it")).join("protocols.rs");
    fs::write(path, out).expect("a writable output directory");
    shaders(&manifest);
    link_vulkan(&manifest);
}

/// Compiles the entry points of `shaders/compose.wgsl` to SPIR-V for the
/// renderer: `compose.vert.spv` and `compose.frag.spv` in the output
/// directory.
fn shaders(manifest: &Path) {
    let file = manifest.join("shaders/compose.wgsl");
    println!("cargo:rerun-if-changed={}", file.display());
    let source = fs::read_to_string(&file).expect("a readable shader");
    let shown = file.display().to_string();
    let module = naga::front::wgsl::parse_str(&source)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string_with_path(&source, &shown)));
    let flags = naga::valid::ValidationFlags::all();
    let info = naga::valid::Validator::new(flags, naga::valid::Capabilities::empty())
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string_with_path(&source, &shown)));
    let options = naga::back::spv::Options {
        lang_version: (1, 3),
        ..Default::default()
    };
    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets it"));
    for (stage, entry, name) in [
        (naga::ShaderStage::Vertex, "vertex", "compose.vert.spv"),
        (naga::ShaderStage::Fragment, "fragment", "compose.frag.spv"),
    ] {
        let pipeline = naga::back::spv::PipelineOptions {
            shader_stage: stage,
            entry_point: entry.into(),
        };
        let words = naga::back::spv::write_vec(&module, &info, &options, Some(&pipeline))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        fs::write(out.join(name), bytes).expect("a writable output directory");
    }
}

/// Links the Vulkan loader, which `cargo xtask` builds as a port before
/// the programs with std, on Mikros; the host builds only the tests.
fn link_vulkan(manifest: &Path) {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("mikros") {
        return;
    }
    let lib = manifest.join("../../../target/ported/vulkan-loader/install/system/libraries");
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!(
        "cargo:rerun-if-changed={}",
        lib.join("libvulkan.dlib").display()
    );
}

/// The protocol files in `dir`, sorted, or None when it does not exist.
fn xml_files(dir: &Path) -> Option<Vec<PathBuf>> {
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|e| e == "xml"))
        .collect();
    files.sort();
    Some(files)
}

fn children<'a>(
    node: roxmltree::Node<'a, 'a>,
    tag: &'static str,
) -> impl Iterator<Item = roxmltree::Node<'a, 'a>> {
    node.children().filter(move |n| n.has_tag_name(tag))
}

fn attribute<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> &'a str {
    node.attribute(name)
        .unwrap_or_else(|| panic!("<{}> without `{name}`", node.tag_name().name()))
}

/// `name` as a Rust identifier: raw when it is a keyword, and with an
/// `_` after it when it is one that cannot be raw.
fn ident(name: &str) -> String {
    const KEYWORDS: [&str; 46] = [
        "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "do",
        "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in",
        "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
        "return", "static", "struct", "trait", "true", "try", "type", "typeof", "unsafe",
        "unsized", "use", "virtual", "where",
    ];
    const NOT_RAW: [&str; 4] = ["crate", "self", "super", "Self"];
    if NOT_RAW.contains(&name) {
        format!("{name}_")
    } else if KEYWORDS.contains(&name) || name == "while" || name == "yield" {
        format!("r#{name}")
    } else {
        name.to_owned()
    }
}

/// `name`, in snake case, in camel case.
fn camel(name: &str) -> String {
    name.split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// An argument of a message.
struct Argument {
    name: String,
    kind: String,
    nullable: bool,
    interface: Option<String>,
}

impl Argument {
    fn new(node: roxmltree::Node) -> Argument {
        Argument {
            name: attribute(node, "name").to_owned(),
            kind: attribute(node, "type").to_owned(),
            nullable: node.attribute("allow-null") == Some("true"),
            interface: node.attribute("interface").map(str::to_owned),
        }
    }

    /// Its `ArgType` in a signature.
    fn arg_type(&self) -> String {
        match self.kind.as_str() {
            "int" => "ArgType::Int".into(),
            "uint" => "ArgType::Uint".into(),
            "fixed" => "ArgType::Fixed".into(),
            "string" => format!("ArgType::String {{ nullable: {} }}", self.nullable),
            "object" => format!("ArgType::Object {{ nullable: {} }}", self.nullable),
            "new_id" => match &self.interface {
                Some(i) => format!("ArgType::NewId {{ interface: Some({i:?}) }}"),
                None => "ArgType::NewId { interface: None }".into(),
            },
            "array" => "ArgType::Array".into(),
            "fd" => "ArgType::Fd".into(),
            other => panic!("an argument of type {other}"),
        }
    }

    /// Its fields in a `Request`, with their types.
    fn fields(&self) -> Vec<(String, &'static str)> {
        let name = ident(&self.name);
        let ty = match self.kind.as_str() {
            "int" => "i32",
            "uint" => "u32",
            "fixed" => "Fixed",
            "string" if self.nullable => "Option<String>",
            "string" => "String",
            "object" if self.nullable => "Option<ObjectId>",
            "object" => "ObjectId",
            "new_id" if self.interface.is_none() => {
                return vec![
                    (format!("{}_interface", self.name), "String"),
                    (format!("{}_version", self.name), "u32"),
                    (name, "NewId"),
                ];
            }
            "new_id" => "NewId",
            "array" => "Vec<u8>",
            "fd" => "Fd",
            other => panic!("an argument of type {other}"),
        };
        vec![(name, ty)]
    }

    /// The pattern of the decoded `Arg` that binds its fields.
    fn pattern(&self) -> String {
        let fields = self.fields();
        let name = &fields.last().expect("a field").0;
        match self.kind.as_str() {
            "int" => format!("Arg::Int({name})"),
            "uint" => format!("Arg::Uint({name})"),
            "fixed" => format!("Arg::Fixed({name})"),
            "string" if self.nullable => format!("Arg::String({name})"),
            "string" => format!("Arg::String(Some({name}))"),
            "object" if self.nullable => format!("Arg::Object({name})"),
            "object" => format!("Arg::Object(Some({name}))"),
            "new_id" if self.interface.is_none() => {
                let id = match name.as_str() {
                    "id" => "id".to_owned(),
                    _ => format!("id: {name}"),
                };
                format!(
                    "Arg::Untyped {{ interface: {}, version: {}, {id} }}",
                    fields[0].0, fields[1].0
                )
            }
            "new_id" => format!("Arg::NewId({name})"),
            "array" => format!("Arg::Array({name})"),
            _ => format!("Arg::Fd({name})"),
        }
    }

    /// Its parameter in an event's `send_` function, and the `Arg` made
    /// of it.
    fn parameter(&self) -> (String, String) {
        let name = ident(&self.name);
        let (ty, arg) = match self.kind.as_str() {
            "int" => ("i32", format!("Arg::Int({name})")),
            "uint" => ("u32", format!("Arg::Uint({name})")),
            "fixed" => ("Fixed", format!("Arg::Fixed({name})")),
            "string" if self.nullable => (
                "Option<&str>",
                format!("Arg::String({name}.map(str::to_owned))"),
            ),
            "string" => ("&str", format!("Arg::String(Some({name}.to_owned()))")),
            "object" if self.nullable => ("Option<ObjectId>", format!("Arg::Object({name})")),
            "object" => ("ObjectId", format!("Arg::Object(Some({name}))")),
            "new_id" => ("NewId", format!("Arg::NewId({name})")),
            "array" => ("&[u8]", format!("Arg::Array({name}.to_vec())")),
            "fd" => ("Fd", format!("Arg::Fd({name})")),
            other => panic!("an argument of type {other}"),
        };
        (format!("{name}: {ty}"), arg)
    }
}

/// A request or event.
struct Msg {
    name: String,
    since: u32,
    destructor: bool,
    args: Vec<Argument>,
}

impl Msg {
    fn new(node: roxmltree::Node) -> Msg {
        Msg {
            name: attribute(node, "name").to_owned(),
            since: node
                .attribute("since")
                .map_or(1, |s| s.parse().expect("a version")),
            destructor: node.attribute("type") == Some("destructor"),
            args: children(node, "arg").map(Argument::new).collect(),
        }
    }

    /// Its entry in the interface's table of messages.
    fn descriptor(&self) -> String {
        let args: Vec<String> = self.args.iter().map(Argument::arg_type).collect();
        format!(
            "Message {{ name: {:?}, since: {}, destructor: {}, args: &[{}] }},\n",
            self.name,
            self.since,
            self.destructor,
            args.join(", ")
        )
    }
}

fn generate(out: &mut String, interface: roxmltree::Node) {
    let name = attribute(interface, "name");
    let version = attribute(interface, "version");
    let requests: Vec<Msg> = children(interface, "request").map(Msg::new).collect();
    let events: Vec<Msg> = children(interface, "event").map(Msg::new).collect();
    writeln!(out, "pub mod {name} {{").unwrap();
    out.push_str(
        "use crate::wayland::wire::{Arg, ArgType, DecodeError, Fd, Fixed, Interface, \
         Message, NewId, ObjectId, Outgoing};\n",
    );
    writeln!(out, "pub const NAME: &str = {name:?};").unwrap();
    writeln!(out, "pub const VERSION: u32 = {version};").unwrap();
    out.push_str(
        "pub static INTERFACE: Interface = Interface { name: NAME, version: VERSION, \
         requests: &REQUESTS, events: &EVENTS };\n",
    );
    for (table, messages) in [("REQUESTS", &requests), ("EVENTS", &events)] {
        writeln!(out, "pub static {table}: [Message; {}] = [", messages.len()).unwrap();
        for message in messages.iter() {
            out.push_str(&message.descriptor());
        }
        out.push_str("];\n");
    }

    // The requests, decoded.
    out.push_str("#[derive(Debug)]\npub enum Request {\n");
    for request in &requests {
        let fields: Vec<String> = request
            .args
            .iter()
            .flat_map(Argument::fields)
            .map(|(field, ty)| format!("{field}: {ty}"))
            .collect();
        match fields.is_empty() {
            true => writeln!(out, "{},", camel(&request.name)).unwrap(),
            false => {
                writeln!(out, "{} {{ {} }},", camel(&request.name), fields.join(", ")).unwrap()
            }
        }
    }
    out.push_str(
        "}\nimpl Request {\n\
         /// The request `opcode` with the arguments that `wire::decode` read.\n\
         pub fn decode(opcode: u16, args: Vec<Arg>) -> Result<Request, DecodeError> {\n\
         let mut args = args.into_iter();\n\
         match opcode {\n",
    );
    for (opcode, request) in requests.iter().enumerate() {
        let variant = camel(&request.name);
        let fields: Vec<String> = request
            .args
            .iter()
            .flat_map(Argument::fields)
            .map(|(field, _)| field)
            .collect();
        if fields.is_empty() {
            writeln!(out, "{opcode} => Ok(Request::{variant}),").unwrap();
            continue;
        }
        let built = format!("Request::{variant} {{ {} }}", fields.join(", "));
        let nexts = vec!["args.next()"; request.args.len()].join(", ");
        let patterns: Vec<String> = request
            .args
            .iter()
            .map(|a| format!("Some({})", a.pattern()))
            .collect();
        writeln!(
            out,
            "{opcode} => match ({nexts}{comma}) {{ ({patterns}{comma}) => Ok({built}), _ => Err(DecodeError::Length) }},",
            comma = if request.args.len() == 1 { "," } else { "" },
            patterns = patterns.join(", "),
        )
        .unwrap();
    }
    out.push_str("_ => Err(DecodeError::Opcode),\n}\n}\n}\n");

    // The events, sent.
    for (opcode, event) in events.iter().enumerate() {
        let (parameters, args): (Vec<String>, Vec<String>) =
            event.args.iter().map(Argument::parameter).unzip();
        let mut parameters = parameters;
        parameters.insert(0, "object: ObjectId".into());
        parameters.insert(0, "out: &mut Outgoing".into());
        writeln!(
            out,
            "pub fn send_{}({}) {{ out.send(object, {opcode}, vec![{}]); }}",
            event.name,
            parameters.join(", "),
            args.join(", ")
        )
        .unwrap();
    }

    // The enums, as constants.
    for e in children(interface, "enum") {
        writeln!(out, "pub mod {} {{", ident(attribute(e, "name"))).unwrap();
        for entry in children(e, "entry") {
            let entry_name = attribute(entry, "name").to_ascii_uppercase();
            let entry_name = match entry_name.starts_with(|c: char| c.is_ascii_digit()) {
                true => format!("_{entry_name}"),
                false => entry_name,
            };
            writeln!(
                out,
                "pub const {entry_name}: u32 = {};",
                attribute(entry, "value")
            )
            .unwrap();
        }
        out.push_str("}\n");
    }
    out.push_str("}\n");
}
