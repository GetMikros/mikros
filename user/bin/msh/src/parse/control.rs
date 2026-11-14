//! Keywords: `let`, `if`, `while`, `for`, `match` and its patterns,
//! `fn`, `param`, `try`, `use`, `return`, `delete` and `extern`.

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
                    value: self.right_side()?,
                }
            }
            "if" => {
                let condition = self.condition()?;
                let then = self.block()?;
                let save = self.pos;
                self.skip_blank();
                let otherwise = if self.at_keyword("else") {
                    self.pos += 4;
                    self.skip_spaces();
                    if self.at_keyword("if") {
                        let line = self.line_of(self.pos);
                        self.pos += 2;
                        self.skip_spaces();
                        let kind = self.keyword("if")?;
                        Some(Block {
                            statements: vec![Statement { line, kind }],
                        })
                    } else {
                        Some(self.block()?)
                    }
                } else {
                    self.pos = save;
                    None
                };
                StatementKind::If {
                    condition,
                    then,
                    otherwise,
                }
            }
            "while" => StatementKind::While {
                condition: self.condition()?,
                body: self.block()?,
            },
            "for" => {
                let name = self.name()?;
                self.skip_spaces();
                if !self.at_keyword("in") {
                    return Err(self.error("`in` is expected here"));
                }
                self.pos += 2;
                self.skip_spaces();
                let items = self.items()?;
                StatementKind::For {
                    name,
                    items,
                    body: self.block()?,
                }
            }
            "match" => {
                let value = self.operand()?;
                self.skip_spaces();
                self.expect('{', "`{`")?;
                StatementKind::Match {
                    value,
                    arms: self.arms()?,
                }
            }
            "fn" => StatementKind::Fn(self.function()?),
            "param" => StatementKind::Param(self.param()?),
            "try" => {
                let body = self.block()?;
                self.skip_blank();
                if !self.at_keyword("catch") {
                    return Err(self.error("`catch` is expected after the block of `try`"));
                }
                self.pos += 5;
                self.skip_spaces();
                let name = self.name()?;
                StatementKind::Try {
                    body,
                    name,
                    handler: self.block()?,
                }
            }
            "use" => StatementKind::Use(self.word()?),
            "return" => {
                self.skip_spaces();
                match self.peek() {
                    None | Some('\n' | '\r' | ';' | '}' | '#') => StatementKind::Return(None),
                    _ => StatementKind::Return(Some(self.right_side()?)),
                }
            }
            "delete" => match self.peek() {
                Some('$') => StatementKind::Delete(self.variable_path(true)?),
                _ if self.looking_at("env.") => {
                    self.pos += 4;
                    StatementKind::Delete(Variable {
                        environment: true,
                        name: self.name()?,
                        path: Vec::new(),
                        spread: false,
                    })
                }
                None => return Err(self.incomplete("what to delete is missing")),
                _ => return Err(self.error("`delete` takes `$name` or `env.NAME`")),
            },
            "extern" => StatementKind::Extern(self.extern_()?),
            _ => unreachable!("not a keyword"),
        })
    }

    /// The condition of `if` or `while`, up to its block.
    fn condition(&mut self) -> Result<Condition> {
        if self.at_expression() {
            let expr = self.expression()?;
            return Ok(Condition::Expr(expr));
        }
        Ok(Condition::Command(self.pipeline(true)?))
    }

    /// The items of `for`: an expression, or words up to the block.
    fn items(&mut self) -> Result<Expr> {
        if self.at_expression() {
            return self.expression();
        }
        let mut words = Vec::new();
        loop {
            self.skip_spaces();
            match self.peek() {
                Some('{') => break,
                None | Some('\n' | '\r' | ';') => {
                    return Err(self.incomplete("the loop's block is missing"));
                }
                _ => words.push(self.word()?),
            }
        }
        Ok(Expr::Words(words))
    }

    /// The arms of `match`, up to its `}`.
    fn arms(&mut self) -> Result<Vec<Arm>> {
        let mut arms = Vec::new();
        loop {
            self.skip_separators();
            match self.peek() {
                None => return Err(self.incomplete("`}` is missing")),
                Some('}') => {
                    self.pos += 1;
                    return Ok(arms);
                }
                _ => {}
            }
            let pattern = self.pattern()?;
            self.skip_spaces();
            let guard = if self.at_keyword("if") {
                self.pos += 2;
                self.skip_spaces();
                Some(self.expression()?)
            } else {
                None
            };
            self.skip_spaces();
            if !self.looking_at("=>") {
                return Err(self.error("`=>` is expected here"));
            }
            self.pos += 2;
            let body = self.block()?;
            arms.push(Arm {
                pattern,
                guard,
                body,
            });
        }
    }

    /// A pattern, with alternatives joined by `|`.
    fn pattern(&mut self) -> Result<Pattern> {
        let mut either = vec![self.single_pattern()?];
        loop {
            self.skip_spaces();
            if self.peek() == Some('|') {
                self.pos += 1;
                self.skip_spaces();
                either.push(self.single_pattern()?);
            } else {
                break;
            }
        }
        Ok(match either.len() {
            1 => either.pop().expect("one pattern"),
            _ => Pattern::Either(either),
        })
    }

    fn single_pattern(&mut self) -> Result<Pattern> {
        match self.peek() {
            Some('[') => {
                self.pos += 1;
                let mut items = Vec::new();
                let mut rest = None;
                loop {
                    self.skip_blank();
                    match self.peek() {
                        None => return Err(self.incomplete("`]` is missing")),
                        Some(']') => {
                            self.pos += 1;
                            break;
                        }
                        _ if self.looking_at("...") => {
                            self.pos += 3;
                            rest = Some(self.name()?);
                        }
                        _ => items.push(self.pattern()?),
                    }
                }
                Ok(Pattern::List { items, rest })
            }
            Some('{') => {
                self.pos += 1;
                let mut fields = Vec::new();
                loop {
                    self.skip_blank();
                    match self.peek() {
                        None => return Err(self.incomplete("`}` is missing")),
                        Some('}') => {
                            self.pos += 1;
                            break;
                        }
                        Some(',') => self.pos += 1,
                        _ => {
                            let key = self.key()?;
                            self.skip_spaces();
                            self.expect(':', "`:`")?;
                            self.skip_spaces();
                            fields.push((key, self.pattern()?));
                        }
                    }
                }
                Ok(Pattern::Record(fields))
            }
            _ if self.at_keyword("_") => {
                self.pos += 1;
                Ok(Pattern::Any)
            }
            Some(c)
                if (c.is_ascii_alphabetic() || c == '_')
                    && !["true", "false", "null"].iter().any(|k| self.at_keyword(k)) =>
            {
                Ok(Pattern::Bind(self.name()?))
            }
            _ => {
                let low = self.literal_pattern()?;
                if self.looking_at("..") {
                    self.pos += 2;
                    let high = self.literal_pattern()?;
                    return Ok(Pattern::Range(low, high));
                }
                Ok(Pattern::Literal(low))
            }
        }
    }

    /// A literal in a pattern: a number, size, duration, date, string,
    /// boolean or `null`, perhaps negative.
    fn literal_pattern(&mut self) -> Result<Literal> {
        match self.operand()? {
            Expr::Literal(literal) => Ok(literal),
            Expr::String(parts) => match parts.as_slice() {
                [] => Ok(Literal::String(String::new())),
                [StringPart::Text(text)] => Ok(Literal::String(text.clone())),
                _ => Err(self.error("a pattern's string cannot interpolate")),
            },
            Expr::Unary(Unary::Negate, value) => match *value {
                Expr::Literal(Literal::Int(n)) => Ok(Literal::Int(-n)),
                Expr::Literal(Literal::Float(n)) => Ok(Literal::Float(-n)),
                _ => Err(self.error("only a number can be negative in a pattern")),
            },
            _ => Err(self.error("a pattern is expected here")),
        }
    }

    /// `fn name(arg, --flag = default) { }`, after `fn`.
    fn function(&mut self) -> Result<Function> {
        let name = self.name()?;
        self.skip_spaces();
        self.expect('(', "`(`")?;
        let mut arguments = Vec::new();
        let mut flags = Vec::new();
        let mut types = Vec::new();
        loop {
            self.skip_blank();
            match self.peek() {
                None => return Err(self.incomplete("`)` is missing")),
                Some(')') => {
                    self.pos += 1;
                    break;
                }
                Some(',') => self.pos += 1,
                _ if self.looking_at("--") => {
                    self.pos += 2;
                    let flag = self.name()?;
                    if let Some(kind) = self.written_type()? {
                        types.push((format!("--{flag}"), kind));
                    }
                    self.skip_spaces();
                    let default = if self.peek() == Some('=') {
                        self.pos += 1;
                        self.skip_spaces();
                        Some(self.expression()?)
                    } else {
                        None
                    };
                    flags.push((flag, default));
                }
                _ => {
                    let argument = self.name()?;
                    if let Some(kind) = self.written_type()? {
                        types.push((argument.clone(), kind));
                    }
                    arguments.push(argument);
                }
            }
        }
        Ok(Function {
            name,
            arguments,
            flags,
            types,
            body: self.block()?,
        })
    }

    /// `: type` after a function's argument or flag, if it has one.
    fn written_type(&mut self) -> Result<Option<ParamType>> {
        self.skip_spaces();
        if self.peek() != Some(':') {
            return Ok(None);
        }
        self.pos += 1;
        self.skip_spaces();
        self.param_type().map(Some)
    }

    /// `name(…)` or `name sub(…)`, after `extern`: the items in the
    /// parentheses, separated by commas, each with a description in
    /// double quotes after it if it has one.
    fn extern_(&mut self) -> Result<Extern> {
        let mut declared = Extern {
            command: self.name()?,
            ..Extern::default()
        };
        self.skip_spaces();
        if self.peek() != Some('(') {
            declared.subcommand = Some(self.name()?);
            self.skip_spaces();
        }
        self.expect('(', "`(`")?;
        loop {
            self.skip_blank();
            match self.peek() {
                None => return Err(self.incomplete("`)` is missing")),
                Some(')') => {
                    self.pos += 1;
                    return Ok(declared);
                }
                Some(',') => self.pos += 1,
                _ if self.looking_at("--") => {
                    self.pos += 2;
                    let name = self.name()?;
                    let short = match self.peek() {
                        Some('(') => {
                            self.pos += 1;
                            self.expect('-', "`-` and the short form")?;
                            let short = self.name()?;
                            self.expect(')', "`)` after the short form")?;
                            Some(short)
                        }
                        _ => None,
                    };
                    let kind = self.item_type()?;
                    declared.options.push(Declared {
                        name,
                        short,
                        kind,
                        description: self.description()?,
                    });
                }
                _ => {
                    let rest = self.looking_at("...");
                    if rest {
                        self.pos += 3;
                    }
                    let label = self.name()?;
                    let optional = self.peek() == Some('?');
                    if optional {
                        self.pos += 1;
                    }
                    let kind = self.item_type()?;
                    declared.positional.push(Positional {
                        label,
                        kind,
                        optional,
                        rest,
                        description: self.description()?,
                    });
                }
            }
        }
    }

    /// `: type` after the label or name of a declared item.
    fn item_type(&mut self) -> Result<ParamType> {
        self.skip_spaces();
        self.expect(':', "`:` and a type")?;
        self.skip_spaces();
        self.param_type()
    }

    /// The description in double quotes after a declared item, if any.
    fn description(&mut self) -> Result<Option<String>> {
        self.skip_spaces();
        match self.peek() {
            Some('"') => Ok(Some(self.quoted_text("a description")?)),
            _ => Ok(None),
        }
    }

    /// A type: a name such as `int` or `folder`, or the choices `[a b c]`.
    pub(super) fn param_type(&mut self) -> Result<ParamType> {
        if self.peek() == Some('[') {
            self.pos += 1;
            let mut choices = Vec::new();
            loop {
                self.skip_spaces();
                match self.peek() {
                    Some(']') => {
                        self.pos += 1;
                        break;
                    }
                    None | Some('\n') => return Err(self.incomplete("`]` is missing")),
                    _ => {
                        let word = self.take_while(|c| {
                            !c.is_whitespace() && !matches!(c, ']' | ',' | ')' | '"')
                        });
                        if word.is_empty() {
                            return Err(self.error("a choice is expected here"));
                        }
                        choices.push(word);
                    }
                }
            }
            if choices.is_empty() {
                return Err(self.error("a list of choices needs one at least"));
            }
            return Ok(ParamType::Choices(choices));
        }
        let at = self.pos;
        Ok(match self.name()?.as_str() {
            "string" => ParamType::String,
            "int" => ParamType::Int,
            "float" => ParamType::Float,
            "bool" => ParamType::Bool,
            "size" => ParamType::Size,
            "duration" => ParamType::Duration,
            "date" => ParamType::Date,
            "path" => ParamType::Path,
            "folder" => ParamType::Folder,
            "file" => ParamType::File,
            "command" => ParamType::Command,
            "list" => ParamType::List,
            "any" => ParamType::Any,
            other => return Err(self.error_at(at, &format!("`{other}` is not a type"))),
        })
    }

    /// `param --name: type = default`, after `param`.
    fn param(&mut self) -> Result<Param> {
        if !self.looking_at("--") {
            return Err(self.error("`--name` is expected here"));
        }
        self.pos += 2;
        let name = self.name()?;
        self.skip_spaces();
        self.expect(':', "`:` and a type")?;
        self.skip_spaces();
        let kind = self.param_type()?;
        self.skip_spaces();
        let default = if self.peek() == Some('=') {
            self.pos += 1;
            self.skip_spaces();
            Some(self.expression()?)
        } else {
            None
        };
        Ok(Param {
            name,
            kind,
            default,
        })
    }
}
