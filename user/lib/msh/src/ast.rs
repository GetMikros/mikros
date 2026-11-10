//! The syntax tree of msh's command language.

/// A script, or a line typed at the prompt: statements in order.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub statements: Vec<Statement>,
}

/// A statement, with the line it starts on.
#[derive(Clone, Debug, PartialEq)]
pub struct Statement {
    pub line: usize,
    pub kind: StatementKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StatementKind {
    /// `let name = value`.
    Let {
        name: String,
        value: Expr,
    },
    /// `name = value`, `name += value`, `env.NAME = value`.
    Assign {
        target: Target,
        append: bool,
        value: Expr,
    },
    /// A pipeline of commands, perhaps run in the background.
    Pipeline(Pipeline),
    /// An expression in parentheses at the start of a line.
    Expr(Expr),
    If {
        condition: Condition,
        then: Block,
        otherwise: Option<Block>,
    },
    While {
        condition: Condition,
        body: Block,
    },
    For {
        name: String,
        items: Expr,
        body: Block,
    },
    Match {
        value: Expr,
        arms: Vec<Arm>,
    },
    Fn(Function),
    Param(Param),
    Try {
        body: Block,
        name: String,
        handler: Block,
    },
    Use(Word),
    Return(Option<Expr>),
}

/// What an assignment changes.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Variable(String),
    Environment(String),
}

/// The condition of `if` and `while`.
#[derive(Clone, Debug, PartialEq)]
pub enum Condition {
    Expr(Expr),
    Command(Pipeline),
}

/// Commands joined by `|`.
#[derive(Clone, Debug, PartialEq)]
pub struct Pipeline {
    pub commands: Vec<Command>,
    /// Started with `&`.
    pub background: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// A program, function or built-in command with its arguments.
    Simple(Simple),
    /// An expression in parentheses as a pipeline's command.
    Expr(Expr),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Simple {
    /// The `NAME=value` words before the command.
    pub environment: Vec<(String, Word)>,
    /// The command's name and arguments.
    pub words: Vec<Argument>,
    pub redirections: Vec<Redirection>,
}

/// An argument: a word, or a block such as `{ $it + 1 }`.
#[derive(Clone, Debug, PartialEq)]
pub enum Argument {
    Word(Word),
    Block(Block),
}

/// A word: its parts, joined into one argument, unless it is a single
/// `$list...`, which gives one argument for each item.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub parts: Vec<WordPart>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WordPart {
    /// Text as written outside quotes, where glob patterns count.
    Bare(String),
    /// Text from quotes or escapes, where they do not.
    Quoted(String),
    /// `$name`, `$name.field.0`, or `$name...`.
    Variable(Variable),
    /// A double-quoted string's `{expr}`, or `(expr)` in a word.
    Expr(Expr),
}

/// A variable with the fields and items read from it.
#[derive(Clone, Debug, PartialEq)]
pub struct Variable {
    /// `env.NAME` reads an environment variable.
    pub environment: bool,
    pub name: String,
    pub path: Vec<Access>,
    /// `$list...`.
    pub spread: bool,
}

/// `.name` or `.0`.
#[derive(Clone, Debug, PartialEq)]
pub enum Access {
    Field(String),
    Item(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Redirection {
    pub kind: RedirectionKind,
    pub target: Word,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedirectionKind {
    /// `<`
    Input,
    /// `>`
    Output,
    /// `>>`
    Append,
    /// `2>`
    Error,
    /// `2>&1`, whose target is empty.
    ErrorToOutput,
    /// `>&2`, whose target is empty.
    OutputToError,
}

/// An expression.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Literal(Literal),
    /// A double-quoted string, with its interpolations.
    String(Vec<StringPart>),
    Variable(Variable),
    List(Vec<Expr>),
    Record(Vec<(String, Expr)>),
    Unary(Unary, Box<Expr>),
    Binary(Binary, Box<Expr>, Box<Expr>),
    Access(Box<Expr>, Access),
    /// A pipeline in parentheses, whose value or output it gives.
    Pipeline(Box<Pipeline>),
    /// A block, as an argument of `each` or `where`.
    Block(Block),
    /// Words, as the items of `for`, each a string after expansion.
    Words(Vec<Word>),
    /// `(command)?` or `(expr)?`, whose error returns to the caller of
    /// the function it is in.
    Propagate(Box<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum StringPart {
    Text(String),
    Expr(Expr),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Literal {
    Int(i64),
    Float(f64),
    /// In bytes.
    Size(i64),
    /// In nanoseconds.
    Duration(i64),
    Date(Date),
    String(String),
    Bool(bool),
    Null,
}

/// A date: seconds and nanoseconds since 1970-01-01T00:00:00Z, and the
/// offset it was written with, in seconds, or `None` for the system's
/// time zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Date {
    /// For a date without an offset, the local time as if it were UTC.
    pub seconds: i64,
    pub nanos: u32,
    pub offset: Option<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unary {
    Negate,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binary {
    Multiply,
    Divide,
    Remainder,
    Add,
    Subtract,
    Default,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Matches,
    NotMatches,
    And,
    Or,
}

/// An arm of `match`.
#[derive(Clone, Debug, PartialEq)]
pub struct Arm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Pattern {
    Literal(Literal),
    Any,
    Bind(String),
    Range(Literal, Literal),
    List {
        items: Vec<Pattern>,
        rest: Option<String>,
    },
    Record(Vec<(String, Pattern)>),
    Either(Vec<Pattern>),
}

/// `fn name(arg, --flag = default) { }`.
#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    pub name: String,
    pub arguments: Vec<String>,
    pub flags: Vec<(String, Option<Expr>)>,
    pub body: Block,
}

/// `param --name: type = default`.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub kind: ParamType,
    pub default: Option<Expr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamType {
    String,
    Int,
    Float,
    Bool,
    Size,
    Duration,
    Date,
    Path,
    List,
    Any,
}
