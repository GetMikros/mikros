//! Keywords: `let` and `return`.

use super::{Parser, Result};
use crate::ast::*;

impl Parser<'_> {
    pub(super) fn keyword(&mut self, keyword: &str) -> Result<StatementKind> {
        Ok(match keyword {
            "let" => {
                let name = self.name()?;
                self.skip_spaces();
                self.expect('=', "`=`")?;
                self.skip_spaces();
                StatementKind::Let {
                    name,
                    value: self.expression()?,
                }
            }
            "return" => {
                self.skip_spaces();
                match self.peek() {
                    None | Some('\n' | '\r' | ';' | '}' | '#') => StatementKind::Return(None),
                    _ => StatementKind::Return(Some(self.expression()?)),
                }
            }
            _ => unreachable!("not a keyword"),
        })
    }
}
