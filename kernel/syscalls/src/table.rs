//! The system call table: its entries, read from `kernel/syscalls.yaml`
//! and checked.

use crate::yaml::{self, Error, Map, Position, Value};
use std::collections::HashMap;

/// The architectures the kernel supports.
pub const ARCHES: [&str; 1] = ["x86_64"];

/// The object types a handle argument can accept, by name.
pub const OBJECT_TYPES: [&str; 17] = [
    "Process",
    "Thread",
    "MemoryObject",
    "Channel",
    "Event",
    "Port",
    "Timer",
    "Interrupt",
    "Resource",
    "Log",
    "Budget",
    "LogReader",
    "Pager",
    "Timeline",
    "Stream",
    "Bti",
    "Pmt",
];

/// The handle rights, by name.
pub const RIGHTS: [&str; 10] = [
    "DUPLICATE",
    "TRANSFER",
    "READ",
    "WRITE",
    "EXECUTE",
    "MAP",
    "WAIT",
    "SIGNAL",
    "MANAGE",
    "INSPECT",
];

/// The types that integer arguments and pointer targets can have.
pub const VALUE_TYPES: [&str; 6] = ["u8", "u32", "u64", "i32", "i64", "Handle"];

/// The most arguments a call has on x86_64.
pub const MAX_ARGS_X86_64: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    /// `u32`, `u64`, `i32` or `i64`.
    Int(String),
    /// A `u32` handle value.
    Handle,
    /// A user address the kernel reads from: `*const T`.
    Const(String),
    /// A user address the kernel writes to: `*mut T`.
    Mut(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    In,
    Out,
    InOut,
}

/// What a handle argument must refer to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandleCheck {
    /// The accepted object types, or `None` for every type.
    pub types: Option<Vec<String>>,
    /// The rights the handle must hold.
    pub rights: Vec<String>,
    /// Whether `HANDLE_INVALID` is accepted, and skips the checks.
    pub allow_invalid: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arg {
    pub name: String,
    pub ty: Type,
    pub dir: Dir,
    /// Set for handle arguments.
    pub handle: Option<HandleCheck>,
    /// For a `*mut Handle` argument, the object type of the handles the
    /// call returns there, when it is always the same.
    pub returns: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub name: String,
    pub number: u32,
    pub args: Vec<Arg>,
    pub arches: Vec<String>,
    pub fuzz_only: bool,
    /// The IDs of the requirements that define the call.
    pub spec: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub calls: Vec<Call>,
}

/// A table and the keys in it that were ignored.
#[derive(Debug)]
pub struct Read {
    pub table: Table,
    pub ignored: Vec<(String, Position)>,
}

/// Whether `id` is a requirement ID such as `MIKSPEC-0012.R4`.
fn is_requirement_id(id: &str) -> bool {
    let Some((document, requirement)) = id
        .strip_prefix("MIKSPEC-")
        .and_then(|rest| rest.split_once(".R"))
    else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    document.len() == 4 && digits(document) && digits(requirement) && !requirement.starts_with('0')
}

/// Whether `name` is usable as a call or argument name.
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

struct Reader {
    ignored: Vec<(String, Position)>,
}

impl Reader {
    fn done(&mut self, map: &Map<'_>) {
        self.ignored.extend(map.unknown());
    }

    fn strings(&self, value: &Value) -> Result<Vec<(String, Position)>, Error> {
        value
            .as_list()?
            .iter()
            .map(|v| Ok((v.as_str()?.to_owned(), v.at)))
            .collect()
    }

    fn one_of(&self, value: &Value, allowed: &[&str], what: &str) -> Result<String, Error> {
        let s = value.as_str()?;
        if allowed.contains(&s) {
            Ok(s.to_owned())
        } else {
            Err(value.at.error(format!("unknown {what} `{s}`")))
        }
    }

    fn ty(&self, value: &Value) -> Result<Type, Error> {
        let text = value.as_str()?;
        let target = |t: &str| {
            let t = t.trim();
            if VALUE_TYPES.contains(&t) {
                Ok(t.to_owned())
            } else {
                Err(value.at.error(format!("unknown pointer target `{t}`")))
            }
        };
        if let Some(t) = text.strip_prefix("*const ") {
            Ok(Type::Const(target(t)?))
        } else if let Some(t) = text.strip_prefix("*mut ") {
            Ok(Type::Mut(target(t)?))
        } else if text == "Handle" {
            Ok(Type::Handle)
        } else if ["u32", "u64", "i32", "i64"].contains(&text) {
            Ok(Type::Int(text.to_owned()))
        } else {
            Err(value.at.error(format!("unknown type `{text}`")))
        }
    }

    fn arg(&mut self, value: &Value) -> Result<Arg, Error> {
        let mut map = value.as_map()?;
        let name_value = map.require("name")?;
        let name = name_value.as_str()?.to_owned();
        if !is_identifier(&name) {
            return Err(name_value.at.error(format!("bad argument name `{name}`")));
        }
        let ty_value = map.require("type")?;
        let ty = self.ty(ty_value)?;
        let dir_value = map.require("dir")?;
        let dir = match dir_value.as_str()? {
            "in" => Dir::In,
            "out" => Dir::Out,
            "inout" => Dir::InOut,
            other => return Err(dir_value.at.error(format!("unknown direction `{other}`"))),
        };
        let fits = match ty {
            Type::Int(_) | Type::Handle | Type::Const(_) => dir == Dir::In,
            Type::Mut(_) => dir != Dir::In,
        };
        if !fits {
            return Err(dir_value
                .at
                .error(format!("`{name}` cannot have this direction")));
        }
        let handle = if ty == Type::Handle {
            let object = map.require("object")?;
            let types = match &object.node {
                yaml::Node::List(_) => Some(
                    self.strings(object)?
                        .into_iter()
                        .map(|(t, at)| {
                            if OBJECT_TYPES.contains(&t.as_str()) {
                                Ok(t)
                            } else {
                                Err(at.error(format!("unknown object type `{t}`")))
                            }
                        })
                        .collect::<Result<_, _>>()?,
                ),
                _ if object.as_str()? == "any" => None,
                _ => Some(vec![self.one_of(object, &OBJECT_TYPES, "object type")?]),
            };
            let rights_value = map.require("rights")?;
            let mut rights = Vec::new();
            for (right, at) in self.strings(rights_value)? {
                if !RIGHTS.contains(&right.as_str()) {
                    return Err(at.error(format!("unknown right `{right}`")));
                }
                rights.push(right);
            }
            let allow_invalid = match map.get("allow_invalid") {
                Some(v) => v.as_bool()?,
                None => false,
            };
            Some(HandleCheck {
                types,
                rights,
                allow_invalid,
            })
        } else {
            let returns_handle = ty == Type::Mut("Handle".into());
            for key in ["object", "rights", "allow_invalid"] {
                if key == "object" && returns_handle {
                    continue;
                }
                if let Some(v) = map.get(key) {
                    return Err(v.at.error(format!("`{key}` applies only to handles")));
                }
            }
            None
        };
        let returns = match map.get("object") {
            Some(object) if handle.is_none() => {
                Some(self.one_of(object, &OBJECT_TYPES, "object type")?)
            }
            _ => None,
        };
        self.done(&map);
        Ok(Arg {
            name,
            ty,
            dir,
            handle,
            returns,
        })
    }

    fn call(&mut self, value: &Value) -> Result<Call, Error> {
        let mut map = value.as_map()?;
        let name_value = map.require("name")?;
        let name = name_value.as_str()?.to_owned();
        if !is_identifier(&name) {
            return Err(name_value.at.error(format!("bad call name `{name}`")));
        }
        let number = map.require("number")?.as_int(0, i128::from(u32::MAX))? as u32;
        let mut arches = Vec::new();
        for (arch, at) in self.strings(map.require("arch")?)? {
            if !ARCHES.contains(&arch.as_str()) {
                return Err(at.error(format!("unknown architecture `{arch}`")));
            }
            arches.push(arch);
        }
        let fuzz_only = map.require("fuzz_only")?.as_bool()?;
        let mut spec = Vec::new();
        for (id, at) in self.strings(map.require("spec")?)? {
            if !is_requirement_id(&id) {
                return Err(at.error(format!("`{id}` is not a requirement ID")));
            }
            spec.push(id);
        }
        if spec.is_empty() {
            return Err(map.at().error(format!("`{name}` lists no requirement")));
        }
        let args = match map.get("args") {
            Some(v) => v
                .as_list()?
                .iter()
                .map(|a| self.arg(a))
                .collect::<Result<Vec<_>, _>>()?,
            None => Vec::new(),
        };
        if arches.iter().any(|a| a == "x86_64") && args.len() > MAX_ARGS_X86_64 {
            return Err(map.at().error(format!(
                "`{name}` has more than {MAX_ARGS_X86_64} arguments"
            )));
        }
        let mut seen = HashMap::new();
        for arg in &args {
            if seen.insert(arg.name.as_str(), ()).is_some() {
                return Err(map
                    .at()
                    .error(format!("`{name}` has two arguments named `{}`", arg.name)));
            }
        }
        self.done(&map);
        Ok(Call {
            name,
            number,
            args,
            arches,
            fuzz_only,
            spec,
        })
    }
}

/// Reads and checks the table in `bytes`.
pub fn read(bytes: &[u8]) -> Result<Read, Error> {
    let root = yaml::parse(bytes)?;
    let mut map = root.as_map()?;
    let mut reader = Reader {
        ignored: Vec::new(),
    };
    let entries = map.require("syscalls")?;
    let mut calls = Vec::new();
    let mut names = HashMap::new();
    let mut numbers = HashMap::new();
    for entry in entries.as_list()? {
        let call = reader.call(entry)?;
        if names.insert(call.name.clone(), ()).is_some() {
            return Err(entry.at.error(format!("`{}` is defined twice", call.name)));
        }
        if let Some(other) = numbers.insert(call.number, call.name.clone()) {
            return Err(entry.at.error(format!(
                "`{}` has the number {} of `{other}`",
                call.name, call.number
            )));
        }
        calls.push(call);
    }
    reader.done(&map);
    calls.sort_by_key(|c| c.number);
    Ok(Read {
        table: Table { calls },
        ignored: reader.ignored,
    })
}
