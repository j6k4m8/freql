use rowan::{GreenNode, GreenNodeBuilder, Language};
use serde::Serialize;

#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum SyntaxKind {
    Root,
    Whitespace,
    Comment,
    DocString,
    String,
    Number,
    Ident,
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Dot,
    Semicolon,
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    Eq,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Arrow,
    FatArrow,
    Bang,
    Error,
}

impl From<SyntaxKind> for rowan::SyntaxKind {
    fn from(kind: SyntaxKind) -> Self {
        rowan::SyntaxKind(kind as u16)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FreqlLanguage {}

impl Language for FreqlLanguage {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> Self::Kind {
        assert!(raw.0 <= SyntaxKind::Error as u16);
        unsafe { std::mem::transmute::<u16, SyntaxKind>(raw.0) }
    }

    fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
        kind.into()
    }
}

pub type SyntaxNode = rowan::SyntaxNode<FreqlLanguage>;

pub fn flat_green(tokens: impl IntoIterator<Item = (SyntaxKind, String)>) -> GreenNode {
    let mut builder = GreenNodeBuilder::new();
    builder.start_node(SyntaxKind::Root.into());
    for (kind, text) in tokens {
        builder.token(kind.into(), &text);
    }
    builder.finish_node();
    builder.finish()
}
