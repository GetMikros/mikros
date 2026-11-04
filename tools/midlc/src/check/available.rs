//! Which elements exist at a version, from their `@available`
//! attributes.

use super::Scope;
use crate::ast::{self, Available};
use crate::{Error, Span};

impl Scope {
    pub(super) fn includes(
        &self,
        attrs: &ast::Attributes,
        span: &Span,
        version: Option<u64>,
    ) -> Result<bool, Error> {
        Ok(window(attrs, Some(self.window), span)?.contains(version))
    }
}

/// The versions from `added` up to, but not including, `removed`.
#[derive(Clone, Copy)]
pub(super) struct Window {
    added: u64,
    removed: u64,
}

impl Window {
    pub(super) fn contains(self, version: Option<u64>) -> bool {
        version.is_none_or(|v| self.added <= v && v < self.removed)
    }
}

/// The window of an element with `attrs` inside `outer`.
pub(super) fn window(
    attrs: &ast::Attributes,
    outer: Option<Window>,
    span: &Span,
) -> Result<Window, Error> {
    let outer = outer.unwrap_or(Window {
        added: 0,
        removed: u64::MAX,
    });
    let Some(Available {
        added,
        deprecated,
        removed,
    }) = attrs.available
    else {
        return Ok(outer);
    };
    let added = added.unwrap_or(outer.added).max(outer.added);
    let removed = removed.unwrap_or(outer.removed).min(outer.removed);
    let order_ok = removed > added && deprecated.is_none_or(|d| added <= d && d < removed);
    if !order_ok {
        return Err(span.error("`@available` versions must satisfy added <= deprecated < removed"));
    }
    Ok(Window { added, removed })
}
