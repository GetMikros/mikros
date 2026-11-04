//! The wire layout of MIDL types: the inline size and alignment of each
//! type, the offsets of structure members, and the largest encoding a
//! value can have.

use crate::model::*;

/// The size and alignment of a type's inline part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    pub size: u64,
    pub align: u64,
}

/// The largest out-of-line bytes and handles a value can carry, or
/// `None` when there is no bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Max {
    pub bytes: u64,
    pub handles: u64,
}

/// The size of a message header.
pub const HEADER_SIZE: u64 = 16;
/// The largest message a channel carries, in bytes and handles.
pub const MESSAGE_BYTES: u64 = 65536;
pub const MESSAGE_HANDLES: u64 = 64;

pub fn pad8(n: u64) -> Option<u64> {
    n.checked_add(7).map(|n| n & !7)
}

impl Max {
    const ZERO: Max = Max {
        bytes: 0,
        handles: 0,
    };

    fn plus(self, other: Max) -> Option<Max> {
        Some(Max {
            bytes: self.bytes.checked_add(other.bytes)?,
            handles: self.handles.checked_add(other.handles)?,
        })
    }

    fn times(self, n: u64) -> Option<Max> {
        Some(Max {
            bytes: self.bytes.checked_mul(n)?,
            handles: self.handles.checked_mul(n)?,
        })
    }

    fn or(self, other: Max) -> Max {
        Max {
            bytes: self.bytes.max(other.bytes),
            handles: self.handles.max(other.handles),
        }
    }
}

pub struct Layout<'a> {
    pub model: &'a Model,
}

impl Layout<'_> {
    fn decl(&self, name: &str) -> &DeclKind {
        &self.model.decl(name).expect("checked names exist").kind
    }

    /// The inline size and alignment of `ty`.
    pub fn shape(&self, ty: &Type) -> Shape {
        let (size, align) = match ty {
            Type::Bool => (1, 1),
            Type::Int(int) => (int.size(), int.size()),
            Type::F32 => (4, 4),
            Type::F64 => (8, 8),
            Type::String { .. } | Type::Vector { .. } => (16, 8),
            Type::Array { element, len } => {
                let s = self.shape(element);
                (s.size * len, s.align)
            }
            Type::Handle { .. } | Type::Endpoint { .. } => (4, 4),
            Type::Named { name, optional } => match self.decl(name) {
                DeclKind::Struct(_) if *optional => (8, 8),
                DeclKind::Struct(fields) => {
                    let s = self.struct_shape(fields).0;
                    (s.size, s.align)
                }
                DeclKind::Enum { underlying, .. } | DeclKind::Bits { underlying, .. } => {
                    (underlying.size(), underlying.size())
                }
                DeclKind::Union { .. } | DeclKind::Table(_) => (16, 8),
                _ => unreachable!("only data types are named as types"),
            },
        };
        Shape { size, align }
    }

    /// The shape of a structure with `fields` and the offset of each.
    pub fn struct_shape(&self, fields: &[Field]) -> (Shape, Vec<u64>) {
        let (mut size, mut align, mut offsets) = (0u64, 1u64, Vec::new());
        for f in fields {
            let s = self.shape(&f.ty);
            size = size.next_multiple_of(s.align);
            offsets.push(size);
            size += s.size;
            align = align.max(s.align);
        }
        (
            Shape {
                size: size.next_multiple_of(align),
                align,
            },
            offsets,
        )
    }

    /// The most a value of `ty` can add out of line, counting the
    /// handles of its inline part too.
    pub fn max(&self, ty: &Type) -> Option<Max> {
        self.max_in(ty, &mut Vec::new())
    }

    fn max_in(&self, ty: &Type, open: &mut Vec<String>) -> Option<Max> {
        Some(match ty {
            Type::Bool | Type::Int(_) | Type::F32 | Type::F64 => Max::ZERO,
            Type::String { max, .. } => Max {
                bytes: pad8(*max)?,
                handles: 0,
            },
            Type::Vector { element, max, .. } => {
                let elements = Max {
                    bytes: pad8(self.shape(element).size.checked_mul(*max)?)?,
                    handles: 0,
                };
                elements.plus(self.max_in(element, open)?.times(*max)?)?
            }
            Type::Array { element, len } => self.max_in(element, open)?.times(*len)?,
            Type::Handle { .. } | Type::Endpoint { .. } => Max {
                bytes: 0,
                handles: 1,
            },
            Type::Named { name, optional } => {
                if open.contains(name) {
                    return None;
                }
                open.push(name.clone());
                let max = self.named_max(name, *optional, open);
                open.pop();
                max?
            }
        })
    }

    fn named_max(&self, name: &str, optional: bool, open: &mut Vec<String>) -> Option<Max> {
        let out_of_line = |ty: &Type, open: &mut Vec<String>| {
            Max {
                bytes: pad8(self.shape(ty).size)?,
                handles: 0,
            }
            .plus(self.max_in(ty, open)?)
        };
        Some(match self.decl(name) {
            DeclKind::Struct(fields) => {
                let mut max = Max::ZERO;
                for f in fields {
                    max = max.plus(self.max_in(&f.ty, open)?)?;
                }
                if optional {
                    let shape = self.struct_shape(fields).0;
                    max = max.plus(Max {
                        bytes: pad8(shape.size)?,
                        handles: 0,
                    })?;
                }
                max
            }
            DeclKind::Enum { .. } | DeclKind::Bits { .. } => Max::ZERO,
            DeclKind::Union { members, .. } => {
                let mut max = Max::ZERO;
                for m in members {
                    max = max.or(out_of_line(&m.ty, open)?);
                }
                max
            }
            DeclKind::Table(members) => {
                let highest = members.iter().map(|m| m.ordinal).max().unwrap_or(0);
                let mut max = Max {
                    bytes: highest.checked_mul(16)?,
                    handles: 0,
                };
                for m in members {
                    max = max.plus(out_of_line(&m.ty, open)?)?;
                }
                max
            }
            _ => unreachable!("only data types are named as types"),
        })
    }

    /// The largest message whose body is a structure with `fields`, or
    /// `None` when there is no bound.
    pub fn message_max(&self, fields: &[Field]) -> Option<Max> {
        let inline = self.struct_shape(fields).0.size;
        let mut max = Max {
            bytes: HEADER_SIZE.checked_add(pad8(inline)?)?,
            handles: 0,
        };
        for f in fields {
            max = max.plus(self.max(&f.ty)?)?;
        }
        Some(max)
    }

    /// The largest reply of a method with `response` fields and an
    /// `error` type: a union of the results and the error.
    pub fn result_max(&self, response: &[Field], error: &Type) -> Option<Max> {
        let results = self.struct_shape(response).0.size;
        let mut results_max = Max {
            bytes: pad8(results)?,
            handles: 0,
        };
        for f in response {
            results_max = results_max.plus(self.max(&f.ty)?)?;
        }
        let error_max = Max {
            bytes: pad8(self.shape(error).size)?,
            handles: 0,
        };
        Max {
            bytes: HEADER_SIZE + 16,
            handles: 0,
        }
        .plus(results_max.or(error_max))
    }

    /// Whether `max` fits a channel message.
    pub fn fits(max: Option<Max>) -> bool {
        max.is_some_and(|m| m.bytes <= MESSAGE_BYTES && m.handles <= MESSAGE_HANDLES)
    }
}
