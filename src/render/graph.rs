use std::collections::{BTreeMap, BTreeSet};

use indexmap::IndexMap;
use serde::Serialize;
use thiserror::Error;

use crate::analyzer::{
    Compilation, ConfigValueSource, ConfiguredProcessModel, ProcessModel, RunModel,
};
use crate::ast::MemberRole;
use crate::diagnostic::{Diagnostic, stable_fingerprint};
use crate::report::{
    Completeness, Constraint, MonitoredMetric, Obligation, ProvisionedResource, ResourceSchedule,
    StreamCardinality, StreamHandoff, TemporalMetric,
};

/// Options shared by all graph renderers.
#[derive(Clone, Debug, Default, Serialize)]
pub struct RenderOptions {
    /// A process declaration name or its qualified semantic path.
    pub process: Option<String>,
}

/// A self-contained, renderer-neutral process graph.
#[derive(Clone, Debug, Serialize)]
pub struct GraphDocument {
    pub schema_version: u32,
    pub entry: String,
    pub status: Completeness,
    /// Scope of the report collections below. Graph selection does not filter
    /// compiler findings in schema version 1.
    pub report_scope: GraphReportScope,
    pub selected_process: Option<String>,
    pub roots: Vec<String>,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub diagnostics: Vec<Diagnostic>,
    pub obligations: Vec<Obligation>,
    pub monitored_metrics: Vec<MonitoredMetric>,
    /// Conservative timing status for monitored time makespans.  Consumers
    /// must use this rather than treating a monitored metric's known-work
    /// range as a whole-design completion bound.
    pub temporal_metrics: Vec<TemporalMetric>,
    /// Earliest item-availability events for `Stream[T]` exports.
    pub stream_handoffs: Vec<StreamHandoff>,
    pub stream_cardinalities: Vec<StreamCardinality>,
    pub resource_schedules: Vec<ResourceSchedule>,
    pub provisioned_resources: Vec<ProvisionedResource>,
    pub constraints: Vec<Constraint>,
    /// Exposed configuration values for the selected root processes.
    pub controls: Vec<GraphControl>,
    /// Raw active override text, including temporarily invalid browser edits.
    pub overrides: IndexMap<String, String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GraphControl {
    /// User-facing override path accepted by `--set` and the live API.
    pub path: String,
    pub process_path: String,
    pub name: String,
    pub ty: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    pub overridden: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphReportScope {
    Compilation,
}

#[derive(Clone, Debug, Serialize)]
pub struct GraphNode {
    /// Stable full instance path. Root nodes use the declaration's semantic path.
    pub id: String,
    pub label: String,
    pub kind: GraphNodeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The process declaration's short name.
    pub process: String,
    /// The process declaration's stable qualified semantic path.
    pub process_path: String,
    pub bodyless: bool,
    pub composite: bool,
    #[serde(skip_serializing_if = "IndexMap::is_empty")]
    pub config: IndexMap<String, String>,
    #[serde(skip_serializing_if = "IndexMap::is_empty")]
    pub config_sources: IndexMap<String, ConfigValueSource>,
    #[serde(skip_serializing_if = "IndexMap::is_empty")]
    pub arguments: IndexMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<GraphMember>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GraphMember {
    pub name: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exposed: bool,
    pub ty: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphNodeKind {
    Process,
    Run,
    Input,
    Export,
}

#[derive(Clone, Debug, Serialize)]
pub struct GraphEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    pub kind: GraphEdgeKind,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_port: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_port: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ty: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ownership: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphEdgeKind {
    Physical,
    Before,
}

#[derive(Clone, Debug, Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RenderError {
    #[error(
        "process selector {selector:?} did not match a declaration; available processes: {available}"
    )]
    UnknownProcess { selector: String, available: String },
    #[error("process selector {selector:?} is ambiguous; matches: {matches}")]
    AmbiguousProcess { selector: String, matches: String },
    #[error("run {instance:?} refers to unknown process declaration {process:?}")]
    UnknownRunProcess { instance: String, process: String },
    #[error("recursive process expansion at {instance}: {chain}")]
    RecursiveProcess { instance: String, chain: String },
}

pub(crate) fn graph_document(
    compilation: &Compilation,
    options: &RenderOptions,
) -> Result<GraphDocument, RenderError> {
    let selected = select_roots(compilation, options)?;
    let selected_process = options
        .process
        .as_ref()
        .and_then(|_| selected.first().map(|(_, process)| process.path.clone()));
    let controls = selected
        .iter()
        .flat_map(|(process_name, process)| {
            process.members.iter().filter_map(move |(name, member)| {
                if member.role != MemberRole::Config || !member.exposed {
                    return None;
                }
                let path = format!("{process_name}.{name}");
                let raw_override = compilation.overrides.get(&path);
                Some(GraphControl {
                    path,
                    process_path: process.path.clone(),
                    name: name.clone(),
                    ty: member.ty.to_string(),
                    value: raw_override
                        .cloned()
                        .or_else(|| process.default_config.get(name).cloned())
                        .unwrap_or_default(),
                    resolved_value: process.default_config.get(name).cloned(),
                    default: member.default.clone(),
                    overridden: raw_override.is_some(),
                })
            })
        })
        .collect::<Vec<_>>();
    let mut builder = GraphBuilder {
        compilation,
        nodes: BTreeMap::new(),
        edges: Vec::new(),
    };
    let mut roots = Vec::with_capacity(selected.len());

    for (name, process) in selected {
        let id = process.path.clone();
        roots.push(id.clone());
        builder.add_process_node(&id, None, &name, &name, process, None);
        let root_environment = InstanceEnvironment {
            values: process.default_config.clone(),
            configured: process.default_configured_arguments.clone(),
        };
        builder.expand(&id, &name, process, &root_environment, &mut Vec::new())?;
    }

    builder.edges.sort_by(|left, right| {
        (&left.source, &left.target, edge_rank(left.kind), &left.id).cmp(&(
            &right.source,
            &right.target,
            edge_rank(right.kind),
            &right.id,
        ))
    });

    Ok(GraphDocument {
        schema_version: 1,
        entry: compilation.report.entry.clone(),
        status: compilation.report.status,
        report_scope: GraphReportScope::Compilation,
        selected_process,
        roots,
        nodes: builder.nodes.into_values().collect(),
        edges: builder.edges,
        diagnostics: compilation.report.diagnostics.clone(),
        obligations: compilation.report.obligations.clone(),
        monitored_metrics: compilation.report.monitored_metrics.clone(),
        temporal_metrics: compilation.report.temporal_metrics.clone(),
        stream_handoffs: compilation.report.stream_handoffs.clone(),
        stream_cardinalities: compilation.report.stream_cardinalities.clone(),
        resource_schedules: compilation.report.resource_schedules.clone(),
        provisioned_resources: compilation.report.provisioned_resources.clone(),
        constraints: compilation.report.constraints.clone(),
        controls,
        overrides: compilation.overrides.clone(),
    })
}

fn edge_rank(kind: GraphEdgeKind) -> u8 {
    match kind {
        GraphEdgeKind::Physical => 0,
        GraphEdgeKind::Before => 1,
    }
}

fn select_roots<'a>(
    compilation: &'a Compilation,
    options: &RenderOptions,
) -> Result<Vec<(String, &'a ProcessModel)>, RenderError> {
    let mut processes = compilation
        .model
        .processes
        .iter()
        .map(|(name, process)| (name.clone(), process))
        .collect::<Vec<_>>();
    processes.sort_by(|left, right| left.1.path.cmp(&right.1.path));

    if let Some(selector) = options.process.as_deref() {
        let matches = processes
            .iter()
            .filter(|(name, process)| {
                name == selector
                    || process.path == selector
                    || process
                        .path
                        .rsplit_once("::")
                        .is_some_and(|(_, tail)| tail == selector)
            })
            .cloned()
            .collect::<Vec<_>>();
        return match matches.as_slice() {
            [matched] => Ok(vec![matched.clone()]),
            [] => Err(RenderError::UnknownProcess {
                selector: selector.to_owned(),
                available: processes
                    .iter()
                    .map(|(_, process)| process.path.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            }),
            _ => Err(RenderError::AmbiguousProcess {
                selector: selector.to_owned(),
                matches: matches
                    .iter()
                    .map(|(_, process)| process.path.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            }),
        };
    }

    let entry = compilation.report.entry.as_str();
    let entry_processes = processes
        .into_iter()
        .filter(|(_, process)| semantic_module(&process.path) == entry)
        .collect::<Vec<_>>();
    // A declaration instantiated by another entry-module declaration is not a
    // top-level design root. Imported declarations are never roots by default.
    let mut referenced = BTreeSet::new();
    for (_, process) in &entry_processes {
        collect_configured_process_names(&process.default_configured_arguments, &mut referenced);
        for run in process.runs.values() {
            referenced.insert(run.process.as_str());
            collect_configured_process_names(&run.configured_arguments, &mut referenced);
        }
    }
    let maximal = entry_processes
        .iter()
        .filter(|(name, _)| !referenced.contains(name.as_str()))
        .cloned()
        .collect::<Vec<_>>();

    // A recursive entry graph can leave every declaration referenced. Let the
    // expansion pass produce the more useful recursion diagnostic.
    Ok(if maximal.is_empty() {
        entry_processes
    } else {
        maximal
    })
}

fn semantic_module(path: &str) -> &str {
    path.rsplit_once("::").map_or(path, |(module, _)| module)
}

struct GraphBuilder<'a> {
    compilation: &'a Compilation,
    nodes: BTreeMap<String, GraphNode>,
    edges: Vec<GraphEdge>,
}

#[derive(Clone, Debug, Default)]
struct InstanceEnvironment {
    values: IndexMap<String, String>,
    configured: IndexMap<String, ConfiguredProcessModel>,
}

impl GraphBuilder<'_> {
    fn expand(
        &mut self,
        instance_path: &str,
        process_name: &str,
        process: &ProcessModel,
        environment: &InstanceEnvironment,
        stack: &mut Vec<String>,
    ) -> Result<(), RenderError> {
        if let Some(cycle_start) = stack.iter().position(|name| name == process_name) {
            let mut chain = stack[cycle_start..].to_vec();
            chain.push(process_name.to_owned());
            return Err(RenderError::RecursiveProcess {
                instance: instance_path.to_owned(),
                chain: chain.join(" -> "),
            });
        }
        stack.push(process_name.to_owned());

        let mut runs = process.runs.iter().collect::<Vec<_>>();
        runs.sort_by(|left, right| left.0.cmp(right.0));
        for (run_name, run) in runs {
            let child_path = format!("{instance_path}.{run_name}");
            let instantiated_run = instantiate_run(run, environment);
            let child = self
                .process(&instantiated_run.process)
                .cloned()
                .ok_or_else(|| RenderError::UnknownRunProcess {
                    instance: child_path.clone(),
                    process: instantiated_run.process.clone(),
                })?;
            let child_environment = run_environment(&instantiated_run);
            self.add_process_node(
                &child_path,
                Some(instance_path),
                run_name,
                &instantiated_run.process,
                &child,
                Some(&instantiated_run),
            );
            self.expand(
                &child_path,
                &instantiated_run.process,
                &child,
                &child_environment,
                stack,
            )?;
        }

        for (index, wire) in process.wires.iter().enumerate() {
            let source = self.endpoint(instance_path, process_name, process, &wire.source, true);
            let target = self.endpoint(instance_path, process_name, process, &wire.target, false);
            let label = match (&source.port, &target.port) {
                (Some(source), Some(target)) => format!("{source} → {target}"),
                _ => wire.ty.clone(),
            };
            let fingerprint = stable_fingerprint(&format!(
                "{instance_path}|{}|{}|{index}|physical",
                wire.source, wire.target
            ));
            self.edges.push(GraphEdge {
                id: format!("edge:{fingerprint}"),
                source: source.node,
                target: target.node,
                kind: GraphEdgeKind::Physical,
                label,
                source_port: source.port,
                target_port: target.port,
                ty: Some(wire.ty.clone()),
                ownership: Some(wire.ownership.clone()),
            });
        }

        let mut ordering = process.ordering.clone();
        ordering.sort();
        for (index, (first, second)) in ordering.into_iter().enumerate() {
            let source = format!("{instance_path}.{first}");
            let target = format!("{instance_path}.{second}");
            // Invalid ordering references already have compiler diagnostics. Do
            // not invent graph nodes for them.
            if !self.nodes.contains_key(&source) || !self.nodes.contains_key(&target) {
                continue;
            }
            let fingerprint =
                stable_fingerprint(&format!("{instance_path}|{first}|{second}|{index}|before"));
            self.edges.push(GraphEdge {
                id: format!("edge:{fingerprint}"),
                source,
                target,
                kind: GraphEdgeKind::Before,
                label: "before".into(),
                source_port: None,
                target_port: None,
                ty: None,
                ownership: None,
            });
        }

        stack.pop();
        Ok(())
    }

    fn process(&self, name: &str) -> Option<&ProcessModel> {
        self.compilation.model.processes.get(name).or_else(|| {
            self.compilation
                .model
                .processes
                .values()
                .find(|process| process.path == name)
        })
    }

    fn add_process_node(
        &mut self,
        id: &str,
        parent: Option<&str>,
        label: &str,
        process_name: &str,
        process: &ProcessModel,
        run: Option<&RunModel>,
    ) {
        let members = process
            .members
            .iter()
            .map(|(name, member)| GraphMember {
                name: name.clone(),
                role: member_role(member.role).into(),
                exposed: member.exposed,
                ty: member.ty.to_string(),
            })
            .collect();
        self.nodes.insert(
            id.to_owned(),
            GraphNode {
                id: id.to_owned(),
                label: label.to_owned(),
                kind: if run.is_some() {
                    GraphNodeKind::Run
                } else {
                    GraphNodeKind::Process
                },
                parent: parent.map(str::to_owned),
                process: process_name.to_owned(),
                process_path: process.path.clone(),
                bodyless: process.bodyless,
                composite: process.composite,
                config: run
                    .map_or_else(|| process.default_config.clone(), |run| run.config.clone()),
                config_sources: run.map_or_else(
                    || process.default_config_sources.clone(),
                    |run| run.config_sources.clone(),
                ),
                arguments: run.map_or_else(IndexMap::new, |run| run.arguments.clone()),
                members,
            },
        );
    }

    fn endpoint(
        &mut self,
        instance_path: &str,
        process_name: &str,
        process: &ProcessModel,
        endpoint: &str,
        source: bool,
    ) -> Endpoint {
        if let Some((run, port)) = endpoint.split_once('.')
            && process.runs.contains_key(run)
        {
            return Endpoint {
                node: format!("{instance_path}.{run}"),
                port: Some(port.to_owned()),
            };
        }

        // A composite process node is its own interface boundary. Reusing its
        // named ports connects both root and nested wiring directly to the
        // internal graph instead of inventing disconnected `run.port` nodes.
        if process.members.contains_key(endpoint)
            && self
                .nodes
                .get(instance_path)
                .is_some_and(|node| node.composite)
        {
            return Endpoint {
                node: instance_path.to_owned(),
                port: Some(endpoint.to_owned()),
            };
        }

        let id = format!("{instance_path}.{endpoint}");
        if !self.nodes.contains_key(&id) {
            let member = process.members.get(endpoint);
            let role = member.map(|member| member.role);
            let kind = match role {
                Some(MemberRole::Export) => GraphNodeKind::Export,
                Some(MemberRole::Config | MemberRole::Gets | MemberRole::Uses) => {
                    GraphNodeKind::Input
                }
                None if source => GraphNodeKind::Input,
                None => GraphNodeKind::Export,
            };
            self.nodes.insert(
                id.clone(),
                GraphNode {
                    id: id.clone(),
                    label: endpoint.to_owned(),
                    kind,
                    parent: Some(instance_path.to_owned()),
                    process: process_name.to_owned(),
                    process_path: process.path.clone(),
                    bodyless: false,
                    composite: false,
                    config: IndexMap::new(),
                    config_sources: IndexMap::new(),
                    arguments: IndexMap::new(),
                    members: member
                        .map(|member| {
                            vec![GraphMember {
                                name: endpoint.to_owned(),
                                role: member_role(member.role).into(),
                                exposed: member.exposed,
                                ty: member.ty.to_string(),
                            }]
                        })
                        .unwrap_or_default(),
                },
            );
        }
        Endpoint {
            node: id,
            port: Some(endpoint.to_owned()),
        }
    }
}

fn instantiate_run(run: &RunModel, environment: &InstanceEnvironment) -> RunModel {
    let selected = run
        .process_source
        .as_ref()
        .and_then(|source| environment.configured.get(source));
    let process = selected.map_or_else(|| run.process.clone(), |value| value.process.clone());
    let config = selected.map_or_else(
        || instantiate_values(&run.config, &environment.values),
        |value| instantiate_values(&value.config, &environment.values),
    );
    let configured_arguments = selected.map_or_else(
        || instantiate_configured_values(&run.configured_arguments, environment),
        |value| instantiate_configured_values(&value.configured_arguments, environment),
    );
    RunModel {
        process,
        process_source: None,
        configured_by: run.configured_by.clone(),
        config,
        config_sources: selected.map_or_else(
            || run.config_sources.clone(),
            |value| value.config_sources.clone(),
        ),
        configured_arguments,
        arguments: instantiate_values(&run.arguments, &environment.values),
    }
}

fn instantiate_configured_values(
    values: &IndexMap<String, ConfiguredProcessModel>,
    environment: &InstanceEnvironment,
) -> IndexMap<String, ConfiguredProcessModel> {
    values
        .iter()
        .map(|(name, value)| {
            (name.clone(), {
                let selected = value
                    .process_source
                    .as_ref()
                    .and_then(|source| environment.configured.get(source));
                let concrete = selected.unwrap_or(value);
                ConfiguredProcessModel {
                    process: concrete.process.clone(),
                    process_source: selected
                        .is_none()
                        .then(|| concrete.process_source.clone())
                        .flatten(),
                    config: instantiate_values(&concrete.config, &environment.values),
                    config_sources: concrete.config_sources.clone(),
                    configured_arguments: instantiate_configured_values(
                        &concrete.configured_arguments,
                        environment,
                    ),
                }
            })
        })
        .collect()
}

fn instantiate_values(
    values: &IndexMap<String, String>,
    environment: &IndexMap<String, String>,
) -> IndexMap<String, String> {
    values
        .iter()
        .map(|(name, value)| (name.clone(), substitute_display(value, environment)))
        .collect()
}

fn run_environment(run: &RunModel) -> InstanceEnvironment {
    InstanceEnvironment {
        values: run
            .config
            .iter()
            .chain(&run.arguments)
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
        configured: run.configured_arguments.clone(),
    }
}

fn collect_configured_process_names<'a>(
    values: &'a IndexMap<String, ConfiguredProcessModel>,
    names: &mut BTreeSet<&'a str>,
) {
    for value in values.values() {
        names.insert(value.process.as_str());
        collect_configured_process_names(&value.configured_arguments, names);
    }
}

/// Substitute identifiers in a formatted expression without rewriting string
/// contents, member names, or identifier substrings. Parenthesizing a
/// replacement inside a larger expression preserves its grouping.
fn substitute_display(value: &str, environment: &IndexMap<String, String>) -> String {
    if let Some(replacement) = environment.get(value) {
        return replacement.clone();
    }
    if environment.is_empty() {
        return value.to_owned();
    }

    let lexed = crate::lexer::lex(value);
    if lexed.errors.is_empty() {
        let mut output = String::with_capacity(value.len());
        let mut previous_significant = None;
        for token in lexed.tokens {
            let replacement = if token.kind == crate::syntax::SyntaxKind::Ident
                && previous_significant != Some(crate::syntax::SyntaxKind::Dot)
            {
                environment.get(&token.text)
            } else {
                None
            };
            if let Some(replacement) = replacement {
                output.push('(');
                output.push_str(replacement);
                output.push(')');
            } else {
                output.push_str(&token.text);
            }
            if !token.is_trivia() {
                previous_significant = Some(token.kind);
            }
        }
        output
    } else {
        // Display values normally come from parsed ASTs. If that invariant is
        // ever relaxed, retaining the symbolic spelling is safer than an
        // imprecise textual rewrite.
        value.to_owned()
    }
}

fn member_role(role: MemberRole) -> &'static str {
    match role {
        MemberRole::Config => "config",
        MemberRole::Gets => "gets",
        MemberRole::Uses => "uses",
        MemberRole::Export => "export",
    }
}

struct Endpoint {
    node: String,
    port: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::{Compiler, CompilerOptions, ConfigOverride};

    use super::*;

    fn compile(source: &str) -> Compilation {
        compile_with_options(source, CompilerOptions::default())
    }

    fn compile_with_options(source: &str, options: CompilerOptions) -> Compilation {
        let directory = tempdir().unwrap();
        let entry = directory.path().join("main.req");
        fs::write(&entry, source).unwrap();
        Compiler::new(options).check_path(&entry).unwrap()
    }

    #[test]
    fn exposed_root_configuration_becomes_live_graph_controls() {
        let compilation = compile_with_options(
            r#"
process Factory {
    expose config lanes: Integer = 10
    config internal: Integer = 2
}
"#,
            CompilerOptions {
                overrides: vec![ConfigOverride {
                    path: "Factory.lanes".into(),
                    value: "20".into(),
                }],
                ..CompilerOptions::default()
            },
        );
        let document = graph_document(&compilation, &RenderOptions::default()).unwrap();
        assert_eq!(document.controls.len(), 1);
        assert_eq!(document.controls[0].path, "Factory.lanes");
        assert_eq!(document.controls[0].value, "20");
        assert_eq!(document.controls[0].resolved_value.as_deref(), Some("20"));
        assert!(document.controls[0].overridden);
        assert_eq!(document.overrides["Factory.lanes"], "20");
    }

    #[test]
    fn projects_nested_runs_wires_boundaries_and_ordering() {
        let compilation = compile(
            r#"
artifact Material {}

process Stage {
    gets input: Material
    export output: Material
    () => { output = input }
}

process Factory {
    gets feed: Material
    export product: Material
    () => {
        let first_config = Stage()
        let first = first_config()
        let second_config = Stage()
        let second = second_config()
        feed -> first.input
        first.output -> second.input
        second.output -> product
        first before second
    }

}
"#,
        );
        let document = graph_document(&compilation, &RenderOptions::default()).unwrap();

        assert_eq!(document.roots, ["main.req::Factory"]);
        assert!(document.nodes.iter().any(|node| {
            node.id == "main.req::Factory.first" && node.kind == GraphNodeKind::Run
        }));
        assert!(!document.nodes.iter().any(|node| {
            node.id == "main.req::Factory.feed" || node.id == "main.req::Factory.product"
        }));
        assert_eq!(
            document
                .edges
                .iter()
                .filter(|edge| edge.kind == GraphEdgeKind::Physical)
                .count(),
            3
        );
        assert_eq!(
            document
                .edges
                .iter()
                .filter(|edge| edge.kind == GraphEdgeKind::Before)
                .count(),
            1
        );
        let first = node(&document, "main.req::Factory.first");
        assert!(first.members.iter().any(|member| {
            member.name == "input" && member.role == "gets" && member.ty == "Material"
        }));
        assert!(first.members.iter().any(|member| {
            member.name == "output" && member.role == "export" && member.ty == "Material"
        }));
        assert!(document.edges.iter().any(|edge| {
            edge.source == "main.req::Factory"
                && edge.source_port.as_deref() == Some("feed")
                && edge.target == "main.req::Factory.first"
                && edge.target_port.as_deref() == Some("input")
        }));
        assert!(document.edges.iter().any(|edge| {
            edge.source == "main.req::Factory.first"
                && edge.source_port.as_deref() == Some("output")
                && edge.target == "main.req::Factory.second"
                && edge.target_port.as_deref() == Some("input")
        }));
        assert!(document.edges.iter().any(|edge| {
            edge.source == "main.req::Factory.second"
                && edge.source_port.as_deref() == Some("output")
                && edge.target == "main.req::Factory"
                && edge.target_port.as_deref() == Some("product")
        }));
    }

    #[test]
    fn nested_composite_node_bridges_its_external_and_internal_ports() {
        let compilation = compile(
            r#"
artifact Material {}

process Stage {
    gets input: Material
    export output: Material
    () => { output = input }
}

process Factory {
    gets feed: Material
    export product: Material
    () => {
        let stage_process = Stage()
        let stage = stage_process()
        feed -> stage.input
        stage.output -> product
    }
}

process Entry {
    gets feed: Material
    export product: Material
    () => {
        let factory_process = Factory()
        let batch = factory_process()
        feed -> batch.feed
        batch.product -> product
    }
}
"#,
        );
        let document = graph_document(&compilation, &RenderOptions::default()).unwrap();
        let batch = "main.req::Entry.batch";
        assert!(!document.nodes.iter().any(|node| {
            node.id == "main.req::Entry.batch.feed" || node.id == "main.req::Entry.batch.product"
        }));
        assert!(
            document.edges.iter().any(|edge| {
                edge.target == batch && edge.target_port.as_deref() == Some("feed")
            })
        );
        assert!(document.edges.iter().any(|edge| {
            edge.source == batch
                && edge.source_port.as_deref() == Some("feed")
                && edge.target == "main.req::Entry.batch.stage"
        }));
        assert!(document.edges.iter().any(|edge| {
            edge.source == "main.req::Entry.batch.stage"
                && edge.target == batch
                && edge.target_port.as_deref() == Some("product")
        }));
    }

    #[test]
    fn selector_accepts_short_and_semantic_names() {
        let compilation = compile("process A {}\nprocess B {}\n");
        for selector in ["A", "main.req::A"] {
            let document = graph_document(
                &compilation,
                &RenderOptions {
                    process: Some(selector.into()),
                },
            )
            .unwrap();
            assert_eq!(document.roots, ["main.req::A"]);
        }
    }

    #[test]
    fn reports_unknown_selector_clearly() {
        let compilation = compile("process A {}\n");
        let error = graph_document(
            &compilation,
            &RenderOptions {
                process: Some("Missing".into()),
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("main.req::A"));
    }

    #[test]
    fn empty_entry_still_produces_a_diagnostic_document() {
        let compilation = compile("this is not freql\n");
        let document = graph_document(&compilation, &RenderOptions::default()).unwrap();
        assert!(document.roots.is_empty());
        assert!(document.nodes.is_empty());
        assert!(!document.diagnostics.is_empty());
    }

    #[test]
    fn recursively_instantiates_nested_run_configuration_and_arguments() {
        let compilation = compile(
            r#"
process Stage {
    (pitch: Length) => {}
}

process Step {
    (count: Integer) => {}
}

process Pipeline {
    config pitch: Length
    config count: Integer

    () => {
        let stage_builder = Stage()
        let stage = stage_builder(pitch: pitch)

        let step_builder = Step()
        let step = step_builder(count: count)
    }
}

process PipelineExample {
    () => {
        let factory = Pipeline(
            pitch: 5 Meter,
            count: 2
        )
        let batch = factory()
    }
}
"#,
        );
        let document = graph_document(&compilation, &RenderOptions::default()).unwrap();

        let batch = node(&document, "main.req::PipelineExample.batch");
        assert_eq!(batch.config["pitch"], "5 Meter");
        assert_eq!(batch.config["count"], "2");
        let stage = node(&document, "main.req::PipelineExample.batch.stage");
        assert_eq!(stage.arguments["pitch"], "5 Meter");
        let step = node(&document, "main.req::PipelineExample.batch.step");
        assert_eq!(step.arguments["count"], "2");
        assert_eq!(document.report_scope, GraphReportScope::Compilation);

        let symbolic = graph_document(
            &compilation,
            &RenderOptions {
                process: Some("Pipeline".into()),
            },
        )
        .unwrap();
        assert_eq!(
            node(&symbolic, "main.req::Pipeline.stage").arguments["pitch"],
            "pitch"
        );
    }

    #[test]
    fn resolves_injected_configured_processes_to_concrete_graph_nodes() {
        let compilation = compile(
            r#"
artifact Feed {}
artifact Product {}

process Strategy {
    gets feed: Feed
    export product: Product
    config limit: Integer
}

process ConcreteStrategy: Strategy {
    () => {
        requires limit > 0
        product = Product()
    }
}

process Factory {
    gets feed: Feed
    export product: Product
    config strategy: Configured[Strategy]

    () => {
        let execute = strategy()
        feed -> execute.feed
        execute.product -> product
    }
}

process Example {
    gets feed: Feed
    export product: Product
    config strategy: Configured[Strategy] = ConcreteStrategy(limit: 2)

    () => {
        let factory = Factory(strategy)
        let batch = factory()
        feed -> batch.feed
        batch.product -> product
    }
}
"#,
        );
        let document = graph_document(&compilation, &RenderOptions::default()).unwrap();
        assert_eq!(document.roots, ["main.req::Example"]);
        let root = node(&document, "main.req::Example");
        assert_eq!(
            root.config_sources["strategy"],
            ConfigValueSource::Defaulted
        );
        let batch = node(&document, "main.req::Example.batch");
        assert_eq!(
            batch.config_sources["strategy"],
            ConfigValueSource::Supplied
        );
        let execute = node(&document, "main.req::Example.batch.execute");
        assert_eq!(execute.process, "ConcreteStrategy");
        assert_eq!(execute.config["limit"], "2");
    }

    #[test]
    fn display_substitution_respects_token_and_member_boundaries() {
        let environment = IndexMap::from([
            ("x".into(), "1 + 2".into()),
            ("field".into(), "wrong".into()),
        ]);
        assert_eq!(
            substitute_display("prefix_x + x * item.field + \"x\"", &environment),
            "prefix_x + (1 + 2) * item.field + \"x\""
        );
    }

    fn node<'a>(document: &'a GraphDocument, id: &str) -> &'a GraphNode {
        document
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap_or_else(|| panic!("missing graph node {id}"))
    }
}
