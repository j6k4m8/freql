use std::fmt;

use serde::Serialize;

use crate::ast::*;
use crate::lexer::{Lexed, Token, lex};
use crate::syntax::{SyntaxKind, SyntaxNode, flat_green};

#[derive(Clone, Debug, Serialize)]
pub struct ParseError {
    pub message: String,
    #[serde(skip)]
    pub span: Span,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at {}..{}",
            self.message, self.span.start, self.span.end
        )
    }
}

#[derive(Clone, Debug)]
pub struct Parse {
    pub module: Module,
    pub errors: Vec<ParseError>,
    pub cst: SyntaxNode,
}

/// An expression paired with an optional author-selected presentation unit.
/// `as` never changes the expression's value or structural dimension.
#[derive(Clone, Debug)]
pub struct DisplayExpression {
    pub expression: Expr,
    pub display_unit: Option<String>,
}

#[derive(Clone, Debug)]
struct ParsedDisplayUnit {
    expression: Expr,
    text: String,
    span: Span,
}

pub fn parse(source: &str) -> Parse {
    let lexed = lex(source);
    let green = flat_green(
        lexed
            .tokens
            .iter()
            .map(|token| (token.kind, token.text.clone())),
    );
    let cst = SyntaxNode::new_root(green);
    let mut parser = Parser::new(source, lexed);
    let module = parser.parse_module();
    Parse {
        module,
        errors: parser.errors,
        cst,
    }
}

/// Parse one standalone expression for tooling inputs such as `--set`.
pub fn parse_expression(source: &str) -> Result<Expr, Vec<ParseError>> {
    parse_display_expression_internal(source, false).map(|parsed| parsed.expression)
}

/// Parse the shared `expression as Unit` presentation form used by calculator
/// input and monitored values.
pub fn parse_display_expression(source: &str) -> Result<DisplayExpression, Vec<ParseError>> {
    parse_display_expression_internal(source, true)
}

fn parse_display_expression_internal(
    source: &str,
    allow_display_unit: bool,
) -> Result<DisplayExpression, Vec<ParseError>> {
    let lexed = lex(source);
    let mut parser = Parser::new(source, lexed);
    let expression = parser.parse_expr(0);
    if expression.is_none() && parser.errors.is_empty() {
        parser.error("expected expression", parser.current_span());
    }
    let display_unit = if allow_display_unit {
        parser.parse_optional_display_unit().map(|unit| unit.text)
    } else {
        None
    };
    if !parser.eof() {
        parser.error("unexpected token after expression", parser.current_span());
    }
    if parser.errors.is_empty() {
        Ok(DisplayExpression {
            expression: expression.expect("successful expression parse"),
            display_unit,
        })
    } else {
        Err(parser.errors)
    }
}

/// Canonical presentation spelling for the deliberately small unit-expression
/// grammar. `Expr::display` already preserves grouping by fully parenthesizing
/// binary operations; trim only that guaranteed outer pair and tighten powers.
fn compact_unit_expression(expression: &Expr) -> String {
    let display = expression.display().replace(" ^ ", "^");
    display
        .strip_prefix('(')
        .and_then(|display| display.strip_suffix(')'))
        .unwrap_or(&display)
        .to_owned()
}

/// Keywords that can begin a top-level item. Used both to dispatch and, after
/// a failed item, to find the next place worth resuming from.
const ITEM_KEYWORDS: &[&str] = &[
    "import", "artifact", "resource", "process", "unit", "fn", "trait", "impl",
];

/// Keywords that can begin a process member.
const PROCESS_MEMBER_KEYWORDS: &[&str] = &["config", "expose", "gets", "uses", "export", "monitor"];

/// How many times "expected <item>" may be reported for one file. Recovery
/// already collapses a run of junk into a single report; this is the backstop
/// that keeps a pathological file from producing a diagnostic per token.
const MAX_UNEXPECTED_ITEM_ERRORS: usize = 2;

/// How many times a trait or impl body may report that it contains something
/// other than a function, for the same reason.
const MAX_UNEXPECTED_MEMBER_ERRORS: usize = 2;

/// Left binding power of `or`, the loosest operator in the language.
const LOGICAL_OR_BINDING_POWER: u8 = 1;
/// Left binding power of `and`, which binds tighter than `or`.
const LOGICAL_AND_BINDING_POWER: u8 = 3;
/// Minimum binding power for the operand of `not` / `!`. It sits just above
/// `and` so `not a and b` is `(not a) and b`, and below the comparisons so
/// `not a > b` is `not (a > b)`.
const LOGICAL_NOT_OPERAND_BINDING_POWER: u8 = LOGICAL_AND_BINDING_POWER + 2;
/// Minimum binding power for the operand of prefix `+` / `-`. It sits above
/// `^` so `-2 ^ 2` keeps its historical `(-2) ^ 2` reading.
const SIGN_OPERAND_BINDING_POWER: u8 = 21;
/// `^` is right-associative. These two powers are also used when desugaring
/// quantity notation such as `130 Millimeter^3` into
/// `130 * (Millimeter^3)`.
const POWER_LEFT_BINDING_POWER: u8 = 18;
const POWER_RIGHT_BINDING_POWER: u8 = 17;
/// Binding power of the postfix call/index/member suffixes, which bind tighter
/// than any prefix or infix operator.
const POSTFIX_BINDING_POWER: u8 = 23;

/// Every keyword that may legally appear immediately after an expression on
/// the same source line.
///
/// `parse_prefix` consults this list to decide whether an identifier following
/// a numeric literal is a unit suffix (`5 Meter`) or the start of the next
/// construct (`5 aggregate sum`). freql keywords are contextual — they are
/// plain `Ident` tokens — so this decision cannot be made from the token kind
/// alone. A keyword that can follow an expression and is missing here is
/// silently swallowed as a unit suffix.
///
/// Keywords that can only appear *before* an expression, or only directly
/// after another keyword, must be left out. In particular the aggregate rule
/// names `sum`, `makespan`, `max`, `min` and `explicit` only ever follow the
/// `aggregate` keyword, which is itself consumed first; listing them here
/// would make a user-defined unit called `min` unusable as `5 min`.
///
/// Outside parentheses/brackets, a newline between the number and identifier
/// already ends the suffix.
const EXPRESSION_FOLLOW_KEYWORDS: &[&str] = &[
    // Statement and declaration starters, which appear after an expression in
    // single-line bodies such as `config a: Integer = 1 export b: USD`.
    "let",
    "mut",
    "requires",
    "for",
    "if",
    "else",
    "yield",
    "repeat",
    "in",
    "monitor",
    "capacity",
    "gets",
    "uses",
    "export",
    "config",
    "expose",
    "import",
    "process",
    "artifact",
    "resource",
    "unit",
    "fn",
    "trait",
    "impl",
    "where",
    // Keywords that genuinely trail an expression.
    "after",     // yield items = 1 after 2 Second
    "before",    // 1 before 2
    "as",        // 1000 * 3 as Meter
    "aggregate", // config n: Integer = 5 aggregate sum
    // Logical operators, which are keyword-spelled.
    "and",
    "or",
    "not",
];

struct Parser<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    position: usize,
    errors: Vec<ParseError>,
    stop_before_process_signature: bool,
    delimiters: Vec<SyntaxKind>,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str, lexed: Lexed) -> Self {
        let mut errors = lexed
            .errors
            .into_iter()
            .map(|span| ParseError {
                message: "unrecognized token".into(),
                span,
            })
            .collect::<Vec<_>>();
        let tokens = lexed
            .tokens
            .into_iter()
            .filter(|token| !token.is_trivia())
            .collect();
        errors.shrink_to_fit();
        Self {
            source,
            tokens,
            position: 0,
            errors,
            stop_before_process_signature: false,
            delimiters: Vec::new(),
        }
    }

    fn parse_module(&mut self) -> Module {
        let mut items = Vec::new();
        let mut unexpected_items = 0_usize;
        while !self.eof() {
            if self.at(SyntaxKind::DocString) {
                self.bump();
                continue;
            }
            let before = self.position;
            let item = if self.at_kw("import") {
                self.parse_import().map(Item::Import)
            } else if self.at_kw("artifact") {
                self.parse_artifact().map(Item::Artifact)
            } else if self.at_kw("resource") {
                self.parse_resource().map(Item::Resource)
            } else if self.at_kw("process") {
                self.parse_process().map(Item::Process)
            } else if self.at_kw("unit") {
                self.parse_unit().map(Item::Unit)
            } else if self.at_kw("fn") {
                self.parse_function(true).map(Item::Function)
            } else if self.at_kw("trait") {
                self.parse_trait().map(Item::Trait)
            } else if self.at_kw("impl") {
                self.parse_impl().map(Item::Impl)
            } else {
                // Capped: an item parser that gave up can leave the cursor deep
                // inside a body, and repeating this message once per remaining
                // token buries the one diagnostic that matters.
                if unexpected_items < MAX_UNEXPECTED_ITEM_ERRORS {
                    let span = self.current_span();
                    self.error(
                        "expected import, artifact, resource, process, unit, fn, trait, or impl",
                        span,
                    );
                }
                unexpected_items += 1;
                None
            };
            match item {
                Some(item) => items.push(item),
                // Skip to the next plausible item keyword so one syntax error
                // costs a bounded number of diagnostics rather than one per
                // token in the rest of the file.
                None => self.recover_to_item(before),
            }
            self.eat(SyntaxKind::Semicolon);
        }
        Module {
            items,
            span: 0..self.source.len(),
        }
    }

    /// Skip forward to the next token that could start a top-level item.
    ///
    /// Always consumes at least one token when no progress was made, so the
    /// module loop cannot spin.
    fn recover_to_item(&mut self, before: usize) {
        if self.position == before {
            self.bump();
        }
        while !self.eof() && !self.at_item_start() {
            self.bump();
        }
    }

    fn at_item_start(&self) -> bool {
        ITEM_KEYWORDS.iter().any(|keyword| self.at_kw(keyword))
    }

    fn parse_import(&mut self) -> Option<Import> {
        let start = self.expect_kw("import")?.span.start;
        let path = if self.at(SyntaxKind::String) {
            let token = self.bump()?;
            ImportPath::Relative(unquote(&token.text))
        } else {
            ImportPath::Module(self.parse_dotted_name()?)
        };
        let mut names = Vec::new();
        let alias = if self.eat_kw("as").is_some() {
            Some(self.expect_ident()?.text)
        } else {
            None
        };
        if alias.is_none() && self.eat(SyntaxKind::LParen).is_some() {
            while !self.eof() && !self.at(SyntaxKind::RParen) {
                let Some(name) = self.expect_ident() else {
                    break;
                };
                names.push(name.text);
                if self.eat(SyntaxKind::Comma).is_none() {
                    break;
                }
            }
            self.expect(SyntaxKind::RParen, "expected ')' after imported names");
        }
        let end = self.previous_end();
        Some(Import {
            path,
            names,
            alias,
            span: start..end,
        })
    }

    fn parse_artifact(&mut self) -> Option<Artifact> {
        let start = self.expect_kw("artifact")?.span.start;
        let name = self.expect_ident()?.text;
        self.expect(SyntaxKind::LBrace, "expected '{' after artifact name")?;
        let doc = self.take_docstring();
        let mut fields = Vec::new();
        while !self.eof() && !self.at(SyntaxKind::RBrace) {
            if self.at(SyntaxKind::DocString) {
                self.bump();
                continue;
            }
            let field_start = self.current_span().start;
            let Some(field_name) = self.expect_ident().map(|token| token.text) else {
                self.recover_to_member();
                continue;
            };
            self.expect(SyntaxKind::Colon, "expected ':' after field name");
            let Some(ty) = self.parse_type() else {
                self.recover_to_member();
                continue;
            };
            let derived = if self.at(SyntaxKind::LBrace) {
                self.bump();
                let value = self.parse_expr(0);
                self.expect(
                    SyntaxKind::RBrace,
                    "expected '}' after derived field expression",
                );
                value
            } else {
                None
            };
            fields.push(Field {
                name: field_name,
                ty,
                derived,
                span: field_start..self.previous_end(),
            });
            self.eat(SyntaxKind::Semicolon);
        }
        // Not fatal: a missing brace should not discard an otherwise usable
        // artifact and drop the module parser into the middle of its body.
        self.expect(SyntaxKind::RBrace, "expected '}' after artifact");
        Some(Artifact {
            name,
            doc,
            fields,
            span: start..self.previous_end(),
        })
    }

    fn parse_resource(&mut self) -> Option<Resource> {
        let start = self.expect_kw("resource")?.span.start;
        let name = self.expect_ident()?.text;
        self.expect(SyntaxKind::LBrace, "expected '{' after resource name")?;
        let doc = self.take_docstring();
        if self.expect_kw("capacity").is_none()
            || self
                .expect(SyntaxKind::Colon, "expected ':' after resource capacity")
                .is_none()
        {
            self.recover_past_close_brace();
            return None;
        }
        let Some(capacity) = self.parse_expr(0) else {
            self.recover_past_close_brace();
            return None;
        };
        self.eat(SyntaxKind::Semicolon);
        self.expect(SyntaxKind::RBrace, "expected '}' after resource");
        Some(Resource {
            name,
            doc,
            capacity,
            span: start..self.previous_end(),
        })
    }

    fn parse_process(&mut self) -> Option<Process> {
        let start = self.expect_kw("process")?.span.start;
        let name = self.expect_ident()?.text;
        let mut parents = Vec::new();
        if self.eat(SyntaxKind::Colon).is_some() {
            loop {
                parents.push(self.parse_dotted_name()?);
                if self.eat(SyntaxKind::Comma).is_none() {
                    break;
                }
            }
        }
        self.expect(SyntaxKind::LBrace, "expected '{' after process declaration")?;
        let doc = self.take_docstring();
        let mut members = Vec::new();
        let mut monitors = Vec::new();
        let mut parameters = Vec::new();
        let mut body = None;
        while !self.eof() && !self.at(SyntaxKind::RBrace) {
            if self.at(SyntaxKind::DocString) {
                self.bump();
                continue;
            }
            if self.at(SyntaxKind::LParen) {
                parameters = self.parse_parameters()?;
                if self.eat(SyntaxKind::FatArrow).is_some() {
                    body = self.parse_block();
                } else if !self.at(SyntaxKind::RBrace) {
                    self.error(
                        "expected '=>' and a body, or '}' after an open invocation signature",
                        self.current_span(),
                    );
                    self.recover_to_member();
                }
                continue;
            }
            if let Some(token) = self.eat_kw("monitor") {
                let Some(name) = self.expect_ident() else {
                    self.recover_to_member();
                    continue;
                };
                let declared_type = if self.eat(SyntaxKind::Colon).is_some() {
                    match self.parse_type() {
                        Some(ty) => Some(ty),
                        None => {
                            self.recover_to_member();
                            continue;
                        }
                    }
                } else {
                    None
                };
                let display_unit = self.parse_optional_display_unit();
                let end = display_unit.as_ref().map_or_else(
                    || {
                        declared_type
                            .as_ref()
                            .map_or(name.span.end, |ty| ty.span.end)
                    },
                    |unit| unit.span.end,
                );
                if let Some(ty) = declared_type {
                    members.push(ProcessMember {
                        role: MemberRole::Export,
                        exposed: false,
                        name: name.text.clone(),
                        ty,
                        default: None,
                        aggregate: Some(AggregateRule::Explicit),
                        span: token.span.start..end,
                    });
                }
                monitors.push(Monitor {
                    name: name.text,
                    display_unit: display_unit.as_ref().map(|unit| unit.text.clone()),
                    display_unit_expression: display_unit.map(|unit| unit.expression),
                    span: token.span.start..end,
                });
                self.eat(SyntaxKind::Semicolon);
                continue;
            }
            let member_start = self.current_span().start;
            let exposed = self.eat_kw("expose").is_some();
            let role = if exposed {
                if self.eat_kw("config").is_none() {
                    self.error("expected 'config' after 'expose'", self.current_span());
                }
                MemberRole::Config
            } else if self.eat_kw("config").is_some() {
                MemberRole::Config
            } else if self.eat_kw("gets").is_some() {
                MemberRole::Gets
            } else if self.eat_kw("uses").is_some() {
                MemberRole::Uses
            } else if self.eat_kw("export").is_some() {
                MemberRole::Export
            } else {
                self.error(
                    "expected 'config', 'expose config', 'gets', 'uses', 'export', 'monitor', or an invocation signature",
                    self.current_span(),
                );
                // Continue as a configuration declaration so one missing
                // keyword does not destroy the rest of the process CST/AST.
                MemberRole::Config
            };
            let Some(member_name) = self.expect_ident().map(|token| token.text) else {
                self.recover_to_process_member();
                continue;
            };
            // Recover instead of propagating: abandoning the whole process here
            // leaves the module parser resuming in the middle of a body, which
            // is what turns one missing ':' into a diagnostic per token.
            if self
                .expect(SyntaxKind::Colon, "expected ':' after process member")
                .is_none()
            {
                self.recover_to_process_member();
                continue;
            }
            let Some(ty) = self.parse_type() else {
                self.recover_to_process_member();
                continue;
            };
            let default = if !self.at_statement_boundary() && self.eat(SyntaxKind::Eq).is_some() {
                let previous = self.stop_before_process_signature;
                self.stop_before_process_signature = true;
                let default = self.parse_expr(0);
                self.stop_before_process_signature = previous;
                default
            } else {
                None
            };
            let aggregate = if !self.at_statement_boundary() && self.eat_kw("aggregate").is_some() {
                let Some(token) = self.expect_ident() else {
                    self.recover_to_process_member();
                    continue;
                };
                match token.text.as_str() {
                    "sum" => Some(AggregateRule::Sum),
                    "makespan" => Some(AggregateRule::Makespan),
                    "max" => Some(AggregateRule::Max),
                    "min" => Some(AggregateRule::Min),
                    "explicit" => Some(AggregateRule::Explicit),
                    _ => {
                        self.error(
                            "expected aggregate rule sum, makespan, max, min, or explicit",
                            token.span,
                        );
                        None
                    }
                }
            } else {
                None
            };
            members.push(ProcessMember {
                role,
                exposed,
                name: member_name,
                ty,
                default,
                aggregate,
                span: member_start..self.previous_end(),
            });
            self.eat(SyntaxKind::Semicolon);
        }
        self.expect(SyntaxKind::RBrace, "expected '}' after process");
        Some(Process {
            name,
            parents,
            doc,
            members,
            monitors,
            parameters,
            body,
            span: start..self.previous_end(),
        })
    }

    fn parse_unit(&mut self) -> Option<UnitDecl> {
        let start = self.expect_kw("unit")?.span.start;
        let name = self.expect_ident()?.text;
        self.expect(SyntaxKind::Eq, "expected '=' in unit declaration")?;
        let value = self.parse_expr(0)?;
        let refinement = if self.at(SyntaxKind::LBrace) {
            self.parse_block()
        } else {
            None
        };
        Some(UnitDecl {
            name,
            value,
            refinement,
            span: start..self.previous_end(),
        })
    }

    fn parse_function(&mut self, require_body: bool) -> Option<Function> {
        let start = self.expect_kw("fn")?.span.start;
        let name = self.expect_ident()?.text;
        let generics = self.parse_generic_names();
        let parameters = self.parse_parameters()?;
        self.expect(SyntaxKind::Arrow, "expected '->' before function outputs")?;
        let outputs = self.parse_parameters()?;
        let mut bounds = Vec::new();
        if self.eat_kw("where").is_some() {
            loop {
                let bound_start = self.current_span().start;
                let parameter = self.expect_ident()?.text;
                self.expect(SyntaxKind::Colon, "expected ':' in type bound")?;
                let capability = self.parse_type()?;
                bounds.push(TypeBound {
                    parameter,
                    capability,
                    span: bound_start..self.previous_end(),
                });
                if self.eat(SyntaxKind::Comma).is_none() {
                    break;
                }
            }
        }
        let body = if self.at(SyntaxKind::LBrace) {
            self.parse_block()
        } else {
            if require_body {
                self.error("function declaration requires a body", self.current_span());
            }
            self.eat(SyntaxKind::Semicolon);
            None
        };
        Some(Function {
            name,
            generics,
            parameters,
            outputs,
            bounds,
            body,
            span: start..self.previous_end(),
        })
    }

    fn parse_trait(&mut self) -> Option<TraitDecl> {
        let start = self.expect_kw("trait")?.span.start;
        let name = self.expect_ident()?.text;
        let generics = self.parse_generic_names();
        self.expect(SyntaxKind::LBrace, "expected '{' after trait")?;
        let mut functions = Vec::new();
        let mut unexpected = 0_usize;
        while !self.eof() && !self.at(SyntaxKind::RBrace) {
            if self.at_kw("fn") {
                let before = self.position;
                if let Some(function) = self.parse_function(false) {
                    functions.push(function);
                } else {
                    self.recover_to_function();
                }
                if self.position == before {
                    self.bump();
                }
            } else {
                if unexpected < MAX_UNEXPECTED_MEMBER_ERRORS {
                    self.error(
                        "traits may contain only pure function signatures",
                        self.current_span(),
                    );
                }
                unexpected += 1;
                self.recover_to_function();
            }
        }
        self.expect(SyntaxKind::RBrace, "expected '}' after trait");
        Some(TraitDecl {
            name,
            generics,
            functions,
            span: start..self.previous_end(),
        })
    }

    fn parse_impl(&mut self) -> Option<ImplDecl> {
        let start = self.expect_kw("impl")?.span.start;
        let trait_name = self.parse_dotted_name()?;
        let arguments = if self.eat(SyntaxKind::LBracket).is_some() {
            let mut values = Vec::new();
            while !self.eof() && !self.at(SyntaxKind::RBracket) {
                values.push(self.parse_type()?);
                if self.eat(SyntaxKind::Comma).is_none() {
                    break;
                }
            }
            self.expect(SyntaxKind::RBracket, "expected ']' after impl arguments")?;
            values
        } else {
            Vec::new()
        };
        self.expect(SyntaxKind::LBrace, "expected '{' after impl")?;
        let mut functions = Vec::new();
        let mut unexpected = 0_usize;
        while !self.eof() && !self.at(SyntaxKind::RBrace) {
            if self.at_kw("fn") {
                let before = self.position;
                if let Some(function) = self.parse_function(true) {
                    functions.push(function);
                } else {
                    self.recover_to_function();
                }
                if self.position == before {
                    self.bump();
                }
            } else {
                if unexpected < MAX_UNEXPECTED_MEMBER_ERRORS {
                    self.error(
                        "impl blocks may contain only pure functions",
                        self.current_span(),
                    );
                }
                unexpected += 1;
                self.recover_to_function();
            }
        }
        self.expect(SyntaxKind::RBrace, "expected '}' after impl");
        Some(ImplDecl {
            trait_name,
            arguments,
            functions,
            span: start..self.previous_end(),
        })
    }

    fn parse_generic_names(&mut self) -> Vec<String> {
        let mut values = Vec::new();
        if self.eat(SyntaxKind::LBracket).is_none() {
            return values;
        }
        while !self.eof() && !self.at(SyntaxKind::RBracket) {
            if let Some(name) = self.expect_ident() {
                values.push(name.text);
            }
            if self.eat(SyntaxKind::Comma).is_none() {
                break;
            }
        }
        self.expect(
            SyntaxKind::RBracket,
            "expected ']' after generic parameters",
        );
        values
    }

    fn parse_parameters(&mut self) -> Option<Vec<Parameter>> {
        self.expect(SyntaxKind::LParen, "expected '('")?;
        let mut parameters = Vec::new();
        while !self.eof() && !self.at(SyntaxKind::RParen) {
            let start = self.current_span().start;
            let name = self.expect_ident()?.text;
            self.expect(SyntaxKind::Colon, "expected ':' after parameter")?;
            let ty = self.parse_type()?;
            parameters.push(Parameter {
                name,
                ty,
                span: start..self.previous_end(),
            });
            if self.eat(SyntaxKind::Comma).is_none() {
                break;
            }
        }
        self.expect(SyntaxKind::RParen, "expected ')' after parameters")?;
        Some(parameters)
    }

    fn parse_type(&mut self) -> Option<TypeExpr> {
        let start = self.current_span().start;
        let mut type_expression = if self.at(SyntaxKind::LParen)
            && self
                .tokens
                .get(self.position + 2)
                .is_some_and(|token| token.kind == SyntaxKind::Colon)
        {
            let fields = self.parse_parameters()?;
            let mut names = std::collections::HashSet::new();
            for field in &fields {
                if !names.insert(&field.name) {
                    self.error("duplicate named tuple field", field.span.clone());
                }
            }
            TypeExpr {
                fields: Some(fields),
                name: String::new(),
                dimension: None,
                arguments: Vec::new(),
                arrays: Vec::new(),
                span: start..self.previous_end(),
            }
        } else if self.eat(SyntaxKind::LParen).is_some() {
            let dimension = self.parse_type_dimension_expr(0)?;
            self.expect(SyntaxKind::RParen, "expected ')' after dimensional type")?;
            TypeExpr {
                fields: None,
                name: dimension.display(),
                dimension: Some(dimension),
                arguments: Vec::new(),
                arrays: Vec::new(),
                span: start..self.previous_end(),
            }
        } else {
            self.parse_named_type()?
        };

        if type_expression.fields.is_none()
            && type_expression.dimension.is_none()
            && type_expression.arguments.is_empty()
            && self.at_type_dimension_operator()
        {
            let left = Expr::name(type_expression.name.clone(), type_expression.span.clone());
            let dimension = self.parse_type_dimension_tail(left, 0)?;
            type_expression.name = dimension.display();
            type_expression.dimension = Some(dimension);
        }

        while !self.at_statement_boundary() && self.eat(SyntaxKind::LBracket).is_some() {
            if self.eat(SyntaxKind::RBracket).is_some() {
                type_expression.arrays.push(ArraySize::Dynamic);
            } else {
                let value_start = self.current_span().start;
                while !self.eof() && !self.at(SyntaxKind::RBracket) {
                    self.bump()?;
                }
                let value_end = self.previous_end();
                let text = self.source[value_start..value_end].trim().to_owned();
                self.expect(SyntaxKind::RBracket, "expected ']' after array size")?;
                type_expression.arrays.push(ArraySize::Fixed(text));
            }
        }
        type_expression.span = start..self.previous_end();
        Some(type_expression)
    }

    fn parse_named_type(&mut self) -> Option<TypeExpr> {
        let start = self.current_span().start;
        let name = self.parse_dotted_name()?;
        let mut arguments = Vec::new();
        let generic_name = matches!(
            name.as_str(),
            "Stream"
                | "Configured"
                | "Array"
                | "Equatable"
                | "Ordered"
                | "Add"
                | "Subtract"
                | "Multiply"
                | "Divide"
                | "Power"
                | "Iterable"
                | "Indexable"
                | "Copyable"
        );
        if generic_name && !self.at_statement_boundary() && self.eat(SyntaxKind::LBracket).is_some()
        {
            while !self.eof() && !self.at(SyntaxKind::RBracket) {
                if self.at(SyntaxKind::Number) {
                    arguments.push(TypeArgument::Const(self.bump()?.text));
                } else {
                    arguments.push(TypeArgument::Type(self.parse_type()?));
                }
                if self.eat(SyntaxKind::Comma).is_none() {
                    break;
                }
            }
            self.expect(SyntaxKind::RBracket, "expected ']' after type arguments")?;
        }
        Some(TypeExpr {
            fields: None,
            name,
            dimension: None,
            arguments,
            arrays: Vec::new(),
            span: start..self.previous_end(),
        })
    }

    fn at_type_dimension_operator(&self) -> bool {
        !self.at_statement_boundary()
            && (self.at(SyntaxKind::Star)
                || self.at(SyntaxKind::Slash)
                || self.at(SyntaxKind::Caret))
    }

    fn parse_type_dimension_expr(&mut self, min_binding_power: u8) -> Option<Expr> {
        let left = self.parse_type_dimension_prefix()?;
        self.parse_type_dimension_tail(left, min_binding_power)
    }

    fn parse_type_dimension_tail(&mut self, mut left: Expr, min_binding_power: u8) -> Option<Expr> {
        loop {
            if self.at_statement_boundary() {
                break;
            }
            let (left_power, right_power, op) = match self.current_kind() {
                Some(SyntaxKind::Star) => (10, 11, BinaryOp::Mul),
                Some(SyntaxKind::Slash) => (10, 11, BinaryOp::Div),
                Some(SyntaxKind::Caret) => (20, 20, BinaryOp::Pow),
                _ => break,
            };
            if left_power < min_binding_power {
                break;
            }
            self.bump();
            let right = self.parse_type_dimension_expr(right_power)?;
            let span = left.span.start..right.span.end;
            left = Expr {
                kind: ExprKind::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                span,
            };
        }
        Some(left)
    }

    fn parse_type_dimension_prefix(&mut self) -> Option<Expr> {
        let Some(token) = self.bump() else {
            self.error(
                "expected a unit or dimension in type expression",
                self.current_span(),
            );
            return None;
        };
        match token.kind {
            SyntaxKind::Plus | SyntaxKind::Minus => {
                let value = self.parse_type_dimension_expr(SIGN_OPERAND_BINDING_POWER)?;
                Some(Expr {
                    span: token.span.start..value.span.end,
                    kind: ExprKind::Unary {
                        op: if token.kind == SyntaxKind::Minus {
                            UnaryOp::Neg
                        } else {
                            UnaryOp::Pos
                        },
                        value: Box::new(value),
                    },
                })
            }
            SyntaxKind::Ident => {
                let mut name = token.text;
                while self.eat(SyntaxKind::Dot).is_some() {
                    name.push('.');
                    name.push_str(&self.expect_ident()?.text);
                }
                Some(Expr::name(name, token.span.start..self.previous_end()))
            }
            SyntaxKind::Number => Some(Expr {
                kind: ExprKind::Number {
                    value: token.text,
                    suffix: None,
                },
                span: token.span,
            }),
            SyntaxKind::LParen => {
                let expression = self.parse_type_dimension_expr(0)?;
                self.expect(SyntaxKind::RParen, "expected ')' after dimensional type")?;
                Some(Expr {
                    span: token.span.start..self.previous_end(),
                    ..expression
                })
            }
            _ => {
                self.error(
                    "expected a unit or dimension in type expression",
                    token.span,
                );
                None
            }
        }
    }

    fn parse_block(&mut self) -> Option<Block> {
        let start = self.expect(SyntaxKind::LBrace, "expected '{'")?.span.start;
        let mut statements = Vec::new();
        while !self.eof() && !self.at(SyntaxKind::RBrace) {
            let before = self.position;
            if let Some(statement) = self.parse_stmt() {
                statements.push(statement);
                self.eat(SyntaxKind::Semicolon);
                if self.position == before {
                    self.bump();
                }
            } else {
                self.recover_to_statement(before);
            }
        }
        self.expect(SyntaxKind::RBrace, "expected '}' after block");
        Some(Block {
            statements,
            span: start..self.previous_end(),
        })
    }

    fn parse_stmt(&mut self) -> Option<Stmt> {
        let start = self.current_span().start;
        if self.eat_kw("let").is_some() {
            let mutable = self.eat_kw("mut").is_some();
            let name = self.expect_ident()?.text;
            let ty = if self.eat(SyntaxKind::Colon).is_some() {
                self.parse_type()
            } else {
                None
            };
            self.expect(SyntaxKind::Eq, "expected '=' in local binding")?;
            let value = self.parse_expr(0)?;
            return Some(Stmt::Let {
                mutable,
                name,
                ty,
                value,
                span: start..self.previous_end(),
            });
        }
        if self.eat_kw("requires").is_some() {
            let value = self.parse_expr(0)?;
            return Some(Stmt::Requires {
                value,
                span: start..self.previous_end(),
            });
        }
        if self.eat_kw("for").is_some() {
            let binding = self.expect_ident()?.text;
            self.expect_kw("in")?;
            let iterable = self.parse_expr(0)?;
            let body = self.parse_block()?;
            return Some(Stmt::For {
                binding,
                iterable,
                body,
                span: start..self.previous_end(),
            });
        }
        if self.eat_kw("repeat").is_some() {
            let count = self.parse_expr(0)?;
            let body = self.parse_block()?;
            return Some(Stmt::Repeat {
                count,
                body,
                span: start..self.previous_end(),
            });
        }
        if self.eat_kw("if").is_some() {
            let condition = self.parse_expr(0)?;
            let then_branch = self.parse_block()?;
            let else_branch = if self.eat_kw("else").is_some() {
                Some(self.parse_block()?)
            } else {
                None
            };
            return Some(Stmt::If {
                condition,
                then_branch,
                else_branch,
                span: start..self.previous_end(),
            });
        }
        if self.eat_kw("yield").is_some() {
            let target = self.parse_expr(0)?;
            self.expect(SyntaxKind::Eq, "expected '=' in yield")?;
            let value = self.parse_expr(0)?;
            let after = if !self.at_statement_boundary() && self.eat_kw("after").is_some() {
                Some(self.parse_expr(0)?)
            } else {
                None
            };
            return Some(Stmt::Yield {
                target,
                value,
                after,
                span: start..self.previous_end(),
            });
        }
        let left = self.parse_expr(0)?;
        if self.at_statement_boundary() {
            return Some(Stmt::Expr {
                value: left,
                span: start..self.previous_end(),
            });
        }
        if self.eat(SyntaxKind::Arrow).is_some() {
            let target = self.parse_expr(0)?;
            return Some(Stmt::Wire {
                source: left,
                target,
                span: start..self.previous_end(),
            });
        }
        if self.eat_kw("before").is_some() {
            let second = self.parse_expr(0)?;
            return Some(Stmt::Before {
                first: left,
                second,
                span: start..self.previous_end(),
            });
        }
        if self.eat(SyntaxKind::Eq).is_some() {
            let value = self.parse_expr(0)?;
            return Some(Stmt::Assign {
                target: left,
                value,
                span: start..self.previous_end(),
            });
        }
        Some(Stmt::Expr {
            value: left,
            span: start..self.previous_end(),
        })
    }

    fn parse_expr(&mut self, min_binding_power: u8) -> Option<Expr> {
        let mut left = self.parse_prefix()?;
        loop {
            // A complete expression ends at a physical newline unless an
            // enclosing pair of parentheses/brackets keeps it open. A leading
            // dot is the one explicit postfix continuation across a boundary.
            if self.at_statement_boundary() && !self.at(SyntaxKind::Dot) {
                break;
            }
            if self.stop_before_process_signature && self.at_process_signature() {
                break;
            }
            if self.at(SyntaxKind::LParen) {
                if POSTFIX_BINDING_POWER < min_binding_power {
                    break;
                }
                let start = left.span.start;
                self.bump();
                let mut arguments = Vec::new();
                while !self.eof() && !self.at(SyntaxKind::RParen) {
                    let arg_start = self.current_span().start;
                    let name = if self.at(SyntaxKind::Ident)
                        && self.peek_kind(1) == Some(SyntaxKind::Colon)
                    {
                        let name = self.bump()?.text;
                        self.bump();
                        Some(name)
                    } else {
                        None
                    };
                    let value = self.parse_expr(0)?;
                    arguments.push(CallArgument {
                        name,
                        value,
                        span: arg_start..self.previous_end(),
                    });
                    if self.eat(SyntaxKind::Comma).is_none() {
                        break;
                    }
                }
                self.expect(SyntaxKind::RParen, "expected ')' after call arguments")?;
                left = Expr {
                    kind: ExprKind::Call {
                        callee: Box::new(left),
                        arguments,
                    },
                    span: start..self.previous_end(),
                };
                continue;
            }
            if self.eat(SyntaxKind::Dot).is_some() {
                let start = left.span.start;
                let member = self.expect_ident()?.text;
                left = Expr {
                    kind: ExprKind::Member {
                        value: Box::new(left),
                        member,
                    },
                    span: start..self.previous_end(),
                };
                continue;
            }
            if self.eat(SyntaxKind::LBracket).is_some() {
                let start = left.span.start;
                let (index, wildcard) = if self.eat(SyntaxKind::Star).is_some() {
                    (None, true)
                } else {
                    (Some(Box::new(self.parse_expr(0)?)), false)
                };
                self.expect(SyntaxKind::RBracket, "expected ']' after index")?;
                left = Expr {
                    kind: ExprKind::Index {
                        value: Box::new(left),
                        index,
                        wildcard,
                    },
                    span: start..self.previous_end(),
                };
                continue;
            }
            if self.at(SyntaxKind::FatArrow) {
                if min_binding_power > 0 {
                    break;
                }
                let parameters = match &left.kind {
                    ExprKind::Name { value } => vec![value.clone()],
                    _ => {
                        self.error("lambda parameters must be identifiers", left.span.clone());
                        Vec::new()
                    }
                };
                let start = left.span.start;
                self.bump();
                let body = self.parse_expr(0)?;
                left = Expr {
                    kind: ExprKind::Lambda {
                        parameters,
                        body: Box::new(body),
                    },
                    span: start..self.previous_end(),
                };
                continue;
            }
            let Some((left_power, right_power, op)) = self.infix_binding_power() else {
                break;
            };
            if left_power < min_binding_power {
                break;
            }
            let start = left.span.start;
            self.bump();
            let right = self.parse_expr(right_power)?;
            let end = right.span.end;
            left = Expr {
                kind: ExprKind::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                span: start..end,
            };
        }
        Some(left)
    }

    fn parse_prefix(&mut self) -> Option<Expr> {
        if matches!(
            self.current_kind(),
            Some(SyntaxKind::RBrace | SyntaxKind::RParen | SyntaxKind::RBracket)
        ) {
            self.error("expected expression", self.current_span());
            return None;
        }
        let Some(token) = self.bump() else {
            self.error("expected expression", self.current_span());
            return None;
        };
        match token.kind {
            SyntaxKind::Ident => {
                if token.text == "true" || token.text == "false" {
                    Some(Expr {
                        kind: ExprKind::Bool {
                            value: token.text == "true",
                        },
                        span: token.span,
                    })
                } else if token.text == "not" {
                    let value = self.parse_expr(LOGICAL_NOT_OPERAND_BINDING_POWER)?;
                    Some(Expr {
                        span: token.span.start..value.span.end,
                        kind: ExprKind::Unary {
                            op: UnaryOp::Not,
                            value: Box::new(value),
                        },
                    })
                } else {
                    Some(Expr::name(token.text, token.span))
                }
            }
            SyntaxKind::Number => {
                let suffix = if self.at(SyntaxKind::Ident)
                    && !self.current_is_expression_follow_keyword()
                    && !self.at_statement_boundary()
                {
                    let mut suffix = self.bump()?;
                    while self.eat(SyntaxKind::Dot).is_some() {
                        suffix.text.push('.');
                        suffix.text.push_str(&self.expect_ident()?.text);
                        suffix.span.end = self.previous_end();
                    }
                    Some(suffix)
                } else {
                    None
                };

                // A power written directly on a quantity suffix applies to
                // the unit, not to the coefficient: `2 Meter^2` means two
                // square meters. Parentheses retain ordinary arithmetic
                // semantics, so `(2 Meter)^2` still means four square meters.
                if let Some(suffix) = suffix.as_ref()
                    && !self.at_statement_boundary()
                    && self.eat(SyntaxKind::Caret).is_some()
                {
                    let exponent = self.parse_expr(POWER_RIGHT_BINDING_POWER)?;
                    let end = exponent.span.end;
                    let coefficient = Expr {
                        kind: ExprKind::Number {
                            value: token.text,
                            suffix: None,
                        },
                        span: token.span.clone(),
                    };
                    let powered_unit = Expr {
                        kind: ExprKind::Binary {
                            op: BinaryOp::Pow,
                            left: Box::new(Expr::name(suffix.text.clone(), suffix.span.clone())),
                            right: Box::new(exponent),
                        },
                        span: suffix.span.start..end,
                    };
                    return Some(Expr {
                        kind: ExprKind::Binary {
                            op: BinaryOp::Mul,
                            left: Box::new(coefficient),
                            right: Box::new(powered_unit),
                        },
                        span: token.span.start..end,
                    });
                }
                Some(Expr {
                    kind: ExprKind::Number {
                        value: token.text,
                        suffix: suffix.map(|suffix| suffix.text),
                    },
                    span: token.span.start..self.previous_end(),
                })
            }
            SyntaxKind::String => Some(Expr {
                kind: ExprKind::String {
                    value: unquote(&token.text),
                },
                span: token.span,
            }),
            SyntaxKind::Bang => {
                // `!` and `not` are the same operator and share a precedence.
                let value = self.parse_expr(LOGICAL_NOT_OPERAND_BINDING_POWER)?;
                Some(Expr {
                    span: token.span.start..value.span.end,
                    kind: ExprKind::Unary {
                        op: UnaryOp::Not,
                        value: Box::new(value),
                    },
                })
            }
            SyntaxKind::Minus | SyntaxKind::Plus => {
                let op = if token.kind == SyntaxKind::Minus {
                    UnaryOp::Neg
                } else {
                    UnaryOp::Pos
                };
                let value = self.parse_expr(SIGN_OPERAND_BINDING_POWER)?;
                Some(Expr {
                    span: token.span.start..value.span.end,
                    kind: ExprKind::Unary {
                        op,
                        value: Box::new(value),
                    },
                })
            }
            SyntaxKind::LParen => {
                if self.at(SyntaxKind::Ident) && self.peek_kind(1) == Some(SyntaxKind::Colon) {
                    let mut fields = Vec::new();
                    let mut names = std::collections::HashSet::new();
                    loop {
                        let start = self.current_span().start;
                        let name = self.expect_ident()?.text;
                        self.expect(SyntaxKind::Colon, "expected ':' after tuple field")?;
                        let value = self.parse_expr(0)?;
                        let span = start..self.previous_end();
                        if !names.insert(name.clone()) {
                            self.error("duplicate named tuple field", span.clone());
                        }
                        fields.push(CallArgument {
                            name: Some(name),
                            value,
                            span,
                        });
                        if self.eat(SyntaxKind::Comma).is_none() || self.at(SyntaxKind::RParen) {
                            break;
                        }
                    }
                    self.expect(SyntaxKind::RParen, "expected ')' after named tuple")?;
                    return Some(Expr {
                        kind: ExprKind::NamedTuple { fields },
                        span: token.span.start..self.previous_end(),
                    });
                }
                let value = self.parse_expr(0)?;
                self.expect(SyntaxKind::RParen, "expected ')' after expression")?;
                Some(Expr {
                    span: token.span.start..self.previous_end(),
                    ..value
                })
            }
            SyntaxKind::LBracket => {
                let mut items = Vec::new();
                while !self.eof() && !self.at(SyntaxKind::RBracket) {
                    items.push(self.parse_expr(0)?);
                    if self.eat(SyntaxKind::Comma).is_none() {
                        break;
                    }
                }
                self.expect(SyntaxKind::RBracket, "expected ']' after array")?;
                Some(Expr {
                    kind: ExprKind::Array { items },
                    span: token.span.start..self.previous_end(),
                })
            }
            _ => {
                self.error("expected expression", token.span);
                None
            }
        }
    }

    /// Infix precedence, loosest binding first:
    ///
    /// `or` < `and` < `not`/`!` < comparisons < `+`/`-` < `*`/`/` < `^`
    ///
    /// Every operator except `^` is left-associative, expressed by giving the
    /// right operand a binding power one greater than the left. `^` reverses
    /// that to bind to the right.
    fn infix_binding_power(&self) -> Option<(u8, u8, BinaryOp)> {
        let kind = self.current_kind()?;
        if kind == SyntaxKind::Ident {
            // `and` and `or` are keyword-spelled operators. They must also
            // appear in EXPRESSION_FOLLOW_KEYWORDS so `1 and x` does not read
            // `and` as a unit suffix.
            if self.at_kw("or") {
                return Some((
                    LOGICAL_OR_BINDING_POWER,
                    LOGICAL_OR_BINDING_POWER + 1,
                    BinaryOp::Or,
                ));
            }
            if self.at_kw("and") {
                return Some((
                    LOGICAL_AND_BINDING_POWER,
                    LOGICAL_AND_BINDING_POWER + 1,
                    BinaryOp::And,
                ));
            }
            return None;
        }
        match kind {
            SyntaxKind::EqEq => Some((7, 8, BinaryOp::Eq)),
            SyntaxKind::NotEq => Some((7, 8, BinaryOp::NotEq)),
            SyntaxKind::Lt => Some((9, 10, BinaryOp::Lt)),
            SyntaxKind::Le => Some((9, 10, BinaryOp::Le)),
            SyntaxKind::Gt => Some((9, 10, BinaryOp::Gt)),
            SyntaxKind::Ge => Some((9, 10, BinaryOp::Ge)),
            SyntaxKind::Plus => Some((11, 12, BinaryOp::Add)),
            SyntaxKind::Minus => Some((11, 12, BinaryOp::Sub)),
            SyntaxKind::Star => Some((13, 14, BinaryOp::Mul)),
            SyntaxKind::Slash => Some((13, 14, BinaryOp::Div)),
            SyntaxKind::Caret => Some((
                POWER_LEFT_BINDING_POWER,
                POWER_RIGHT_BINDING_POWER,
                BinaryOp::Pow,
            )),
            _ => None,
        }
    }

    fn parse_dotted_name(&mut self) -> Option<String> {
        let mut name = self.expect_ident()?.text;
        while self.eat(SyntaxKind::Dot).is_some() {
            name.push('.');
            name.push_str(&self.expect_ident()?.text);
        }
        Some(name)
    }

    fn parse_optional_display_unit(&mut self) -> Option<ParsedDisplayUnit> {
        if self.at_statement_boundary() {
            return None;
        }
        self.eat_kw("as")?;
        if self.eof() {
            self.error("expected a unit expression after 'as'", self.current_span());
            return None;
        }
        let expression = self.parse_type_dimension_expr(0)?;
        let span = expression.span.clone();
        Some(ParsedDisplayUnit {
            text: compact_unit_expression(&expression),
            expression,
            span,
        })
    }

    fn take_docstring(&mut self) -> Option<String> {
        if self.at(SyntaxKind::DocString) {
            self.bump().map(|token| token.text)
        } else {
            None
        }
    }

    /// Recovery point for artifact fields, whose members start with a bare
    /// identifier.
    fn recover_to_member(&mut self) {
        while !self.eof()
            && !self.at(SyntaxKind::RBrace)
            && !self.at(SyntaxKind::Ident)
            && !self.at(SyntaxKind::LParen)
        {
            self.bump();
        }
    }

    /// Recovery point for process members.
    ///
    /// Process members start with a role keyword rather than a bare
    /// identifier, so stopping at the first identifier (as
    /// [`Self::recover_to_member`] does) would make no progress at all when
    /// the malformed member *is* an identifier. Always consume at least one
    /// token, then stop at the next role keyword, invocation signature, or the
    /// process's closing brace.
    fn recover_to_process_member(&mut self) {
        let mut advanced = false;
        while !self.eof() && !self.at(SyntaxKind::RBrace) {
            if advanced && (self.at_process_member_start() || self.at(SyntaxKind::LParen)) {
                break;
            }
            self.bump();
            advanced = true;
        }
    }

    fn at_process_member_start(&self) -> bool {
        PROCESS_MEMBER_KEYWORDS
            .iter()
            .any(|keyword| self.at_kw(keyword))
    }

    /// Skip past the closing brace of the body currently being parsed, so a
    /// failed item does not leave the module parser inside it.
    fn recover_past_close_brace(&mut self) {
        let mut depth = 0_usize;
        while !self.eof() {
            match self.current_kind() {
                Some(SyntaxKind::LBrace) => depth += 1,
                Some(SyntaxKind::RBrace) => {
                    self.bump();
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                    continue;
                }
                _ => {}
            }
            self.bump();
        }
    }

    /// Skip to the next `fn` or the closing brace of a trait or impl body.
    fn recover_to_function(&mut self) {
        let mut advanced = false;
        while !self.eof() && !self.at(SyntaxKind::RBrace) {
            if advanced && self.at_kw("fn") {
                break;
            }
            self.bump();
            advanced = true;
        }
    }

    /// Skip to the start of the next statement inside a block: the next line,
    /// an explicit `;`, or the block's closing brace.
    fn recover_to_statement(&mut self, before: usize) {
        if self.position == before && !self.eof() {
            self.bump();
        }
        while !self.eof() && !self.at(SyntaxKind::RBrace) && !self.at_line_start() {
            if self.eat(SyntaxKind::Semicolon).is_some() {
                return;
            }
            self.bump();
        }
    }

    /// True when a newline separates the current token from the previous one.
    fn at_line_start(&self) -> bool {
        let end = self.previous_end();
        let start = self.current_span().start;
        end <= start
            && self.source[end..start]
                .bytes()
                .any(|byte| byte == b'\n' || byte == b'\r')
    }

    /// Braces introduce statement lists, even if an outer expression is open.
    /// Delimiter tracking also covers dimensional types and call arguments.
    fn at_statement_boundary(&self) -> bool {
        self.at_line_start()
            && !matches!(
                self.delimiters.last(),
                Some(SyntaxKind::LParen | SyntaxKind::LBracket)
            )
    }

    /// True when the current token is a keyword that can follow an expression,
    /// and therefore must not be absorbed as a numeric literal's unit suffix.
    /// See [`EXPRESSION_FOLLOW_KEYWORDS`].
    fn current_is_expression_follow_keyword(&self) -> bool {
        EXPRESSION_FOLLOW_KEYWORDS
            .iter()
            .any(|keyword| self.at_kw(keyword))
    }

    fn expect_ident(&mut self) -> Option<Token> {
        self.expect(SyntaxKind::Ident, "expected identifier")
    }

    fn expect_kw(&mut self, keyword: &str) -> Option<Token> {
        if self.at_kw(keyword) {
            self.bump()
        } else {
            let span = self.current_span();
            self.error(format!("expected '{keyword}'"), span);
            None
        }
    }

    fn eat_kw(&mut self, keyword: &str) -> Option<Token> {
        if self.at_kw(keyword) {
            self.bump()
        } else {
            None
        }
    }

    fn at_kw(&self, keyword: &str) -> bool {
        self.tokens
            .get(self.position)
            .is_some_and(|token| token.kind == SyntaxKind::Ident && token.text == keyword)
    }

    fn expect(&mut self, kind: SyntaxKind, message: impl Into<String>) -> Option<Token> {
        if self.at(kind) {
            self.bump()
        } else {
            let span = self.current_span();
            self.error(message, span);
            None
        }
    }

    fn eat(&mut self, kind: SyntaxKind) -> Option<Token> {
        if self.at(kind) { self.bump() } else { None }
    }

    fn at(&self, kind: SyntaxKind) -> bool {
        self.current_kind() == Some(kind)
    }

    fn current_kind(&self) -> Option<SyntaxKind> {
        self.tokens.get(self.position).map(|token| token.kind)
    }

    fn peek_kind(&self, offset: usize) -> Option<SyntaxKind> {
        self.tokens
            .get(self.position + offset)
            .map(|token| token.kind)
    }

    fn bump(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.position)?.clone();
        self.position += 1;
        match token.kind {
            SyntaxKind::LParen | SyntaxKind::LBracket | SyntaxKind::LBrace => {
                self.delimiters.push(token.kind);
            }
            SyntaxKind::RParen | SyntaxKind::RBracket | SyntaxKind::RBrace => {
                let opener = match token.kind {
                    SyntaxKind::RParen => SyntaxKind::LParen,
                    SyntaxKind::RBracket => SyntaxKind::LBracket,
                    _ => SyntaxKind::LBrace,
                };
                if let Some(index) = self.delimiters.iter().rposition(|kind| *kind == opener) {
                    self.delimiters.truncate(index);
                }
            }
            _ => {}
        }
        Some(token)
    }

    fn eof(&self) -> bool {
        self.position >= self.tokens.len()
    }

    fn current_span(&self) -> Span {
        self.tokens
            .get(self.position)
            .map(|token| token.span.clone())
            .unwrap_or(self.source.len()..self.source.len())
    }

    fn previous_end(&self) -> usize {
        self.position
            .checked_sub(1)
            .and_then(|index| self.tokens.get(index))
            .map(|token| token.span.end)
            .unwrap_or(0)
    }

    fn at_process_signature(&self) -> bool {
        if !self.at(SyntaxKind::LParen) {
            return false;
        }
        let mut depth = 0_usize;
        for (offset, token) in self.tokens[self.position..].iter().enumerate() {
            match token.kind {
                SyntaxKind::LParen => depth += 1,
                SyntaxKind::RParen => {
                    let Some(next_depth) = depth.checked_sub(1) else {
                        return false;
                    };
                    depth = next_depth;
                    if depth == 0 {
                        return self
                            .tokens
                            .get(self.position + offset + 1)
                            .is_some_and(|next| next.kind == SyntaxKind::FatArrow);
                    }
                }
                _ => {}
            }
        }
        false
    }

    fn error(&mut self, message: impl Into<String>, span: Span) {
        self.errors.push(ParseError {
            message: message.into(),
            span,
        });
    }
}

/// Strip the surrounding quotes from a string token and decode its escapes.
///
/// The lexer keeps the raw source slice, so this is the single place that
/// turns source text into the value the rest of the compiler sees. An unknown
/// escape keeps its following character verbatim rather than failing; the
/// lexer already rejects a string that is not closed.
fn unquote(value: &str) -> String {
    let body = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value);
    if !body.contains('\\') {
        return body.to_owned();
    }
    let mut decoded = String::with_capacity(body.len());
    let mut characters = body.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        match characters.next() {
            Some('n') => decoded.push('\n'),
            Some('r') => decoded.push('\r'),
            Some('t') => decoded.push('\t'),
            Some('0') => decoded.push('\0'),
            Some('\\') => decoded.push('\\'),
            Some('"') => decoded.push('"'),
            Some(other) => decoded.push(other),
            None => decoded.push('\\'),
        }
    }
    decoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_current_process_shape() {
        let source = r#"
            process Cut: DurationAccounting {
                gets sheet: Sheet
                export packages: Stream[Package]
                config thickness: Length
                (width: Length) => {
                    requires width > 0 Meter
                    yield packages = Package(width: width)
                }
            }
        "#;
        let parsed = parse(source);
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
        let Item::Process(process) = &parsed.module.items[0] else {
            panic!("expected process");
        };
        assert_eq!(process.parents, ["DurationAccounting"]);
        assert_eq!(process.members.len(), 3);
        assert!(process.body.is_some());
    }

    #[test]
    fn parses_resources_and_physical_repeats() {
        let parsed = parse(
            r#"
                resource Rope { capacity: 100 Meter }
                process Ascent { uses rope: Rope[10 Meter] }
                process Climb {
                    uses rope: Rope[10 Meter]
                    () => {
                        repeat 3 {
                            let ascent = Ascent()()
                            rope -> ascent.rope
                        }
                    }
                }
            "#,
        );
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
        let Item::Resource(resource) = &parsed.module.items[0] else {
            panic!("expected resource");
        };
        assert_eq!(resource.capacity.display(), "100 Meter");
        let Item::Process(process) = &parsed.module.items[2] else {
            panic!("expected process");
        };
        assert_eq!(process.members[0].ty.to_string(), "Rope[10 Meter]");
        assert!(matches!(
            process.body.as_ref().unwrap().statements[0],
            Stmt::Repeat { .. }
        ));
    }

    #[test]
    fn named_tuple_syntax_round_trips_and_preserves_newline_boundaries() {
        let source = "process P { config settings: (width: Length, nested: (n: Integer))\n () => { let value = (width: 2 Meter, nested: (n: 3),)\n let w = value\n .width } }";
        let parsed = parse(source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let Item::Process(process) = &parsed.module.items[0] else {
            panic!()
        };
        assert_eq!(
            process.members[0].ty.to_string(),
            "(width: Length, nested: (n: Integer))"
        );
        let Stmt::Let { value, .. } = &process.body.as_ref().unwrap().statements[0] else {
            panic!()
        };
        let shown = value.display();
        assert_eq!(parse_expression(&shown).unwrap().display(), shown);
        let formatted = crate::formatter::format_source(source).unwrap();
        assert_eq!(
            crate::formatter::format_source(&formatted).unwrap(),
            formatted
        );
        for invalid in ["(x: 1, x: 2)", "(x: 1, 2)", "(x:)"] {
            assert!(parse_expression(invalid).is_err(), "{invalid}");
        }
        assert!(
            !parse("process P { config x: (a: Scalar, a: Bool) }")
                .errors
                .is_empty()
        );
    }

    #[test]
    fn parses_first_stream_emission_timing() {
        let parsed = parse(
            "process P { export items: Stream[Integer] () => { yield items = 1 after 2 Second } }",
        );
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
        let Item::Process(process) = &parsed.module.items[0] else {
            panic!("expected process");
        };
        let Some(Block { statements, .. }) = &process.body else {
            panic!("expected body");
        };
        let Stmt::Yield { after, .. } = &statements[0] else {
            panic!("expected yield");
        };
        assert_eq!(
            after.as_ref().map(Expr::display).as_deref(),
            Some("2 Second")
        );
    }

    #[test]
    fn parses_functions_units_and_ordering() {
        let source = r#"
            unit Millimeter = Meter / 1000
            fn same[T](items: T[]) -> (result: Bool) where T: Equatable {
                result = true
            }
            process P { () => { first before second } }
        "#;
        let parsed = parse(source);
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
        assert_eq!(parsed.module.items.len(), 3);
    }

    #[test]
    fn parses_configured_process_contract_types() {
        let parsed =
            parse("process Contract {} process Factory { config strategy: Configured[Contract] }");
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
        let Item::Process(factory) = &parsed.module.items[1] else {
            panic!("expected factory process");
        };
        assert_eq!(factory.members[0].ty.to_string(), "Configured[Contract]");
    }

    #[test]
    fn parses_inline_dimensional_types_and_typed_monitors() {
        let parsed = parse(
            "process Result { monitor velocity: Length / Time () => { velocity = Meter / Second } }",
        );
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
        let Item::Process(process) = &parsed.module.items[0] else {
            panic!("expected process");
        };
        assert_eq!(process.members.len(), 1);
        assert_eq!(process.members[0].role, MemberRole::Export);
        assert_eq!(process.members[0].aggregate, Some(AggregateRule::Explicit));
        assert_eq!(process.members[0].ty.to_string(), "(Length / Time)");
        assert_eq!(process.monitors[0].name, "velocity");
        assert_eq!(process.monitors[0].display_unit, None);
    }

    #[test]
    fn parses_process_configuration_defaults() {
        let parsed = parse(
            "process P { config base: Length = 2 Meter expose config doubled: Length = base * 2 }\n",
        );
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
        let Item::Process(process) = &parsed.module.items[0] else {
            panic!("expected process");
        };
        assert_eq!(
            process.members[0].default.as_ref().unwrap().display(),
            "2 Meter"
        );
        assert_eq!(
            process.members[1].default.as_ref().unwrap().display(),
            "(base * 2)"
        );
        assert!(!process.members[0].exposed);
        assert!(process.members[1].exposed);
    }

    #[test]
    fn rejects_unmarked_process_configuration() {
        let parsed = parse("process P { amount: Integer = 2 }\n");
        assert_eq!(parsed.errors.len(), 1, "{:#?}", parsed.errors);
        assert!(parsed.errors[0].message.contains("expected 'config'"));
    }

    #[test]
    fn parses_aggregate_exports_and_monitors() {
        let parsed = parse(
            "process EnergyAccounting { export electricity: Mass aggregate sum }\n\
             process Factory: EnergyAccounting { monitor electricity () => {} }\n",
        );
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
        let Item::Process(contract) = &parsed.module.items[0] else {
            panic!("expected process");
        };
        assert_eq!(contract.members[0].aggregate, Some(AggregateRule::Sum));
        let Item::Process(factory) = &parsed.module.items[1] else {
            panic!("expected process");
        };
        assert_eq!(factory.monitors[0].name, "electricity");
        assert_eq!(factory.monitors[0].display_unit, None);
    }

    #[test]
    fn parses_monitor_display_units() {
        let parsed = parse(
            "process CostAccounting { export cost: USD aggregate sum }\n\
             process Factory: CostAccounting { monitor cost as MillionUSD () => {} }\n",
        );
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
        let Item::Process(factory) = &parsed.module.items[1] else {
            panic!("expected process");
        };
        assert_eq!(factory.monitors[0].name, "cost");
        assert_eq!(
            factory.monitors[0].display_unit.as_deref(),
            Some("MillionUSD")
        );
    }

    #[test]
    fn calculator_and_monitor_share_display_unit_syntax() {
        let parsed = parse_display_expression("rate * 5 Hour as Terabyte").unwrap();
        assert_eq!(parsed.expression.display(), "(rate * 5 Hour)");
        assert_eq!(parsed.display_unit.as_deref(), Some("Terabyte"));

        let monitor = parse("process Result { monitor rate: Information as Terabyte }");
        assert!(monitor.errors.is_empty(), "{:#?}", monitor.errors);
        let Item::Process(process) = &monitor.module.items[0] else {
            panic!("expected process");
        };
        assert_eq!(process.monitors[0].display_unit, parsed.display_unit);

        let squared =
            parse_display_expression("(100 Meter * 10 Millimeter) as Millimeter^2").unwrap();
        assert_eq!(squared.display_unit.as_deref(), Some("Millimeter^2"));

        let compound =
            parse("process Motion { monitor rate: Length / Time as (Mile / Hour) () => {} }");
        assert!(compound.errors.is_empty(), "{:#?}", compound.errors);
        let Item::Process(process) = &compound.module.items[0] else {
            panic!("expected process");
        };
        assert_eq!(
            process.monitors[0].display_unit.as_deref(),
            Some("Mile / Hour")
        );
    }

    #[test]
    fn parses_open_process_invocation_signatures() {
        let parsed =
            parse("process Dispatch { (jobs: Integer, copies_per_job: Integer) }\n");
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
        let Item::Process(process) = &parsed.module.items[0] else {
            panic!("expected process");
        };
        assert_eq!(process.parameters.len(), 2);
        assert!(process.body.is_none());
    }

    #[test]
    fn newline_after_number_is_a_statement_boundary() {
        let source = r#"
            fn values() -> (y: Integer) {
                let x = 1
                y = 2
                requires y > 0
            }
        "#;
        let parsed = parse(source);
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
    }

    fn statement_displays(source: &str) -> Vec<String> {
        let parsed = parse(&format!("process P {{ () => {{ {source} }} }}"));
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
        let Item::Process(process) = &parsed.module.items[0] else {
            panic!("expected process");
        };
        process
            .body
            .as_ref()
            .unwrap()
            .statements
            .iter()
            .map(|statement| match statement {
                Stmt::Expr { value, .. } | Stmt::Let { value, .. } => value.display(),
                _ => panic!("expected expression or binding"),
            })
            .collect()
    }

    #[test]
    fn newline_ends_complete_expressions_before_calls_indexes_and_operators() {
        assert_eq!(
            statement_displays("let cutter = CutSheet()\n(sheet)"),
            ["CutSheet()", "sheet"]
        );
        assert_eq!(
            statement_displays("let items = values\n[1, 2]"),
            ["values", "[1, 2]"]
        );
        assert_eq!(
            statement_displays("let amount = 1\n+ 2\n- 3"),
            ["1", "+2", "-3"]
        );
        assert!(parse_expression("value\n* 2").is_err());
        assert!(parse_display_expression("value\nas Meter").is_err());
    }

    #[test]
    fn explicit_expression_continuations_cross_newlines_and_comments() {
        assert_eq!(
            parse_expression("a + # explanation\n b").unwrap().display(),
            "(a + b)"
        );
        assert_eq!(parse_expression("(a\n + b)").unwrap().display(), "(a + b)");
        assert_eq!(
            parse_expression("f(\n a\n + b,\n [1,\n 2]\n)")
                .unwrap()
                .display(),
            "f((a + b), [1, 2])"
        );
        assert_eq!(
            parse_expression("plan # explanation\n .work_duration")
                .unwrap()
                .display(),
            "plan.work_duration"
        );
        assert_eq!(
            parse_expression("1 Meter^\n 2").unwrap().display(),
            "(1 * (Meter ^ 2))"
        );
        assert!(parse_expression("1 Meter\n ^2").is_err());
        assert_eq!(
            statement_displays("value # end\r\n(other)"),
            ["value", "other"]
        );
        assert_eq!(
            parse_expression("value # trailing comment")
                .unwrap()
                .display(),
            "value"
        );
        assert!(parse_expression("value + # missing operand").is_err());
        assert!(parse_display_expression("value as Meter /").is_err());
        assert!(!parse("unit Broken = Meter /\n").errors.is_empty());
    }

    #[test]
    fn structural_headers_and_else_attach_across_newlines() {
        let parsed = parse(
            r#"
            unit Speed = Length /
                Time
            process Cutter
            {
                config thickness: Length
                config speed: (Length
                    / Time) = 1 Meter / Second
                config amount: Integer = 1
                (width: Length)
                =>
                {
                    if true
                    { requires amount > 0 }
                    # else belongs to the same if
                    else
                    { requires amount < 2 }
                }
            }
        "#,
        );
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
        let Item::Process(process) = &parsed.module.items[1] else {
            panic!("expected process")
        };
        assert_eq!(process.members.len(), 3);
        assert_eq!(process.parameters.len(), 1);
        assert!(matches!(
            &process.body.as_ref().unwrap().statements[0],
            Stmt::If {
                else_branch: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn semicolons_remain_explicit_boundaries_and_delimiters_recover() {
        assert_eq!(
            statement_displays("value; (other); [1]"),
            ["value", "other", "[1]"]
        );
        let parsed = parse(
            "process P { () => { let broken = (\n } }\nunit Fine = Meter\nunit Other = Second",
        );
        assert!(!parsed.errors.is_empty());
        assert!(
            parsed
                .module
                .items
                .iter()
                .any(|item| matches!(item, Item::Unit(unit) if unit.name == "Fine"))
        );
    }

    #[test]
    fn let_mut_is_the_only_mutable_binding_syntax() {
        let parsed = parse(
            r#"
                process P {
                    () => {
                        let mut count: Integer = 0
                        count = 1
                    }
                }
            "#,
        );
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
        let Item::Process(process) = &parsed.module.items[0] else {
            panic!("expected process");
        };
        let body = process.body.as_ref().expect("process body");
        assert!(matches!(
            &body.statements[0],
            Stmt::Let { mutable: true, .. }
        ));

        let deprecated = parse("process P { () => { var count: Integer = 0 } }");
        assert!(
            !deprecated.errors.is_empty(),
            "var must not remain an alias"
        );
    }

    /// A single missing ':' must cost a bounded number of diagnostics, however
    /// much source follows it.
    #[test]
    fn one_syntax_error_does_not_scale_with_the_rest_of_the_file() {
        let head = "process Root {\n    config n Integer = 5\n    export cost: USD aggregate sum\n    () => {\n        cost = 1 USD\n";
        let counts = [0, 50, 400].map(|extra| {
            let body = (0..extra)
                .map(|index| format!("        let a{index} = {index} USD\n"))
                .collect::<String>();
            let trailing = (0..extra / 10)
                .map(|index| {
                    format!("process P{index} {{ export cost: USD aggregate sum () => {{ cost = 1 USD }} }}\n")
                })
                .collect::<String>();
            let source = format!("{head}{body}    }}\n}}\n{trailing}");
            let parsed = parse(&source);
            assert!(
                parsed.errors.iter().any(|error| error
                    .message
                    .contains("expected ':' after process member")),
                "the real error must survive recovery: {:#?}",
                parsed.errors
            );
            parsed.errors.len()
        });

        for count in counts {
            assert!(
                count <= 3,
                "one syntax error produced {count} diagnostics, expected at most 3"
            );
        }
        assert_eq!(
            counts[0], counts[2],
            "diagnostic count must not grow with the size of the rest of the file"
        );
    }

    #[test]
    fn recovers_from_a_malformed_member_and_keeps_parsing_the_process() {
        let parsed = parse(
            "process Root {\n\
             \x20   config n Integer = 5\n\
             \x20   export cost: USD aggregate sum\n\
             \x20   () => { cost = 1 USD }\n\
             }\n",
        );
        let Item::Process(process) = &parsed.module.items[0] else {
            panic!("the process must still be recovered: {:#?}", parsed.errors);
        };
        assert_eq!(process.members.len(), 1, "the good export survives");
        assert_eq!(process.members[0].name, "cost");
        assert!(process.body.is_some(), "the body still parses");
    }

    #[test]
    fn logical_operators_bind_looser_than_comparisons() {
        let parsed = parse_expression("a > 1 and b < 2 or not c == 3").unwrap();
        assert_eq!(
            parsed.display(),
            "(((a > 1) and (b < 2)) or !(c == 3))",
            "or < and < not < comparisons"
        );

        let associativity = parse_expression("a and b and c").unwrap();
        assert_eq!(associativity.display(), "((a and b) and c)");

        // `!` is the same operator as `not`, and prints in the canonical form.
        let bang = parse_expression("!(a == b)").unwrap();
        assert_eq!(
            bang.display(),
            parse_expression("not (a == b)").unwrap().display()
        );
    }

    #[test]
    fn arithmetic_precedence_survives_the_new_logical_tiers() {
        assert_eq!(
            parse_expression("1 + 2 * 3 ^ 2 - -4").unwrap().display(),
            "((1 + (2 * (3 ^ 2))) - -4)"
        );
        assert_eq!(
            parse_expression("a == b + c").unwrap().display(),
            "(a == (b + c))"
        );
    }

    #[test]
    fn a_power_on_a_quantity_suffix_applies_to_the_unit() {
        assert_eq!(
            parse_expression("130 Millimeter^3").unwrap().display(),
            "(130 * (Millimeter ^ 3))"
        );
        assert_eq!(
            parse_expression("2 Meter^2 / Second").unwrap().display(),
            "((2 * (Meter ^ 2)) / Second)"
        );

        // Explicit grouping still raises the complete quantity, including its
        // coefficient, to the power.
        assert_eq!(
            parse_expression("(2 Meter)^2").unwrap().display(),
            "((2 Meter) ^ 2)"
        );
    }

    #[test]
    fn logical_keywords_are_not_numeric_unit_suffixes() {
        let parsed = parse_expression("1 Meter > 0 Meter and 2 Meter < 3 Meter").unwrap();
        assert_eq!(
            parsed.display(),
            "((1 Meter > 0 Meter) and (2 Meter < 3 Meter))"
        );

        // Without `and` in EXPRESSION_FOLLOW_KEYWORDS this reads `0 and`.
        let bare = parse_expression("0 and true").unwrap();
        assert_eq!(bare.display(), "(0 and true)");
    }

    /// `as` and `aggregate` can both follow a bare numeric literal, so neither
    /// may be absorbed as a unit suffix.
    #[test]
    fn keywords_after_a_bare_number_are_not_unit_suffixes() {
        let calculated = parse_display_expression("1000 * 3 as Meter").unwrap();
        assert_eq!(calculated.expression.display(), "(1000 * 3)");
        assert_eq!(calculated.display_unit.as_deref(), Some("Meter"));

        let divided = parse_display_expression("6 Meter / 2 as Meter").unwrap();
        assert_eq!(divided.expression.display(), "(6 Meter / 2)");
        assert_eq!(divided.display_unit.as_deref(), Some("Meter"));

        let parsed = parse("process Root { config n: Integer = 5 aggregate sum }\n");
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
        let Item::Process(process) = &parsed.module.items[0] else {
            panic!("expected process");
        };
        assert_eq!(process.members[0].aggregate, Some(AggregateRule::Sum));
        let Some(Expr {
            kind: ExprKind::Number { value, suffix },
            ..
        }) = &process.members[0].default
        else {
            panic!("expected a numeric default");
        };
        assert_eq!(value, "5");
        assert_eq!(*suffix, None, "'aggregate' must not become a unit suffix");
    }

    /// Aggregate rule names only ever follow the `aggregate` keyword, so they
    /// stay out of the follow set and remain usable as unit spellings.
    #[test]
    fn aggregate_rule_names_remain_usable_as_units() {
        let parsed = parse_expression("5 min").unwrap();
        assert_eq!(parsed.display(), "5 min");
    }

    #[test]
    fn string_literals_hold_decoded_text() {
        let Expr {
            kind: ExprKind::String { value },
            ..
        } = parse_expression(r#""hello\nworld \"quoted\" \\ done""#).unwrap()
        else {
            panic!("expected a string literal");
        };
        assert_eq!(value, "hello\nworld \"quoted\" \\ done");
        assert_eq!(
            Expr {
                kind: ExprKind::String { value },
                span: 0..0
            }
            .display(),
            r#""hello\nworld \"quoted\" \\ done""#,
            "display must round-trip back to source syntax"
        );
    }
}
