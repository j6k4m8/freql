use thiserror::Error;

use std::collections::BTreeSet;

use crate::ast::{Block, Function, Item, Module, Stmt};
use crate::lexer::{Token, lex};
use crate::parser::parse;
use crate::syntax::SyntaxKind as K;

#[derive(Debug, Error)]
pub enum FormatError {
    #[error("cannot format source with syntax errors: {0}")]
    Parse(String),
    #[error("formatter refused to change the program's syntax tree; source was left unchanged")]
    UnsafeLayout,
}

/// Format with four-space indentation and a 100-column soft target.
/// Statement boundaries come from the parsed AST, not newline heuristics.
/// Original tokens (including string/docstring contents and comments) survive;
/// only whitespace and optional statement semicolons are rewritten. Reparse
/// and compare the AST before returning any edit to the CLI or editor.
pub fn format_source(source: &str) -> Result<String, FormatError> {
    let parsed = parse(source);
    if !parsed.errors.is_empty() {
        return Err(FormatError::Parse(
            parsed
                .errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    let tokens = lex(source).tokens;
    let starts = statement_starts(&parsed.module, &tokens);
    let output = Printer::new(source, &starts).print(&tokens);
    let reparsed = parse(&output);
    if !reparsed.errors.is_empty()
        || fingerprint(&parsed.module) != fingerprint(&reparsed.module)
        || significant_tokens(source) != significant_tokens(&output)
    {
        return Err(FormatError::UnsafeLayout);
    }
    Ok(output)
}

// Fixed array sizes are stored as source text in the AST. Whitespace in those
// expressions is not semantic; string literals inside them still are.
fn fingerprint(module: &Module) -> serde_json::Value {
    fn normalize(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(object) => {
                if object.get("kind").and_then(|v| v.as_str()) == Some("fixed")
                    && let Some(serde_json::Value::String(text)) = object.get_mut("value")
                {
                    *text = lex(text)
                        .tokens
                        .into_iter()
                        .filter(|t| !t.is_trivia())
                        .map(|t| t.text)
                        .collect::<Vec<_>>()
                        .join(" ");
                }
                for child in object.values_mut() {
                    normalize(child);
                }
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    normalize(child);
                }
            }
            _ => {}
        }
    }
    let mut value = serde_json::to_value(module).expect("AST serialization is infallible");
    normalize(&mut value);
    value
}

fn significant_tokens(source: &str) -> Vec<(K, String)> {
    lex(source)
        .tokens
        .into_iter()
        .filter(|t| !matches!(t.kind, K::Whitespace | K::Semicolon))
        .map(|t| (t.kind, t.text))
        .collect()
}

fn statement_starts(module: &Module, tokens: &[Token]) -> BTreeSet<usize> {
    fn function(function: &Function, starts: &mut BTreeSet<usize>) {
        starts.insert(function.span.start);
        if let Some(body) = &function.body {
            mark_block(body, starts);
        }
    }
    let mut starts = BTreeSet::new();
    for item in &module.items {
        starts.insert(item.span().start);
        match item {
            Item::Artifact(a) => {
                starts.extend(a.fields.iter().map(|f| f.span.start));
            }
            Item::Process(p) => {
                starts.extend(p.members.iter().map(|m| m.span.start));
                starts.extend(p.monitors.iter().map(|m| m.span.start));
                if let Some(body) = &p.body {
                    mark_block(body, &mut starts);
                }
                // The invocation signature has no independent span, and may
                // be empty or bodyless. Find its '(' outside member ranges.
                let mut braces = 0;
                for token in tokens.iter().filter(|t| p.span.contains(&t.span.start)) {
                    match token.kind {
                        K::LBrace => braces += 1,
                        K::RBrace => braces -= 1,
                        K::LParen
                            if braces == 1
                                && !p
                                    .members
                                    .iter()
                                    .any(|m| m.span.contains(&token.span.start))
                                && !p
                                    .monitors
                                    .iter()
                                    .any(|m| m.span.contains(&token.span.start)) =>
                        {
                            starts.insert(token.span.start);
                            break;
                        }
                        _ => {}
                    }
                }
            }
            Item::Unit(u) => {
                if let Some(body) = &u.refinement {
                    mark_block(body, &mut starts);
                }
            }
            Item::Function(f) => function(f, &mut starts),
            Item::Trait(t) => {
                for f in &t.functions {
                    function(f, &mut starts);
                }
            }
            Item::Impl(i) => {
                for f in &i.functions {
                    function(f, &mut starts);
                }
            }
            Item::Import(_) | Item::Resource(_) => {}
        }
    }
    starts
}

fn mark_block(body: &Block, starts: &mut BTreeSet<usize>) {
    for stmt in &body.statements {
        starts.insert(stmt.span().start);
        match stmt {
            Stmt::For { body, .. } | Stmt::Repeat { body, .. } => mark_block(body, starts),
            Stmt::If {
                then_branch,
                else_branch,
                ..
            } => {
                mark_block(then_branch, starts);
                if let Some(body) = else_branch {
                    mark_block(body, starts);
                }
            }
            _ => {}
        }
    }
}

struct Printer<'a> {
    source: &'a str,
    starts: &'a BTreeSet<usize>,
    lines: Vec<String>,
    pending: Vec<&'a Token>,
    depth: usize,
    indent: usize,
    continuation: bool,
}

impl<'a> Printer<'a> {
    fn new(source: &'a str, starts: &'a BTreeSet<usize>) -> Self {
        Self {
            source,
            starts,
            lines: Vec::new(),
            pending: Vec::new(),
            depth: 0,
            indent: 0,
            continuation: false,
        }
    }

    fn flush(&mut self) {
        if !self.pending.is_empty() {
            self.lines.extend(wrap(&self.pending, self.indent));
            self.pending.clear();
        }
    }

    fn push(&mut self, token: &'a Token) {
        if self.pending.is_empty() {
            self.indent = self.depth * 4 + usize::from(self.continuation) * 4;
        }
        self.pending.push(token);
    }

    fn print(mut self, tokens: &'a [Token]) -> String {
        let mut previous_end = 0;
        for token in tokens.iter().filter(|t| t.kind != K::Whitespace) {
            let gap = &self.source[previous_end..token.span.start];
            let newlines = gap
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .matches('\n')
                .count();
            let boundary = self.starts.contains(&token.span.start);
            if boundary {
                self.flush();
                self.continuation = false;
            }
            if (boundary || token.kind == K::Comment) && newlines > 1 {
                self.flush();
                if self.lines.last().is_some_and(|l| !l.is_empty()) {
                    self.lines.push(String::new());
                }
            }
            match token.kind {
                K::Semicolon => { /* AST boundaries replace optional semicolons. */ }
                K::Comment => {
                    let inline = newlines == 0 && !self.pending.is_empty();
                    self.flush();
                    if inline {
                        let last = self.lines.last_mut().expect("pending line was flushed");
                        last.push_str("  ");
                        last.push_str(&token.text);
                    } else {
                        self.lines
                            .push(format!("{}{}", " ".repeat(self.depth * 4), token.text));
                    }
                    self.continuation = inline;
                }
                K::DocString => {
                    self.flush();
                    self.lines
                        .push(format!("{}{}", " ".repeat(self.depth * 4), token.text));
                    self.continuation = false;
                }
                K::LBrace => {
                    self.push(token);
                    self.flush();
                    self.depth += 1;
                    self.continuation = false;
                }
                K::RBrace => {
                    self.flush();
                    self.depth = self.depth.saturating_sub(1);
                    self.continuation = false;
                    self.push(token);
                }
                _ => self.push(token),
            }
            previous_end = token.span.end;
        }
        self.flush();
        self.lines.join("\n") + "\n"
    }
}

fn operator(token: &Token) -> bool {
    matches!(
        token.kind,
        K::Plus
            | K::Minus
            | K::Star
            | K::Slash
            | K::Eq
            | K::EqEq
            | K::NotEq
            | K::Lt
            | K::Le
            | K::Gt
            | K::Ge
            | K::Arrow
            | K::FatArrow
    ) || matches!(token.text.as_str(), "and" | "or")
}

fn unary(tokens: &[&Token], index: usize) -> bool {
    let t = tokens[index];
    matches!(t.kind, K::Bang | K::Plus | K::Minus)
        && (index == 0
            || matches!(
                tokens[index - 1].kind,
                K::LParen | K::LBracket | K::Comma | K::Colon | K::Caret
            )
            || operator(tokens[index - 1])
            || matches!(
                tokens[index - 1].text.as_str(),
                "requires" | "not" | "if" | "in"
            ))
}

fn space_before(tokens: &[&Token], index: usize) -> bool {
    if index == 0 {
        return false;
    }
    let a = tokens[index - 1];
    let b = tokens[index];
    if matches!(
        b.kind,
        K::Comma | K::Colon | K::Dot | K::RParen | K::RBracket | K::Caret
    ) || matches!(a.kind, K::Dot | K::LParen | K::LBracket | K::Caret | K::Tilde)
        || unary(tokens, index - 1)
    {
        return false;
    }
    if b.kind == K::LBracket {
        return operator(a)
            || matches!(a.kind, K::Colon | K::Comma)
            || matches!(a.text.as_str(), "in" | "requires");
    }
    if b.kind == K::LParen {
        return operator(a)
            || matches!(a.kind, K::Colon | K::Comma | K::String)
            || matches!(
                a.text.as_str(),
                "if" | "for" | "in" | "repeat" | "requires" | "not" | "as"
            );
    }
    true
}

fn wrap(tokens: &[&Token], indent: usize) -> Vec<String> {
    const WIDTH: usize = 100;
    let mut pieces = Vec::new();
    let mut breaks = BTreeSet::new();
    for (index, token) in tokens.iter().enumerate() {
        pieces.push(format!(
            "{}{}",
            if space_before(tokens, index) { " " } else { "" },
            token.text
        ));
        // Every generated continuation follows an unfinished construct. Never
        // put a call's '(' or an index's '[' on a new line after its receiver.
        if matches!(token.kind, K::Comma | K::Eq | K::LParen | K::LBracket)
            || (operator(token) && !unary(tokens, index))
        {
            breaks.insert(index + 1);
        }
    }
    let mut lines = Vec::new();
    let mut start = 0;
    // Keep a compact right-hand side together instead of stranding its final
    // operand on a continuation line just because the output name is long.
    if indent + pieces.iter().map(|p| p.chars().count()).sum::<usize>() > WIDTH
        && let Some(eq) = tokens.iter().position(|t| t.kind == K::Eq)
        && eq + 1 < pieces.len()
        && indent
            + pieces[..=eq]
                .iter()
                .map(|p| p.chars().count())
                .sum::<usize>()
            <= WIDTH
        && indent + 4 + pieces[eq + 1..].concat().trim_start().chars().count() <= WIDTH
    {
        lines.push(format!("{}{}", " ".repeat(indent), pieces[..=eq].concat()));
        start = eq + 1;
    }
    while start < pieces.len() {
        let current_indent = indent + if start == 0 { 0 } else { 4 };
        let mut length = current_indent;
        let mut last_break = None;
        let mut end = pieces.len();
        for (index, piece) in pieces.iter().enumerate().skip(start) {
            length += if index == start {
                piece.trim_start().chars().count()
            } else {
                piece.chars().count()
            };
            if length > WIDTH
                && let Some(at) = last_break
            {
                end = at;
                break;
            }
            if breaks.contains(&(index + 1)) {
                last_break = Some(index + 1);
            }
        }
        let text = pieces[start..end].concat();
        lines.push(format!(
            "{}{}",
            " ".repeat(current_indent),
            text.trim_start()
        ));
        start = end;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_is_idempotent_and_preserves_comments() {
        let source = "artifact A {  \n    # keep me\n    x: Length\t\n}\n\n\n";
        let once = format_source(source).unwrap();
        let twice = format_source(&once).unwrap();
        assert_eq!(once, twice);
        assert!(once.contains("# keep me"));
        assert!(!once.contains('\t'));
    }

    #[test]
    fn numeric_refinement_formatting_preserves_binder_and_predicates() {
        let source = "unit Positive = Scalar(x) {\n    # strict positivity\n    requires x > 0\n}\nunit Fraction = Positive(x) { requires x <= 1 }\n";
        let formatted = format_source(source).unwrap();
        assert_eq!(format_source(&formatted).unwrap(), formatted);
        let parsed = parse(&formatted);
        assert!(parsed.errors.is_empty());
        let crate::ast::Item::Unit(unit) = &parsed.module.items[0] else {
            panic!("expected refinement");
        };
        assert_eq!(unit.value.display(), "Scalar(x)");
        assert_eq!(unit.refinement.as_ref().unwrap().statements.len(), 1);
    }

    #[test]
    fn preserves_statement_boundaries_and_explicit_continuation() {
        let source = "process P {\r\n    () => {\r\n        let f = Cut()  \r\n        (input)\r\n        let x = (1\r\n            + 2)\r\n        let y = plan # preserve\r\n            .duration\r\n        if true {}\r\n        else {}\r\n    }\r\n}";
        let once = format_source(source).unwrap();
        assert_eq!(once, format_source(&once).unwrap());
        assert!(once.contains("Cut()\n        (input)"));
        assert!(once.contains("plan  # preserve\n            .duration"));
    }

    #[test]
    fn formats_blocks_spacing_and_short_continuations() {
        let source =
            "process P{config x:Scalar=\n 1;monitor result:Scalar\n()=>{result=\n (x+2)*3;}}";
        let expected = "process P {\n    config x: Scalar = 1\n    monitor result: Scalar\n    () => {\n        result = (x + 2) * 3\n    }\n}\n";
        assert_eq!(format_source(source).unwrap(), expected);
        assert_eq!(format_source(expected).unwrap(), expected);
    }

    #[test]
    fn preserves_newline_separated_numbers_calls_and_arrays() {
        let source = "process P { () => { let f = F()\n(x)\n[1,2]\n1\nMeter\nlet g = F()(x)\nlet a = x[0]\n} }";
        let formatted = format_source(source).unwrap();
        assert!(formatted.contains("F()\n        (x)\n        [1, 2]\n        1\n        Meter"));
        assert!(formatted.contains("F()(x)"));
        assert_eq!(format_source(&formatted).unwrap(), formatted);
    }

    #[test]
    fn wraps_long_expressions_without_changing_operator_precedence() {
        let source = format!(
            "process P {{ () => {{ let value = {} }} }}",
            (0..20)
                .map(|i| format!("operand_{i}"))
                .collect::<Vec<_>>()
                .join(" + ")
        );
        let formatted = format_source(&source).unwrap();
        assert!(formatted.lines().all(|line| line.len() <= 100));
        assert_eq!(format_source(&formatted).unwrap(), formatted);
    }

    #[test]
    fn preserves_literal_contents_comments_and_array_size_expressions() {
        let source = "artifact A {\n\"\"\"literal\r\n  indentation  \r\n\"\"\"\nvalues:Scalar[2 + 3]\n}\n# Keep é and punctuation: =  ; {}\nprocess P { () => { let s = \"spacing  matters\"; let x = -2^(-3)\nlet a = [1,-2,+3]\n} }";
        let formatted = format_source(source).unwrap();
        assert!(formatted.contains("\"\"\"literal\r\n  indentation  \r\n\"\"\""));
        assert_eq!(format_source(&formatted).unwrap(), formatted);
    }

    #[test]
    fn grouped_unit_suffixes_preserve_meaning_when_formatted() {
        let source = "process P { config duration:Time=(350/24) Day\nconfig area:Area=(2+1) Meter^2\n}\n";
        let formatted = format_source(source).unwrap();
        assert!(formatted.contains("(350 / 24) Day"), "{formatted}");
        assert_eq!(format_source(&formatted).unwrap(), formatted);
    }

    #[test]
    fn repository_models_preserve_ast_and_are_idempotent() {
        fn visit(path: &std::path::Path, checked: &mut usize) {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(&path, checked);
                } else if path.extension().is_some_and(|e| e == "req") {
                    let source = std::fs::read_to_string(&path).unwrap();
                    assert!(parse(&source).errors.is_empty(), "{} must parse", path.display());
                    let formatted = format_source(&source)
                        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                    assert_eq!(
                        format_source(&formatted).unwrap(),
                        formatted,
                        "{}",
                        path.display()
                    );
                    *checked += 1;
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut checked = 0;
        for directory in ["docs/examples", "stdlib"] {
            visit(&root.join(directory), &mut checked);
        }
        assert!(checked > 0, "no example or standard-library models were checked");
    }
}
