use std::fmt;
use std::ops::Range;

use serde::{Serialize, Serializer};

pub type Span = Range<usize>;

/// The complete binary operator vocabulary.
///
/// This is a closed enum rather than a string so the parser and the analyzer
/// cannot drift apart: every dispatch site is an exhaustive `match`, and an
/// operator added here fails to compile until each consumer handles it.
///
/// Variants are declared loosest-binding first; see
/// `Parser::infix_binding_power` for the authoritative precedence table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Or,
    And,
    Eq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Pow,
}

impl BinaryOp {
    /// The canonical source spelling. This is also the serialized form, so it
    /// must stay stable: `freql inspect --stage ast` and report predicates
    /// both round-trip through it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Or => "or",
            Self::And => "and",
            Self::Eq => "==",
            Self::NotEq => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Pow => "^",
        }
    }

    /// True for operators whose operands are `Bool` and whose result is `Bool`.
    pub const fn is_logical(self) -> bool {
        matches!(self, Self::And | Self::Or)
    }

    /// True for operators that compare two values and produce a `Bool`.
    pub const fn is_comparison(self) -> bool {
        matches!(
            self,
            Self::Eq | Self::NotEq | Self::Lt | Self::Le | Self::Gt | Self::Ge
        )
    }
}

impl fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for BinaryOp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// The complete prefix operator vocabulary.
///
/// `not` and `!` are the same operator; `!` is the canonical spelling used for
/// display and serialization so existing report output is unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Neg,
    Pos,
    Not,
}

impl UnaryOp {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Neg => "-",
            Self::Pos => "+",
            Self::Not => "!",
        }
    }
}

impl fmt::Display for UnaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for UnaryOp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Module {
    pub items: Vec<Item>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    Import(Import),
    Artifact(Artifact),
    Resource(Resource),
    Process(Process),
    Unit(UnitDecl),
    Function(Function),
    Trait(TraitDecl),
    Impl(ImplDecl),
}

impl Item {
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Import(_) => None,
            Self::Artifact(item) => Some(&item.name),
            Self::Resource(item) => Some(&item.name),
            Self::Process(item) => Some(&item.name),
            Self::Unit(item) => Some(&item.name),
            Self::Function(item) => Some(&item.name),
            Self::Trait(item) => Some(&item.name),
            Self::Impl(_) => None,
        }
    }

    pub fn span(&self) -> &Span {
        match self {
            Self::Import(item) => &item.span,
            Self::Artifact(item) => &item.span,
            Self::Resource(item) => &item.span,
            Self::Process(item) => &item.span,
            Self::Unit(item) => &item.span,
            Self::Function(item) => &item.span,
            Self::Trait(item) => &item.span,
            Self::Impl(item) => &item.span,
        }
    }
}

/// A reusable, capacity-constrained scheduling resource.
///
/// The capacity expression also establishes the dimension of claims. For
/// example, `12` creates a countable resource while `100 Meter` creates a
/// divisible length resource.
#[derive(Clone, Debug, Serialize)]
pub struct Resource {
    pub name: String,
    pub doc: Option<String>,
    pub capacity: Expr,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct Import {
    pub path: ImportPath,
    pub names: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ImportPath {
    Relative(String),
    Module(String),
}

#[derive(Clone, Debug, Serialize)]
pub struct Artifact {
    pub name: String,
    pub doc: Option<String>,
    pub fields: Vec<Field>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct Field {
    pub name: String,
    pub ty: TypeExpr,
    pub derived: Option<Expr>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberRole {
    Config,
    Gets,
    Uses,
    Export,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregateRule {
    Sum,
    Makespan,
    Max,
    Min,
    Explicit,
}

impl AggregateRule {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sum => "sum",
            Self::Makespan => "makespan",
            Self::Max => "max",
            Self::Min => "min",
            Self::Explicit => "explicit",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ProcessMember {
    pub role: MemberRole,
    /// Whether a configuration member is intentionally part of the
    /// design-facing parameter surface.
    pub exposed: bool,
    pub name: String,
    pub ty: TypeExpr,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Expr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregate: Option<AggregateRule>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct Monitor {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_unit: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub approximate: bool,
    /// Parsed form of the presentation unit. Reports retain the compact source
    /// spelling above, while analysis uses this expression to resolve compound
    /// units such as `Millimeter^2` and `Mile / Hour` exactly.
    #[serde(skip)]
    pub display_unit_expression: Option<Expr>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct Process {
    pub name: String,
    pub parents: Vec<String>,
    pub doc: Option<String>,
    pub members: Vec<ProcessMember>,
    pub monitors: Vec<Monitor>,
    pub parameters: Vec<Parameter>,
    pub body: Option<Block>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct UnitDecl {
    pub name: String,
    pub value: Expr,
    /// Numeric refinement predicates; the value is the binder form `Scalar(x)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refinement: Option<Block>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct Function {
    pub name: String,
    pub generics: Vec<String>,
    pub parameters: Vec<Parameter>,
    pub outputs: Vec<Parameter>,
    pub bounds: Vec<TypeBound>,
    pub body: Option<Block>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct TraitDecl {
    pub name: String,
    pub generics: Vec<String>,
    pub functions: Vec<Function>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImplDecl {
    pub trait_name: String,
    pub arguments: Vec<TypeExpr>,
    pub functions: Vec<Function>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct TypeBound {
    pub parameter: String,
    pub capability: TypeExpr,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct Parameter {
    pub name: String,
    pub ty: TypeExpr,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
pub struct TypeExpr {
    /// Structural copyable tuple fields; no nominal schema declaration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<Parameter>>,
    /// A dimension or concrete-unit expression used directly as a type, such
    /// as `Length / Time`.  The resolved `Type` is still structural; this
    /// expression merely preserves the spelling needed to resolve it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimension: Option<Expr>,
    pub name: String,
    pub arguments: Vec<TypeArgument>,
    pub arrays: Vec<ArraySize>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TypeArgument {
    Type(TypeExpr),
    Const(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ArraySize {
    Dynamic,
    Fixed(String),
}

impl std::fmt::Display for TypeExpr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(fields) = &self.fields {
            write!(
                f,
                "({})",
                fields
                    .iter()
                    .map(|field| format!("{}: {}", field.name, field.ty))
                    .collect::<Vec<_>>()
                    .join(", ")
            )?;
        } else if let Some(dimension) = &self.dimension {
            write!(f, "{}", dimension.display())?;
        } else {
            write!(f, "{}", self.name)?;
        }
        if !self.arguments.is_empty() {
            write!(f, "[")?;
            for (index, argument) in self.arguments.iter().enumerate() {
                if index > 0 {
                    write!(f, ", ")?;
                }
                match argument {
                    TypeArgument::Type(ty) => write!(f, "{ty}")?,
                    TypeArgument::Const(value) => write!(f, "{value}")?,
                }
            }
            write!(f, "]")?;
        }
        for array in &self.arrays {
            match array {
                ArraySize::Dynamic => write!(f, "[]")?,
                ArraySize::Fixed(size) => write!(f, "[{size}]")?,
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Block {
    pub statements: Vec<Stmt>,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Stmt {
    Let {
        mutable: bool,
        name: String,
        ty: Option<TypeExpr>,
        value: Expr,
        #[serde(skip)]
        span: Span,
    },
    Requires {
        value: Expr,
        #[serde(skip)]
        span: Span,
    },
    For {
        binding: String,
        iterable: Expr,
        body: Block,
        #[serde(skip)]
        span: Span,
    },
    /// Repeat a block sequentially a statically known number of times.
    /// Independent enclosing `for` iterations remain schedulable in parallel.
    Repeat {
        count: Expr,
        body: Block,
        #[serde(skip)]
        span: Span,
    },
    If {
        condition: Expr,
        then_branch: Block,
        else_branch: Option<Block>,
        #[serde(skip)]
        span: Span,
    },
    Yield {
        target: Expr,
        value: Expr,
        /// Earliest offset at which this stream can emit its first item.
        /// This is a scheduling declaration; it does not delay ordinary
        /// expression evaluation in the static analyzer.
        after: Option<Expr>,
        #[serde(skip)]
        span: Span,
    },
    Wire {
        source: Expr,
        target: Expr,
        #[serde(skip)]
        span: Span,
    },
    Before {
        first: Expr,
        second: Expr,
        #[serde(skip)]
        span: Span,
    },
    Assign {
        target: Expr,
        value: Expr,
        #[serde(skip)]
        span: Span,
    },
    Expr {
        value: Expr,
        #[serde(skip)]
        span: Span,
    },
}

impl Stmt {
    pub fn span(&self) -> &Span {
        match self {
            Self::Let { span, .. }
            | Self::Requires { span, .. }
            | Self::For { span, .. }
            | Self::Repeat { span, .. }
            | Self::If { span, .. }
            | Self::Yield { span, .. }
            | Self::Wire { span, .. }
            | Self::Before { span, .. }
            | Self::Assign { span, .. }
            | Self::Expr { span, .. } => span,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Expr {
    pub kind: ExprKind,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExprKind {
    /// An exact value with a compatible display unit and optional decimal output.
    Presentation {
        value: Box<Expr>,
        unit: Option<Box<Expr>>,
        approximate: bool,
    },
    Name {
        value: String,
    },
    Number {
        value: String,
        suffix: Option<String>,
    },
    String {
        value: String,
    },
    Bool {
        value: bool,
    },
    Array {
        items: Vec<Expr>,
    },
    NamedTuple {
        fields: Vec<CallArgument>,
    },
    Unary {
        op: UnaryOp,
        value: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Member {
        value: Box<Expr>,
        member: String,
    },
    Index {
        value: Box<Expr>,
        index: Option<Box<Expr>>,
        wildcard: bool,
    },
    Call {
        callee: Box<Expr>,
        arguments: Vec<CallArgument>,
    },
    Lambda {
        parameters: Vec<String>,
        body: Box<Expr>,
    },
}

/// Render decoded string text back into freql source syntax.
pub fn quote_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

#[derive(Clone, Debug, Serialize)]
pub struct CallArgument {
    pub name: Option<String>,
    pub value: Expr,
    #[serde(skip)]
    pub span: Span,
}

impl Expr {
    pub fn name(value: impl Into<String>, span: Span) -> Self {
        Self {
            kind: ExprKind::Name {
                value: value.into(),
            },
            span,
        }
    }

    pub fn as_name(&self) -> Option<&str> {
        match &self.kind {
            ExprKind::Name { value } => Some(value),
            _ => None,
        }
    }

    pub fn path(&self) -> Option<String> {
        match &self.kind {
            ExprKind::Name { value } => Some(value.clone()),
            ExprKind::Member { value, member } => Some(format!("{}.{}", value.path()?, member)),
            ExprKind::Index {
                value,
                index,
                wildcard,
            } => {
                if *wildcard {
                    Some(format!("{}[*]", value.path()?))
                } else {
                    Some(format!("{}[{}]", value.path()?, index.as_ref()?.display()))
                }
            }
            _ => None,
        }
    }

    pub fn display(&self) -> String {
        match &self.kind {
            ExprKind::Presentation {
                value,
                unit,
                approximate,
            } => format!(
                "({} as {}{})",
                value.display(),
                if *approximate { "~" } else { "" },
                unit.as_ref()
                    .map_or_else(String::new, |unit| unit.display())
            ),
            ExprKind::Name { value } => value.clone(),
            ExprKind::Number { value, suffix } => match suffix {
                Some(suffix) => format!("{value} {suffix}"),
                None => value.clone(),
            },
            // `value` holds the decoded text, so display re-encodes it. Round
            // tripping through the same escapes keeps report predicates and
            // `--set` inputs re-parsable.
            ExprKind::String { value } => quote_string(value),
            ExprKind::Bool { value } => value.to_string(),
            ExprKind::NamedTuple { fields } => format!(
                "({})",
                fields
                    .iter()
                    .map(|field| format!(
                        "{}: {}",
                        field.name.as_deref().expect("tuple fields are named"),
                        field.value.display()
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ExprKind::Array { items } => format!(
                "[{}]",
                items
                    .iter()
                    .map(Self::display)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ExprKind::Unary { op, value } => format!("{op}{}", value.display()),
            // A power whose base is a complete quantity literal can only come
            // from explicit source grouping now that `2 Meter^2` desugars to
            // `2 * Meter^2`. Preserve that grouping so display -> parse does
            // not silently change `(2 Meter)^2` back into `2 Meter^2`.
            ExprKind::Binary {
                op: BinaryOp::Pow,
                left,
                right,
            } if matches!(
                left.kind,
                ExprKind::Number {
                    suffix: Some(_),
                    ..
                }
            ) =>
            {
                format!("(({}) ^ {})", left.display(), right.display())
            }
            ExprKind::Binary { op, left, right } => {
                format!("({} {op} {})", left.display(), right.display())
            }
            ExprKind::Member { value, member } => format!("{}.{}", value.display(), member),
            ExprKind::Index {
                value,
                index,
                wildcard,
            } => {
                if *wildcard {
                    format!("{}[*]", value.display())
                } else {
                    format!(
                        "{}[{}]",
                        value.display(),
                        index.as_ref().map(|v| v.display()).unwrap_or_default()
                    )
                }
            }
            ExprKind::Call { callee, arguments } => format!(
                "{}({})",
                callee.display(),
                arguments
                    .iter()
                    .map(|argument| match &argument.name {
                        Some(name) => format!("{name}: {}", argument.value.display()),
                        None => argument.value.display(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ExprKind::Lambda { parameters, body } => {
                format!("{} => {}", parameters.join(", "), body.display())
            }
        }
    }
}
