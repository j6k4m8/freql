use std::collections::BTreeMap;
use std::fmt::Write;

use super::{GraphDocument, GraphEdgeKind, GraphNode, GraphNodeKind};

/// Render a graph document as deterministic Graphviz DOT source.
pub fn render_dot(document: &GraphDocument) -> String {
    let mut output = String::from(
        "digraph freql {\n  graph [rankdir=LR, compound=true, fontname=\"Helvetica\", bgcolor=\"transparent\"];\n  node [fontname=\"Helvetica\", fontsize=11];\n  edge [fontname=\"Helvetica\", fontsize=9];\n",
    );
    let nodes = document
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<BTreeMap<_, _>>();
    let mut children: BTreeMap<Option<&str>, Vec<&GraphNode>> = BTreeMap::new();
    for node in &document.nodes {
        children
            .entry(node.parent.as_deref())
            .or_default()
            .push(node);
    }
    for values in children.values_mut() {
        values.sort_by(|left, right| left.id.cmp(&right.id));
    }

    for root in &document.roots {
        if let Some(node) = nodes.get(root.as_str()) {
            emit_tree(&mut output, node, &children, 1);
        }
    }
    // Be defensive if a future graph producer supplies detached nodes.
    if let Some(detached) = children.get(&None) {
        for node in detached {
            if !document.roots.contains(&node.id) {
                emit_tree(&mut output, node, &children, 1);
            }
        }
    }

    for edge in &document.edges {
        let (style, color) = match edge.kind {
            GraphEdgeKind::Physical => ("solid", "#334155"),
            GraphEdgeKind::Before => ("dashed", "#7c3aed"),
        };
        let mut label = edge.label.clone();
        if edge.kind == GraphEdgeKind::Physical {
            if let Some(ty) = &edge.ty {
                label.push('\n');
                label.push_str(ty);
            }
            if let Some(ownership) = &edge.ownership {
                label.push_str(" · ");
                label.push_str(ownership);
            }
        }
        writeln!(
            output,
            "  {} -> {} [id={}, label={}, style={}, color=\"{}\"];",
            quote(&edge.source),
            quote(&edge.target),
            quote(&edge.id),
            quote(&label),
            style,
            color
        )
        .expect("writing to String cannot fail");
    }
    output.push_str("}\n");
    output
}

fn emit_tree(
    output: &mut String,
    node: &GraphNode,
    children: &BTreeMap<Option<&str>, Vec<&GraphNode>>,
    indent: usize,
) {
    let descendants = children.get(&Some(node.id.as_str()));
    if descendants.is_some_and(|nodes| !nodes.is_empty()) {
        line(
            output,
            indent,
            &format!(
                "subgraph {} {{",
                quote(&format!("cluster:{}.children", node.id))
            ),
        );
        line(
            output,
            indent + 1,
            &format!(
                "label={}; color=\"#cbd5e1\"; style=\"rounded\";",
                quote(&format!("{} · {}", node.label, node.process))
            ),
        );
        emit_node(output, node, indent + 1);
        for child in descendants.expect("checked above") {
            emit_tree(output, child, children, indent + 1);
        }
        line(output, indent, "}");
    } else {
        emit_node(output, node, indent);
    }
}

fn emit_node(output: &mut String, node: &GraphNode, indent: usize) {
    let (shape, fill, style) = match node.kind {
        GraphNodeKind::Process => ("component", "#f8fafc", "filled"),
        GraphNodeKind::Run => ("box", "#eef2ff", "rounded,filled"),
        GraphNodeKind::Input => ("oval", "#ecfdf5", "filled"),
        GraphNodeKind::Export => ("oval", "#eff6ff", "filled"),
    };
    let label = match node.kind {
        GraphNodeKind::Run | GraphNodeKind::Process => {
            format!("{}\n{}", node.label, node.process)
        }
        GraphNodeKind::Input => format!("{}\ninput", node.label),
        GraphNodeKind::Export => format!("{}\nexport", node.label),
    };
    line(
        output,
        indent,
        &format!(
            "{} [label={}, shape={}, style=\"{}\", fillcolor=\"{}\", tooltip={}];",
            quote(&node.id),
            quote(&label),
            shape,
            style,
            fill,
            quote(&node.id)
        ),
    );
}

fn line(output: &mut String, indent: usize, value: &str) {
    output.push_str(&"  ".repeat(indent));
    output.push_str(value);
    output.push('\n');
}

fn quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            character if character.is_control() => {
                write!(quoted, "\\u{:04x}", character as u32)
                    .expect("writing to String cannot fail");
            }
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use indexmap::IndexMap;

    use crate::report::Completeness;

    use super::*;
    use crate::render::{GraphEdge, GraphNode};

    #[test]
    fn dot_is_deterministic_and_escapes_values() {
        let document = GraphDocument {
            schema_version: 1,
            entry: "main.req".into(),
            status: Completeness::Complete,
            report_scope: crate::render::GraphReportScope::Compilation,
            selected_process: None,
            roots: vec!["main.req::A\"B".into()],
            nodes: vec![GraphNode {
                id: "main.req::A\"B".into(),
                label: "A\"B".into(),
                kind: GraphNodeKind::Process,
                parent: None,
                process: "A\"B".into(),
                process_path: "main.req::A\"B".into(),
                bodyless: false,
                composite: false,
                config: IndexMap::new(),
                config_sources: IndexMap::new(),
                arguments: IndexMap::new(),
                members: Vec::new(),
            }],
            edges: vec![GraphEdge {
                id: "edge:1".into(),
                source: "main.req::A\"B".into(),
                target: "main.req::A\"B".into(),
                kind: GraphEdgeKind::Before,
                label: "before".into(),
                source_port: None,
                target_port: None,
                ty: None,
                ownership: None,
            }],
            diagnostics: Vec::new(),
            obligations: Vec::new(),
            monitored_metrics: Vec::new(),
            temporal_metrics: Vec::new(),
            stream_handoffs: Vec::new(),
            stream_cardinalities: Vec::new(),
            resource_schedules: Vec::new(),
            provisioned_resources: Vec::new(),
            constraints: Vec::new(),
            controls: Vec::new(),
            overrides: IndexMap::new(),
        };

        let first = render_dot(&document);
        assert_eq!(first, render_dot(&document));
        assert!(first.contains("\\\"B"));
        assert!(first.contains("style=dashed"));
    }
}
