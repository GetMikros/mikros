//! Checking constants and the values of enumeration and bit set
//! members.

use super::{Checker, Scope, unique};
use crate::ast::{self, Param};
use crate::model::*;
use crate::{Error, Span};

impl Checker<'_> {
    /// The checked form of a constant, enumeration or bit set.
    pub(super) fn value_decl(&self, decl: &ast::Decl, scope: &Scope) -> Result<DeclKind, Error> {
        use ast::DeclKind as A;
        Ok(match &decl.kind {
            A::Const { ty, value } => {
                let ty = self.ty(scope, ty)?;
                let value = self.constant(scope, &ty, value, decl)?;
                DeclKind::Const { ty, value }
            }
            A::Enum {
                strictness,
                underlying,
                members,
            }
            | A::Bits {
                strictness,
                underlying,
                members,
            } => {
                let bits = matches!(decl.kind, A::Bits { .. });
                let int = match self.ty(scope, underlying)? {
                    Type::Int(int) if !bits || !int.signed() => int,
                    _ => {
                        let what = if bits {
                            "an unsigned integer type"
                        } else {
                            "an integer type"
                        };
                        return Err(underlying
                            .name
                            .span
                            .error(format!("the underlying type must be {what}")));
                    }
                };
                let mut out: Vec<ValueMember> = Vec::new();
                for m in members {
                    if !scope.includes(&m.attrs, &m.span, self.version)? {
                        continue;
                    }
                    unique(&m.name, &m.span, out.iter().map(|o| &o.name))?;
                    let value = self.int_value(scope, int, &m.value, &out)?;
                    out.push(ValueMember {
                        name: m.name.clone(),
                        doc: m.attrs.doc.clone(),
                        value,
                    });
                }
                match bits {
                    false => DeclKind::Enum {
                        strictness: *strictness,
                        underlying: int,
                        members: out,
                    },
                    true => DeclKind::Bits {
                        strictness: *strictness,
                        underlying: int,
                        members: out,
                    },
                }
            }
            _ => unreachable!("only values are checked here"),
        })
    }

    /// A length or count: an integer or the name of an integer constant.
    pub(super) fn count(&self, scope: &Scope, p: &Param, span: &Span) -> Result<u64, Error> {
        match p {
            Param::Int(n, _) => Ok(*n),
            Param::Type(t) if t.params.is_empty() && !t.optional => {
                if let Some(v) = self.const_int(scope, &t.name)?
                    && let Ok(v) = u64::try_from(v)
                {
                    return Ok(v);
                }
                Err(t.name.span.error(format!(
                    "`{}` is not a non-negative integer constant",
                    t.name.text()
                )))
            }
            _ => Err(span.error("expected a count")),
        }
    }

    pub(super) fn constant(
        &self,
        scope: &Scope,
        ty: &Type,
        value: &ast::Constant,
        decl: &ast::Decl,
    ) -> Result<Value, Error> {
        match (ty, value) {
            (Type::Int(int), v) => Ok(Value::Int(self.int_value(scope, *int, v, &[])?)),
            (Type::Bool, ast::Constant::Bool(b, _)) => Ok(Value::Bool(*b)),
            (
                Type::String {
                    max,
                    optional: false,
                },
                ast::Constant::Str(s, span),
            ) => {
                if s.len() as u64 > *max {
                    return Err(span.error("the string is longer than its type allows"));
                }
                Ok(Value::Str(s.clone()))
            }
            _ => Err(decl
                .span
                .error("the value does not fit the constant's type")),
        }
    }

    /// An integer of type `int`: a literal, or names of integer constants
    /// and of `earlier` members joined by `|`.
    pub(super) fn int_value(
        &self,
        scope: &Scope,
        int: Int,
        value: &ast::Constant,
        earlier: &[ValueMember],
    ) -> Result<i128, Error> {
        let (min, max) = int.range();
        let (v, span) = match value {
            ast::Constant::Int {
                value,
                negative,
                span,
            } => {
                let v = i128::from(*value);
                (if *negative { -v } else { v }, span.clone())
            }
            ast::Constant::Names(names) => {
                let mut v = 0;
                for name in names {
                    let found = match (
                        name.parts.as_slice(),
                        earlier.iter().find(|m| m.name == name.text()),
                    ) {
                        (_, Some(m)) => m.value,
                        _ => self.const_int(scope, name)?.ok_or_else(|| {
                            name.span
                                .error(format!("`{}` is not an integer constant", name.text()))
                        })?,
                    };
                    v |= found;
                }
                (v, names[0].span.clone())
            }
            ast::Constant::Bool(_, span) | ast::Constant::Str(_, span) => {
                return Err(span.error("expected an integer"));
            }
        };
        if v < min || v > max {
            return Err(span.error(format!("{v} does not fit in {}", int.name())));
        }
        Ok(v)
    }

    /// The value of the integer constant that `path` names from `scope`,
    /// or `None` when it names something else.
    fn const_int(&self, scope: &Scope, path: &ast::Path) -> Result<Option<i128>, Error> {
        let (full, decl, decl_scope) = self.lookup(scope, path)?;
        if !matches!(decl.kind, ast::DeclKind::Const { .. }) {
            return Ok(None);
        }
        if !self.expanding.borrow_mut().insert(full.clone()) {
            return Err(path
                .span
                .error(format!("constant `{full}` refers to itself")));
        }
        let kind = self.decl(decl, decl_scope);
        self.expanding.borrow_mut().remove(&full);
        Ok(match kind? {
            DeclKind::Const {
                value: Value::Int(v),
                ..
            } => Some(v),
            _ => None,
        })
    }
}
