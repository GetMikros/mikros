//! Parses the tokens of one MIDL file into its syntax tree.

use crate::ast::*;
use crate::lexer::Token;
use crate::{Error, Span};

/// The syntax tree of the file, or its first error.
pub fn parse(tokens: Vec<(Token, Span)>, end: Span) -> Result<File, Error> {
    let (file, mut errors) = parse_all(tokens, end);
    match file {
        Some(file) if errors.is_empty() => Ok(file),
        _ => Err(errors.remove(0)),
    }
}

/// The syntax tree of the file, without the declarations that have
/// errors, and every error. After an error in a declaration, parsing
/// goes on after the `;` that ends it. The tree is `None` when the
/// `library` declaration cannot be parsed.
pub fn parse_all(tokens: Vec<(Token, Span)>, end: Span) -> (Option<File>, Vec<Error>) {
    let mut p = Parser { tokens, at: 0, end };
    let mut errors = Vec::new();
    let header = (|| {
        let attrs = p.attributes()?;
        p.keyword("library")?;
        let library = p.path()?;
        p.punct(';')?;
        Ok((attrs, library))
    })();
    let (attrs, library) = match header {
        Ok(header) => header,
        Err(e) => return (None, vec![e]),
    };
    let mut usings = Vec::new();
    while p.peek_ident("using") {
        let start = p.at;
        p.at += 1;
        match p.path().and_then(|path| p.punct(';').map(|()| path)) {
            Ok(path) => usings.push(path),
            Err(e) => {
                errors.push(e);
                p.skip_from(start);
            }
        }
    }
    let mut decls = Vec::new();
    while p.at < p.tokens.len() {
        let start = p.at;
        match p.decl() {
            Ok(decl) => decls.push(decl),
            Err(e) => {
                errors.push(e);
                p.skip_from(start);
            }
        }
    }
    let file = File {
        attrs,
        library,
        usings,
        decls,
    };
    (Some(file), errors)
}

/// The words that start a declaration.
const DECLARATION_WORDS: [&str; 5] = ["const", "alias", "type", "protocol", "using"];

struct Parser {
    tokens: Vec<(Token, Span)>,
    at: usize,
    /// Where the file ends, for errors at its end.
    end: Span,
}

impl Parser {
    /// After an error in the statement that starts at `start`, moves to
    /// just after the `;` that ends it, or to the start of a line with
    /// a declaration, a documentation comment or an attribute after
    /// the error, whichever comes first.
    fn skip_from(&mut self, start: usize) {
        let failed = self.at.max(start + 1);
        let mut depth = 0i32;
        self.at = start;
        while let Some((token, span)) = self.tokens.get(self.at) {
            let after = self.at >= failed;
            match token {
                Token::Punct('{' | '(' | '<') => depth += 1,
                Token::Punct('}' | ')' | '>') => depth -= 1,
                Token::Punct(';') if after && depth <= 0 => {
                    self.at += 1;
                    return;
                }
                Token::Ident(word)
                    if after && span.col == 1 && DECLARATION_WORDS.contains(&word.as_str()) =>
                {
                    return;
                }
                Token::Doc(_) | Token::Punct('@') if after && span.col == 1 => return,
                _ => {}
            }
            self.at += 1;
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at).map(|(t, _)| t)
    }

    fn peek_at(&self, ahead: usize) -> Option<&Token> {
        self.tokens.get(self.at + ahead).map(|(t, _)| t)
    }

    fn span(&self) -> Span {
        self.tokens
            .get(self.at)
            .map_or_else(|| self.end.clone(), |(_, s)| s.clone())
    }

    fn fail<T>(&self, what: &str) -> Result<T, Error> {
        let found = match self.peek() {
            None => "the end of the file".to_string(),
            Some(Token::Ident(s)) => format!("`{s}`"),
            Some(Token::Int(n)) => format!("`{n}`"),
            Some(Token::Str(_)) => "a string".to_string(),
            Some(Token::Doc(_)) => "a documentation comment".to_string(),
            Some(Token::Arrow) => "`->`".to_string(),
            Some(Token::Punct(c)) => format!("`{c}`"),
        };
        Err(self.span().error(format!("expected {what}, found {found}")))
    }

    fn is_punct(&self, c: char) -> bool {
        self.peek() == Some(&Token::Punct(c))
    }

    fn eat_punct(&mut self, c: char) -> bool {
        let found = self.is_punct(c);
        self.at += usize::from(found);
        found
    }

    fn punct(&mut self, c: char) -> Result<(), Error> {
        if self.eat_punct(c) {
            Ok(())
        } else {
            self.fail(&format!("`{c}`"))
        }
    }

    fn peek_ident(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Token::Ident(s)) if s == word)
    }

    fn keyword(&mut self, word: &str) -> Result<(), Error> {
        if self.peek_ident(word) {
            self.at += 1;
            Ok(())
        } else {
            self.fail(&format!("`{word}`"))
        }
    }

    fn ident(&mut self) -> Result<(String, Span), Error> {
        match self.peek() {
            Some(Token::Ident(s)) => {
                let found = (s.clone(), self.span());
                self.at += 1;
                Ok(found)
            }
            _ => self.fail("an identifier"),
        }
    }

    fn int(&mut self) -> Result<(u64, Span), Error> {
        match self.peek() {
            Some(&Token::Int(n)) => {
                let span = self.span();
                self.at += 1;
                Ok((n, span))
            }
            _ => self.fail("an integer"),
        }
    }

    fn path(&mut self) -> Result<Path, Error> {
        let (first, span) = self.ident()?;
        let mut end = span.after(&first);
        let mut parts = vec![first];
        while self.eat_punct('.') {
            let (part, at) = self.ident()?;
            end = at.after(&part);
            parts.push(part);
        }
        Ok(Path { parts, span, end })
    }

    /// Documentation comments and `@available(...)`, but not a method
    /// ordinal such as `@1`.
    fn attributes(&mut self) -> Result<Attributes, Error> {
        let mut attrs = Attributes::default();
        loop {
            match self.peek() {
                Some(Token::Doc(line)) => {
                    attrs.doc.push(line.clone());
                    self.at += 1;
                }
                Some(Token::Punct('@')) if matches!(self.peek_at(1), Some(Token::Ident(_))) => {
                    self.at += 1;
                    let (name, span) = self.ident()?;
                    if name != "available" {
                        return Err(span.error(format!("unknown attribute `@{name}`")));
                    }
                    if attrs.available.is_some() {
                        return Err(span.error("`@available` given twice"));
                    }
                    attrs.available = Some(self.available()?);
                }
                _ => return Ok(attrs),
            }
        }
    }

    fn available(&mut self) -> Result<Available, Error> {
        let mut available = Available::default();
        self.punct('(')?;
        loop {
            let (key, span) = self.ident()?;
            self.punct('=')?;
            let (value, _) = self.int()?;
            let slot = match key.as_str() {
                "added" => &mut available.added,
                "deprecated" => &mut available.deprecated,
                "removed" => &mut available.removed,
                _ => return Err(span.error(format!("unknown `@available` argument `{key}`"))),
            };
            if slot.replace(value).is_some() {
                return Err(span.error(format!("`{key}` given twice")));
            }
            if !self.eat_punct(',') {
                break;
            }
        }
        self.punct(')')?;
        Ok(available)
    }

    fn strictness(&mut self) -> Option<Strictness> {
        let found = match self.peek() {
            Some(Token::Ident(s)) if s == "strict" => Strictness::Strict,
            Some(Token::Ident(s)) if s == "flexible" => Strictness::Flexible,
            _ => return None,
        };
        self.at += 1;
        Some(found)
    }

    fn decl(&mut self) -> Result<Decl, Error> {
        let attrs = self.attributes()?;
        let (word, _) = self.ident()?;
        let (name, span) = self.ident()?;
        let kind = match word.as_str() {
            "const" => {
                let ty = self.type_ref()?;
                self.punct('=')?;
                DeclKind::Const {
                    ty,
                    value: self.constant()?,
                }
            }
            "alias" => {
                self.punct('=')?;
                DeclKind::Alias(self.type_ref()?)
            }
            "type" => {
                self.punct('=')?;
                self.type_body()?
            }
            "protocol" => DeclKind::Protocol(self.methods()?),
            _ => {
                self.at -= 2;
                return self.fail("a declaration");
            }
        };
        self.punct(';')?;
        Ok(Decl {
            attrs,
            name,
            span,
            kind,
        })
    }

    fn type_body(&mut self) -> Result<DeclKind, Error> {
        let modifier = self.strictness();
        let (kind, span) = self.ident()?;
        let strictness = modifier.unwrap_or(Strictness::Flexible);
        let body = match kind.as_str() {
            "struct" if modifier.is_none() => DeclKind::Struct(self.braced(Self::struct_member)?),
            "enum" | "bits" => {
                self.punct(':')?;
                let underlying = self.type_ref()?;
                let members = self.braced(Self::value_member)?;
                match kind.as_str() {
                    "enum" => DeclKind::Enum {
                        strictness,
                        underlying,
                        members,
                    },
                    _ => DeclKind::Bits {
                        strictness,
                        underlying,
                        members,
                    },
                }
            }
            "union" => DeclKind::Union {
                strictness,
                members: self.braced(Self::ordinal_member)?,
            },
            "table" if modifier.is_none() => DeclKind::Table(self.braced(Self::ordinal_member)?),
            "struct" | "table" => {
                return Err(span.error(format!("`{kind}` takes no strict or flexible modifier")));
            }
            _ => return Err(span.error(format!("unknown type kind `{kind}`"))),
        };
        Ok(body)
    }

    /// `{ item; item; ... }`, each item ending with `;`.
    fn braced<T>(&mut self, item: fn(&mut Self) -> Result<T, Error>) -> Result<Vec<T>, Error> {
        self.punct('{')?;
        let mut items = Vec::new();
        while !self.eat_punct('}') {
            items.push(item(self)?);
            self.punct(';')?;
        }
        Ok(items)
    }

    fn struct_member(&mut self) -> Result<Member, Error> {
        let attrs = self.attributes()?;
        let (name, span) = self.ident()?;
        Ok(Member {
            attrs,
            name,
            span,
            ty: self.type_ref()?,
        })
    }

    fn value_member(&mut self) -> Result<ValueMember, Error> {
        let attrs = self.attributes()?;
        let (name, span) = self.ident()?;
        self.punct('=')?;
        Ok(ValueMember {
            attrs,
            name,
            span,
            value: self.constant()?,
        })
    }

    fn ordinal_member(&mut self) -> Result<OrdinalMember, Error> {
        let attrs = self.attributes()?;
        let (ordinal, _) = self.int()?;
        self.punct(':')?;
        let (name, span) = self.ident()?;
        Ok(OrdinalMember {
            attrs,
            ordinal,
            name,
            span,
            ty: self.type_ref()?,
        })
    }

    fn methods(&mut self) -> Result<Vec<Method>, Error> {
        self.punct('{')?;
        let mut methods = Vec::new();
        while !self.eat_punct('}') {
            methods.push(self.method()?);
            self.punct(';')?;
        }
        Ok(methods)
    }

    /// A method or event. The strictness modifier may come before or
    /// after the ordinal.
    fn method(&mut self) -> Result<Method, Error> {
        let attrs = self.attributes()?;
        let before = self.strictness();
        self.punct('@')?;
        let (ordinal, _) = self.int()?;
        let after = self.strictness();
        if before.is_some() && after.is_some() {
            return self.fail("one strict or flexible modifier");
        }
        let strictness = before.or(after).unwrap_or(Strictness::Flexible);
        let event = self.peek() == Some(&Token::Arrow);
        self.at += usize::from(event);
        let (name, span) = self.ident()?;
        let arguments = self.parameters()?;
        let kind = if event {
            MethodKind::Event(arguments)
        } else if self.peek() == Some(&Token::Arrow) {
            self.at += 1;
            let response = self.parameters()?;
            let error = if self.peek_ident("error") {
                self.at += 1;
                Some(self.type_ref()?)
            } else {
                None
            };
            MethodKind::TwoWay {
                request: arguments,
                response,
                error,
            }
        } else {
            MethodKind::OneWay(arguments)
        };
        Ok(Method {
            attrs,
            strictness,
            ordinal,
            name,
            span,
            kind,
        })
    }

    /// `(name type, name type)`, with an optional trailing comma.
    fn parameters(&mut self) -> Result<Vec<Member>, Error> {
        self.punct('(')?;
        let mut members = Vec::new();
        while !self.eat_punct(')') {
            members.push(self.struct_member()?);
            if !self.eat_punct(',') {
                self.punct(')')?;
                break;
            }
        }
        Ok(members)
    }

    fn type_ref(&mut self) -> Result<TypeRef, Error> {
        let name = self.path()?;
        let mut params = Vec::new();
        if self.eat_punct('<') {
            loop {
                params.push(self.param()?);
                if !self.eat_punct(',') {
                    break;
                }
            }
            self.punct('>')?;
        }
        let optional = self.eat_punct('?');
        Ok(TypeRef {
            name,
            params,
            optional,
        })
    }

    fn param(&mut self) -> Result<Param, Error> {
        if let Some(&Token::Int(n)) = self.peek() {
            let span = self.span();
            self.at += 1;
            return Ok(Param::Int(n, span));
        }
        let first = self.type_ref()?;
        if !self.is_punct('|') {
            return Ok(Param::Type(first));
        }
        if !first.params.is_empty() || first.optional {
            return self.fail("a name");
        }
        let mut names = vec![first.name];
        while self.eat_punct('|') {
            names.push(self.path()?);
        }
        Ok(Param::Union(names))
    }

    fn constant(&mut self) -> Result<Constant, Error> {
        let span = self.span();
        match self.peek().cloned() {
            Some(Token::Punct('-')) => {
                self.at += 1;
                let (value, _) = self.int()?;
                Ok(Constant::Int {
                    value,
                    negative: true,
                    span,
                })
            }
            Some(Token::Int(value)) => {
                self.at += 1;
                Ok(Constant::Int {
                    value,
                    negative: false,
                    span,
                })
            }
            Some(Token::Str(s)) => {
                self.at += 1;
                Ok(Constant::Str(s, span))
            }
            Some(Token::Ident(s)) if s == "true" || s == "false" => {
                self.at += 1;
                Ok(Constant::Bool(s == "true", span))
            }
            Some(Token::Ident(_)) => {
                let mut names = vec![self.path()?];
                while self.eat_punct('|') {
                    names.push(self.path()?);
                }
                Ok(Constant::Names(names))
            }
            _ => self.fail("a constant"),
        }
    }
}
