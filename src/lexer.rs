use std::ops::Range;

use logos::{Lexer, Logos};
use serde::Serialize;

use crate::syntax::SyntaxKind;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LexError;

#[derive(Logos, Clone, Copy, Debug, PartialEq, Eq)]
#[logos(error = LexError)]
enum RawKind {
    #[regex(r"[ \t\r\n\f]+")]
    Whitespace,
    #[regex(r"#[^\r\n]*")]
    Comment,
    #[token("\"\"\"", lex_doc_string)]
    DocString,
    #[regex(r#"\"([^\"\\]|\\.)*\""#)]
    String,
    #[regex(r"[0-9]+(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?")]
    Number,
    #[regex(r"[A-Za-z_][A-Za-z0-9_]*")]
    Ident,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token(",")]
    Comma,
    #[token(":")]
    Colon,
    #[token(".")]
    Dot,
    #[token(";")]
    Semicolon,
    #[token("+")]
    Plus,
    #[token("->")]
    Arrow,
    #[token("=>")]
    FatArrow,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("^")]
    Caret,
    #[token("==")]
    EqEq,
    #[token("!=")]
    NotEq,
    #[token("<=")]
    Le,
    #[token(">=")]
    Ge,
    #[token("=")]
    Eq,
    #[token("<")]
    Lt,
    #[token(">")]
    Gt,
    #[token("!")]
    Bang,
}

fn lex_doc_string(lexer: &mut Lexer<'_, RawKind>) -> bool {
    if let Some(end) = lexer.remainder().find("\"\"\"") {
        lexer.bump(end + 3);
        true
    } else {
        lexer.bump(lexer.remainder().len());
        false
    }
}

impl From<RawKind> for SyntaxKind {
    fn from(value: RawKind) -> Self {
        match value {
            RawKind::Whitespace => Self::Whitespace,
            RawKind::Comment => Self::Comment,
            RawKind::DocString => Self::DocString,
            RawKind::String => Self::String,
            RawKind::Number => Self::Number,
            RawKind::Ident => Self::Ident,
            RawKind::LBrace => Self::LBrace,
            RawKind::RBrace => Self::RBrace,
            RawKind::LParen => Self::LParen,
            RawKind::RParen => Self::RParen,
            RawKind::LBracket => Self::LBracket,
            RawKind::RBracket => Self::RBracket,
            RawKind::Comma => Self::Comma,
            RawKind::Colon => Self::Colon,
            RawKind::Dot => Self::Dot,
            RawKind::Semicolon => Self::Semicolon,
            RawKind::Plus => Self::Plus,
            RawKind::Minus => Self::Minus,
            RawKind::Star => Self::Star,
            RawKind::Slash => Self::Slash,
            RawKind::Caret => Self::Caret,
            RawKind::Eq => Self::Eq,
            RawKind::EqEq => Self::EqEq,
            RawKind::NotEq => Self::NotEq,
            RawKind::Lt => Self::Lt,
            RawKind::Le => Self::Le,
            RawKind::Gt => Self::Gt,
            RawKind::Ge => Self::Ge,
            RawKind::Arrow => Self::Arrow,
            RawKind::FatArrow => Self::FatArrow,
            RawKind::Bang => Self::Bang,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Token {
    pub kind: SyntaxKind,
    pub text: String,
    pub span: Range<usize>,
}

impl Token {
    pub fn is_trivia(&self) -> bool {
        matches!(self.kind, SyntaxKind::Whitespace | SyntaxKind::Comment)
    }
}

#[derive(Clone, Debug)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub errors: Vec<Range<usize>>,
}

pub fn lex(source: &str) -> Lexed {
    let mut lexer = RawKind::lexer(source);
    let mut tokens = Vec::new();
    let mut errors = Vec::new();
    while let Some(result) = lexer.next() {
        let span = lexer.span();
        let (kind, is_error) = match result {
            Ok(kind) => (SyntaxKind::from(kind), false),
            Err(_) => (SyntaxKind::Error, true),
        };
        if is_error {
            errors.push(span.clone());
        }
        tokens.push(Token {
            kind,
            text: source[span.clone()].to_owned(),
            span,
        });
    }
    Lexed { tokens, errors }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_docstrings_and_trivia() {
        let source = "artifact A { \"\"\"hello\nworld\"\"\" # c\n x: Length }";
        let lexed = lex(source);
        assert!(lexed.errors.is_empty());
        assert!(
            lexed
                .tokens
                .iter()
                .any(|token| token.kind == SyntaxKind::DocString)
        );
        assert_eq!(
            lexed
                .tokens
                .iter()
                .map(|token| token.text.as_str())
                .collect::<String>(),
            source
        );
    }
}
