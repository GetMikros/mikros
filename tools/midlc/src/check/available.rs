//! Which elements exist at a version. Every element exists at every
//! version until `@available` is checked.

use super::Scope;
use crate::ast;
use crate::{Error, Span};

impl Scope {
    pub(super) fn includes(
        &self,
        _attrs: &ast::Attributes,
        _span: &Span,
        _version: Option<u64>,
    ) -> Result<bool, Error> {
        let Window = self.window;
        Ok(true)
    }
}

/// The versions at which an element exists.
#[derive(Clone, Copy)]
pub(super) struct Window;

pub(super) fn window(
    _attrs: &ast::Attributes,
    _outer: Option<Window>,
    _span: &Span,
) -> Result<Window, Error> {
    Ok(Window)
}
