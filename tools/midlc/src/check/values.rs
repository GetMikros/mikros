//! Checking constants and the values of enumeration and bit set
//! members.

use super::{Checker, Scope};
use crate::ast::{self, Param};
use crate::model::*;
use crate::{Error, Span};

impl Checker<'_> {
    /// The checked form of a constant, enumeration or bit set.
    pub(super) fn value_decl(&self, decl: &ast::Decl, _scope: &Scope) -> Result<DeclKind, Error> {
        Err(decl
            .span
            .error("constants, enumerations and bit sets are not checked yet"))
    }

    /// A length or count, which is an integer.
    pub(super) fn count(&self, _scope: &Scope, p: &Param, span: &Span) -> Result<u64, Error> {
        match p {
            Param::Int(n, _) => Ok(*n),
            _ => Err(span.error("expected an integer count")),
        }
    }
}
