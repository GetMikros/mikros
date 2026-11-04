//! Finding structures that contain themselves, which is not checked
//! yet.

use super::Scope;
use crate::Error;
use crate::ast;
use crate::model::*;
use std::collections::HashMap;

pub(super) fn no_self_containment(
    _model: &Model,
    _decls: &HashMap<String, (&ast::Decl, Scope)>,
) -> Result<(), Error> {
    Ok(())
}
