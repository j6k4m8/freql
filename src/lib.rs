//! freql compiler library.

pub mod analyzer;
pub mod ast;
pub mod calculator;
pub mod diagnostic;
pub mod formatter;
pub mod ide;
pub mod ide_navigation;
pub mod ide_editing;
pub mod lexer;
pub mod lsp;
mod namespaces;
pub mod parser;
mod presentation;
pub mod render;
pub mod report;
pub mod server;
pub mod source;
pub mod syntax;
pub mod types;

pub use analyzer::{
    Compilation, Compiler, CompilerOptions, ConfigOverride, ConfigValueSource, InspectStage,
};
pub use calculator::{CalculatorError, CalculatorSession};
pub use formatter::{FormatError, format_source};
pub use ide::{
    AnalysisHost, AnalysisSnapshot, CompletionCandidate, CompletionKind, EntryContext, FileId, LineIndex,
    SignatureCandidate,
};
pub use render::{
    GraphControl, GraphDocument, RenderOptions, graph_document, render_dot, render_html,
};
pub use report::{Completeness, Report};
pub use server::{ServeOptions, serve};
