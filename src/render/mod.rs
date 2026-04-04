//! Stable projections of a compilation for graph renderers.

mod dot;
mod graph;
pub mod html;

pub use dot::render_dot;
pub use graph::{
    GraphControl, GraphDocument, GraphEdge, GraphEdgeKind, GraphMember, GraphNode, GraphNodeKind,
    GraphReportScope, RenderError, RenderOptions,
};
pub use html::render_html;

/// Project the semantic model and report into a renderer-neutral graph.
pub fn graph_document(
    compilation: &crate::Compilation,
    options: &RenderOptions,
) -> Result<GraphDocument, RenderError> {
    graph::graph_document(compilation, options)
}
