//! Rejecting methods whose largest message does not fit a channel.

use super::Scope;
use crate::Error;
use crate::ast;
use crate::layout::Layout;
use crate::model::*;
use std::collections::HashMap;

pub(super) fn messages_fit(
    model: &Model,
    decls: &HashMap<String, (&ast::Decl, Scope)>,
) -> Result<(), Error> {
    let layout = Layout { model };
    for library in &model.libraries {
        for decl in &library.decls {
            let DeclKind::Protocol(methods) = &decl.kind else {
                continue;
            };
            let full = format!("{}.{}", library.name, decl.name);
            let ast::DeclKind::Protocol(written) = &decls[&full].0.kind else {
                unreachable!("a protocol stays a protocol");
            };
            for method in methods {
                let messages = match &method.kind {
                    MethodKind::TwoWay {
                        request,
                        response,
                        error,
                    } => vec![
                        layout.message_max(request),
                        match error {
                            Some(error) => layout.result_max(response, error),
                            None => layout.message_max(response),
                        },
                    ],
                    MethodKind::OneWay(args) | MethodKind::Event(args) => {
                        vec![layout.message_max(args)]
                    }
                };
                if !messages.into_iter().all(Layout::fits) {
                    let span = &written
                        .iter()
                        .find(|m| m.name == method.name)
                        .expect("a checked method was written")
                        .span;
                    return Err(span.error(format!(
                        "a message of `{}` can exceed 65536 bytes or 64 handles",
                        method.name
                    )));
                }
            }
        }
    }
    Ok(())
}
